use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Type of a parsed diff line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLineType {
    FileHeader,
    HunkHeader,
    Addition,
    Deletion,
    Context,
    NoNewline,
}

/// A single parsed diff line with metadata.
#[derive(Debug, Clone)]
pub struct ParsedDiffLine {
    pub line_type: DiffLineType,
    pub content: String,
    /// Old file line number (for context/deletion lines)
    pub old_line: Option<usize>,
    /// New file line number (for context/addition lines)
    pub new_line: Option<usize>,
    /// The file path this diff line belongs to.
    pub current_path: String,
}

/// Parse a unified diff into structured lines, tracking which file each line belongs to.
pub fn parse_diff(diff_text: &str) -> Vec<ParsedDiffLine> {
    let mut lines = Vec::new();
    let mut old_line: usize = 0;
    let mut new_line: usize = 0;
    let mut current_path = String::new();

    for raw_line in diff_text.lines() {
        if raw_line.starts_with("diff --git") {
            // diff --git a/<old> b/<new>
            current_path = extract_diff_path(raw_line);
            lines.push(ParsedDiffLine {
                line_type: DiffLineType::FileHeader,
                content: current_path.clone(),
                old_line: None,
                new_line: None,
                current_path: current_path.clone(),
            });
        } else if raw_line.starts_with("--- ") || raw_line.starts_with("+++ ") {
            // Skip the redundant old/new file markers; the diff --git header already
            // identified the file, and line numbers make the rest obvious.
            continue;
        } else if raw_line.starts_with("@@") {
            if let Some((o, n)) = parse_hunk_header(raw_line) {
                old_line = o;
                new_line = n;
            }
            lines.push(ParsedDiffLine {
                line_type: DiffLineType::HunkHeader,
                content: raw_line.to_string(),
                old_line: None,
                new_line: None,
                current_path: current_path.clone(),
            });
        } else if raw_line.starts_with("+++") || raw_line.starts_with("---") {
            // Already handled above, skip duplicates
        } else if raw_line.starts_with('+') {
            lines.push(ParsedDiffLine {
                line_type: DiffLineType::Addition,
                content: raw_line.strip_prefix('+').unwrap().to_string(),
                old_line: None,
                new_line: Some(new_line),
                current_path: current_path.clone(),
            });
            new_line += 1;
        } else if raw_line.starts_with('-') {
            lines.push(ParsedDiffLine {
                line_type: DiffLineType::Deletion,
                content: raw_line.strip_prefix('-').unwrap().to_string(),
                old_line: Some(old_line),
                new_line: None,
                current_path: current_path.clone(),
            });
            old_line += 1;
        } else if raw_line.starts_with("\\ No newline at end of file") {
            lines.push(ParsedDiffLine {
                line_type: DiffLineType::NoNewline,
                content: raw_line.to_string(),
                old_line: None,
                new_line: None,
                current_path: current_path.clone(),
            });
        } else if raw_line.starts_with(' ') {
            lines.push(ParsedDiffLine {
                line_type: DiffLineType::Context,
                content: raw_line.strip_prefix(' ').unwrap().to_string(),
                old_line: Some(old_line),
                new_line: Some(new_line),
                current_path: current_path.clone(),
            });
            old_line += 1;
            new_line += 1;
        } else {
            // Empty line or unknown prefix — treat as context if it has content
            let is_content = !raw_line.is_empty();
            if is_content {
                lines.push(ParsedDiffLine {
                    line_type: DiffLineType::Context,
                    content: raw_line.to_string(),
                    old_line: Some(old_line),
                    new_line: Some(new_line),
                    current_path: current_path.clone(),
                });
                old_line += 1;
                new_line += 1;
            }
        }
    }

    lines
}

/// Extract the new file path from a `diff --git a/... b/...` line.
fn extract_diff_path(header: &str) -> String {
    // Format: diff --git a/<path> b/<path>
    let parts: Vec<&str> = header.split_whitespace().collect();
    if parts.len() >= 4 {
        let candidate = parts[3];
        if let Some(stripped) = candidate.strip_prefix("b/") {
            return stripped.to_string();
        }
        return candidate.to_string();
    }
    String::new()
}

/// Parse a hunk header `@@ -start,count +start,count @@`
/// Returns (old_start, new_start)
fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }

    let old_part = parts.iter().find(|p| p.starts_with('-'))?;
    let new_part = parts.iter().find(|p| p.starts_with('+'))?;

    let old_start: usize = old_part
        .trim_start_matches("-")
        .split(',')
        .next()?
        .parse()
        .ok()?;

    let new_start: usize = new_part
        .trim_start_matches("+")
        .split(',')
        .next()?
        .parse()
        .ok()?;

    Some((old_start, new_start))
}

/// Map review threads to diff line indices where their first comments should appear inline.
///
/// Matching algorithm:
/// 1.  First comment of a thread identifies the file path and new line number.
/// 2.  Find the diff line for that exact file path whose `new_line` equals the comment's line.
/// 3.  If no exact match, attach to the nearest hunk header line in the same file and mark the
///     comment text with `(outdated)`.
pub fn map_review_threads_to_diff_indices(
    threads: &[crate::api::ReviewThreadDto],
    parsed_lines: &[ParsedDiffLine],
) -> std::collections::HashMap<usize, Vec<crate::api::ReviewCommentDto>> {
    let mut map: std::collections::HashMap<usize, Vec<crate::api::ReviewCommentDto>> =
        std::collections::HashMap::new();

    for thread in threads {
        let Some(comment) = thread.comments.first() else {
            continue;
        };
        let Some(target_line) = comment.line else {
            // No line number — append to first hunk header of the file, or index 0.
            let fallback = find_first_hunk_for_file(parsed_lines, &comment.path).unwrap_or(0);
            map.entry(fallback)
                .or_default()
                .push(outdated_comment(comment));
            continue;
        };

        let target_line = target_line as usize;
        let mut matched = None;
        let mut nearest_hunk_idx = None;
        let mut nearest_hunk_distance = usize::MAX;

        for (idx, line) in parsed_lines.iter().enumerate() {
            if line.current_path != comment.path {
                continue;
            }
            if line.line_type == DiffLineType::HunkHeader {
                let distance = if let Some(new) = line.new_line {
                    target_line.abs_diff(new)
                } else {
                    target_line
                };
                if distance < nearest_hunk_distance {
                    nearest_hunk_distance = distance;
                    nearest_hunk_idx = Some(idx);
                }
            }
            if line.new_line == Some(target_line) {
                matched = Some(idx);
                break;
            }
        }

        if let Some(idx) = matched {
            map.entry(idx).or_default().push(comment.clone());
        } else if let Some(idx) = nearest_hunk_idx {
            map.entry(idx).or_default().push(outdated_comment(comment));
        }
    }

    map
}

fn find_first_hunk_for_file(lines: &[ParsedDiffLine], path: &str) -> Option<usize> {
    lines.iter().enumerate().find_map(|(i, line)| {
        if line.current_path == path && line.line_type == DiffLineType::HunkHeader {
            Some(i)
        } else {
            None
        }
    })
}

fn outdated_comment(comment: &crate::api::ReviewCommentDto) -> crate::api::ReviewCommentDto {
    let mut c = comment.clone();
    c.body = format!("[outdated] {}", c.body);
    c
}

/// Render a parsed diff line as a styled ratatui Line.
pub fn render_diff_line(parsed: &ParsedDiffLine, show_line_numbers: bool) -> Line<'static> {
    let mut spans = Vec::new();

    if show_line_numbers {
        let old_ln = parsed
            .old_line
            .map(|n| format!("{:>4} ", n))
            .unwrap_or_else(|| "     ".to_string());
        let new_ln = parsed
            .new_line
            .map(|n| format!("{:>4} ", n))
            .unwrap_or_else(|| "     ".to_string());
        spans.push(Span::styled(
            format!("{}{}", old_ln, new_ln),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let (prefix, style) = match parsed.line_type {
        DiffLineType::FileHeader => (
            "── ",
            Style::default()
                .fg(Color::Rgb(148, 226, 213)) // DIFF teal
                .add_modifier(Modifier::BOLD),
        ),
        DiffLineType::HunkHeader => (
            "",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        DiffLineType::Addition => (
            "+",
            Style::default().fg(Color::Green).bg(Color::Rgb(0, 40, 0)),
        ),
        DiffLineType::Deletion => (
            "−",
            Style::default().fg(Color::Red).bg(Color::Rgb(40, 0, 0)),
        ),
        DiffLineType::Context => (" ", Style::default().fg(Color::Gray)),
        DiffLineType::NoNewline => ("", Style::default().fg(Color::DarkGray)),
    };

    spans.push(Span::styled(format!("{}{}", prefix, parsed.content), style));

    Line::from(spans)
}

/// Find the indices of file boundaries (lines where new files begin).
pub fn find_file_boundaries(lines: &[ParsedDiffLine]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            if l.line_type == DiffLineType::FileHeader {
                Some(i)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ReviewCommentDto, ReviewThreadDto};

    const SAMPLE_DIFF: &str = r#"diff --git a/src/main.rs b/src/main.rs
index abc123..def456 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,5 +10,8 @@ fn main() {
     let x = 1;
-    let y = 2;
+    let y = 3;
+    let z = 4;
+    let w = 5;
     println!("{}", x);
 }
"#;

    #[test]
    fn test_parse_diff_file_header() {
        let lines = parse_diff(SAMPLE_DIFF);
        assert!(lines
            .iter()
            .any(|l| l.line_type == DiffLineType::FileHeader));
        assert_eq!(lines[0].content, "src/main.rs");
    }

    #[test]
    fn test_parse_diff_hunk_header() {
        let lines = parse_diff(SAMPLE_DIFF);
        let hunk = lines
            .iter()
            .find(|l| l.line_type == DiffLineType::HunkHeader);
        assert!(hunk.is_some());
        assert!(hunk.unwrap().content.starts_with("@@"));
    }

    #[test]
    fn test_parse_diff_line_types() {
        let lines = parse_diff(SAMPLE_DIFF);

        assert!(lines
            .iter()
            .any(|l| l.line_type == DiffLineType::Addition && l.content.contains("let z = 4")));
        assert!(lines
            .iter()
            .any(|l| l.line_type == DiffLineType::Deletion && l.content.contains("let y = 2")));
        assert!(lines
            .iter()
            .any(|l| l.line_type == DiffLineType::Context && l.content.contains("println")));
    }

    #[test]
    fn test_parse_diff_line_numbers() {
        let lines = parse_diff(SAMPLE_DIFF);
        let hunk_idx = lines
            .iter()
            .position(|l| l.line_type == DiffLineType::HunkHeader)
            .unwrap();

        let context = &lines[hunk_idx + 1];
        assert_eq!(context.old_line, Some(10));
        assert_eq!(context.new_line, Some(10));

        let deletion = &lines[hunk_idx + 2];
        assert_eq!(deletion.line_type, DiffLineType::Deletion);
        assert_eq!(deletion.old_line, Some(11));

        let addition1 = &lines[hunk_idx + 3];
        assert_eq!(addition1.line_type, DiffLineType::Addition);
        assert_eq!(addition1.new_line, Some(11));
    }

    #[test]
    fn test_parse_hunk_header() {
        let (old, new) = parse_hunk_header("@@ -10,5 +10,8 @@ fn main() {").unwrap();
        assert_eq!(old, 10);
        assert_eq!(new, 10);

        let (old, new) = parse_hunk_header("@@ -1,1 +1,1 @@").unwrap();
        assert_eq!(old, 1);
        assert_eq!(new, 1);
    }

    #[test]
    fn test_find_file_boundaries() {
        let lines = parse_diff(SAMPLE_DIFF);
        let boundaries = find_file_boundaries(&lines);
        assert_eq!(boundaries, vec![0]);
        assert!(lines[0].line_type == DiffLineType::FileHeader);
    }

    #[test]
    fn test_empty_diff() {
        let lines = parse_diff("");
        assert!(lines.is_empty());
    }

    #[test]
    fn test_no_newline_marker() {
        let diff =
            "--- a/file\n+++ b/file\n@@ -1,1 +1,1 @@\n-old\n\\ No newline at end of file\n+new\n";
        let lines = parse_diff(diff);
        assert!(lines.iter().any(|l| l.line_type == DiffLineType::NoNewline));
    }

    #[test]
    fn test_current_path_tracked_per_file() {
        let diff = r#"diff --git a/one.txt b/one.txt
--- a/one.txt
+++ b/one.txt
@@ -1,1 +1,1 @@
-old
+new

diff --git a/two.txt b/two.txt
--- a/two.txt
+++ b/two.txt
@@ -1,1 +1,1 @@
-foo
+bar
"#;
        let lines = parse_diff(diff);
        let one_path: Vec<_> = lines
            .iter()
            .filter(|l| l.current_path == "one.txt")
            .collect();
        let two_path: Vec<_> = lines
            .iter()
            .filter(|l| l.current_path == "two.txt")
            .collect();
        assert_eq!(one_path.len(), 4);
        assert_eq!(two_path.len(), 4);
        assert_ne!(one_path[0].current_path, two_path[0].current_path);
    }

    #[test]
    fn test_renamed_file_path_extracted() {
        let diff =
            "diff --git a/old.rs b/new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1,1 +0,0 @@\n-old\n";
        let lines = parse_diff(diff);
        assert!(lines.iter().all(|l| l.current_path == "new.rs"));
    }

    #[test]
    fn test_map_comments_to_diff_lines_across_files() {
        let diff = r#"diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,3 +1,3 @@
 a
-b
+c
 d

diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1,3 +1,3 @@
 1
-2
+3
 4
"#;
        let lines = parse_diff(diff);
        let threads = vec![
            ReviewThreadDto {
                is_resolved: false,
                is_outdated: false,
                comments: vec![ReviewCommentDto {
                    author: "alice".into(),
                    body: "comment in a".into(),
                    path: "a.rs".into(),
                    line: Some(2),
                    created_at: String::new(),
                    url: String::new(),
                }],
            },
            ReviewThreadDto {
                is_resolved: false,
                is_outdated: false,
                comments: vec![ReviewCommentDto {
                    author: "bob".into(),
                    body: "comment in b".into(),
                    path: "b.rs".into(),
                    line: Some(2),
                    created_at: String::new(),
                    url: String::new(),
                }],
            },
        ];

        let map = map_review_threads_to_diff_indices(&threads, &lines);
        assert_eq!(map.len(), 2);

        let a_idx = map
            .keys()
            .find(|&&idx| lines.get(idx).map(|l| l.current_path.as_str()) == Some("a.rs"));
        let b_idx = map
            .keys()
            .find(|&&idx| lines.get(idx).map(|l| l.current_path.as_str()) == Some("b.rs"));
        assert!(a_idx.is_some());
        assert!(b_idx.is_some());
        assert_ne!(a_idx, b_idx);
    }

    #[test]
    fn test_outdated_comment_attaches_to_hunk_header() {
        let diff = r#"diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,1 +1,1 @@
-line
+modified
"#;
        let lines = parse_diff(diff);
        let threads = vec![ReviewThreadDto {
            is_resolved: false,
            is_outdated: true,
            comments: vec![ReviewCommentDto {
                author: "alice".into(),
                body: "needs fix".into(),
                path: "a.rs".into(),
                line: Some(99),
                created_at: String::new(),
                url: String::new(),
            }],
        }];
        let map = map_review_threads_to_diff_indices(&threads, &lines);
        assert_eq!(map.len(), 1);
        let (&idx, comments) = map.iter().next().unwrap();
        assert_eq!(lines[idx].line_type, DiffLineType::HunkHeader);
        assert!(comments[0].body.starts_with("[outdated]"));
    }
}
