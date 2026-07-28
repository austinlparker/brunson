use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::api::ReviewCommentDto;
use crate::diff::render::{DiffLineType, ParsedDiffLine};
use crate::tui::render::component::RenderContext;
use crate::tui::render::layout::fill;
use crate::tui::render::primitives::ScrollViewport;
use crate::tui::render::theme::{
    ADD, ADD_BG, BASE, DEL, DEL_BG, DIFF, HUNK, MANTLE, MUTED, OVERLAY0, SUBTEXT0, TEXT,
};
use crate::tui::views::markdown::markdown_lines;

pub fn render_diff(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    let state = ctx.state;
    let view = ctx.view;

    fill(f, area, MANTLE);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // file stats + line position
            Constraint::Min(1),    // body
        ])
        .split(area);

    let (total_adds, total_dels) = selected_file_stats(state, view);
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("+{}", total_adds),
            Style::default().fg(ADD).bg(BASE),
        ),
        Span::styled(" ", Style::default().bg(BASE)),
        Span::styled(
            format!("−{}", total_dels),
            Style::default().fg(DEL).bg(BASE),
        ),
        Span::styled(
            format!(
                "  line {} / {}",
                // The scroll offset lives in rendered (wrapped) row space,
                // but the indicator should track logical diff lines so it
                // stays put across resizes even though wrapping changes.
                logical_line_for_visual_offset(
                    &state.render_cache.diff_line_starts,
                    view.diff_scroll.offset
                ),
                state.render_cache.diff_line_starts.len().max(1)
            ),
            Style::default().fg(MUTED).bg(BASE),
        ),
    ]))
    .style(Style::default().bg(BASE));
    f.render_widget(header, chunks[0]);

    let body_area = chunks[1];
    // Distinguish "the diff response hasn't arrived yet" (spin) from "it
    // arrived and is empty" (render as empty content, not forever-loading).
    if state.pr_diff.is_none() {
        f.render_widget(
            Paragraph::new(" loading diff…").style(Style::default().fg(MUTED).bg(MANTLE)),
            body_area,
        );
        return;
    }

    // The render cache already flattened diff lines + inline comments at the
    // current width, so the viewport just slices that cached line list.
    let cached = &state.render_cache.diff_lines;
    ScrollViewport::new(cached, view.diff_scroll.offset)
        .style(Style::default().fg(TEXT).bg(MANTLE))
        .scrollbar(true)
        .render(f, body_area);
}

/// Map a visual (post-wrap) row offset back to its 1-based logical diff line
/// number. `line_starts[i]` is the row at which logical line `i` begins, so
/// the logical line containing `visual_offset` is the count of starts at or
/// before it.
fn logical_line_for_visual_offset(line_starts: &[usize], visual_offset: usize) -> usize {
    if line_starts.is_empty() {
        return 1;
    }
    line_starts
        .partition_point(|&start| start <= visual_offset)
        .max(1)
}

fn selected_file_stats(
    state: &crate::tui::app::AppState,
    view: &crate::tui::state::ViewState,
) -> (u64, u64) {
    match state
        .pr_detail
        .as_ref()
        .and_then(|d| d.files.get(view.selected_file_index))
    {
        Some(f) => (f.additions, f.deletions),
        None => (0, 0),
    }
}

pub(crate) fn render_diff_line_internal(
    parsed: &ParsedDiffLine,
    show_line_numbers: bool,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let style = line_style(parsed);

    let marker = match parsed.line_type {
        DiffLineType::Addition => "+",
        DiffLineType::Deletion => "−",
        _ => " ",
    };

    let wrapped = wrap_diff_content(&parsed.content, max_content_width);
    let line_number_prefix = if show_line_numbers {
        let old_ln = parsed
            .old_line
            .map(|n| format!("{:>4}", n))
            .unwrap_or_else(|| "    ".to_string());
        let new_ln = parsed
            .new_line
            .map(|n| format!("{:>4}", n))
            .unwrap_or_else(|| "    ".to_string());
        Some(format!("{} {} │ ", old_ln, new_ln))
    } else {
        None
    };

    wrapped
        .into_iter()
        .enumerate()
        .map(|(i, content)| {
            let mut spans = Vec::new();
            if let Some(ref prefix) = line_number_prefix {
                if i == 0 {
                    spans.push(Span::styled(prefix.clone(), Style::default().fg(OVERLAY0)));
                } else {
                    spans.push(Span::styled(
                        "          │ ".to_string(),
                        Style::default().fg(OVERLAY0),
                    ));
                }
            }
            if i == 0 {
                spans.push(Span::styled(format!("{} {}", marker, content), style));
            } else {
                // Continuation rows must not repeat the +/- sign — that reads
                // as a second added/deleted line. Use a dimmed wrap marker
                // instead, keeping the add/del background on the content span
                // so the wrapped text still reads as part of the same line.
                let marker_style = match style.bg {
                    Some(bg) => Style::default().fg(MUTED).bg(bg),
                    None => Style::default().fg(MUTED),
                };
                spans.push(Span::styled(WRAP_MARKER, marker_style));
                spans.push(Span::styled(content, style));
            }
            Line::from(spans)
        })
        .collect()
}

/// Gutter marker for a wrapped line's continuation rows, replacing the +/-
/// sign so a wrapped deletion/addition doesn't look like two diff lines.
const WRAP_MARKER: &str = "↪ ";

fn line_style(parsed: &ParsedDiffLine) -> Style {
    match parsed.line_type {
        DiffLineType::FileHeader => Style::default().fg(HUNK).add_modifier(Modifier::BOLD),
        DiffLineType::HunkHeader => Style::default().fg(HUNK).add_modifier(Modifier::BOLD),
        DiffLineType::Addition => Style::default().fg(ADD).bg(ADD_BG),
        DiffLineType::Deletion => Style::default().fg(DEL).bg(DEL_BG),
        DiffLineType::Context => Style::default().fg(TEXT).bg(MANTLE),
        DiffLineType::NoNewline => Style::default().fg(MUTED),
    }
}

pub(crate) fn render_inline_comments_internal(
    comments: &[ReviewCommentDto],
    max_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for comment in comments {
        let path_suffix = format!(
            "{}{}",
            comment.path,
            comment
                .line
                .map(|line| format!(":{}", line))
                .unwrap_or_else(|| " outdated".to_string())
        );
        let header_prefix_cols = "  ▌ X @author · ".chars().count();
        let path_part = truncate_to(&path_suffix, max_width.saturating_sub(header_prefix_cols));

        lines.push(Line::from(vec![
            Span::styled("  ▌ ", Style::default().fg(DIFF)),
            Span::styled(
                format!("{} ", initial(&comment.author)),
                Style::default()
                    .fg(BASE)
                    .bg(DIFF)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("@{} · ", comment.author),
                Style::default().fg(SUBTEXT0),
            ),
            Span::styled(path_part, Style::default().fg(MUTED)),
        ]));

        let indent = "    ";
        let indent_cols = indent.chars().count();
        let body_width = max_width.saturating_sub(indent_cols);
        if body_width > 0 {
            for md_line in markdown_lines(&comment.body, body_width)
                .into_iter()
                .take(8)
            {
                let mut spans = vec![Span::styled(indent, Style::default())];
                spans.extend(md_line.spans);
                lines.push(Line::from(spans));
            }
        }
        lines.push(Line::from(""));
    }
    lines
}

fn initial(author: &str) -> String {
    author
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string()
}

/// Truncate a single-line label to at most `max_width` display columns, adding
/// an ellipsis when truncation occurs.
fn truncate_to(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_width {
        return text.to_string();
    }
    let take = max_width.saturating_sub(1).min(chars.len());
    let mut out: String = chars[..take].iter().collect();
    out.push('…');
    out
}

/// Expand tabs and wrap the diff content so long lines are displayed on multiple
/// rows instead of being truncated.
fn wrap_diff_content(content: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![String::new()];
    }
    let expanded = content.replace('\t', "    ");
    if expanded.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for ch in expanded.chars() {
        if ch == '\n' {
            lines.push(current);
            current = String::new();
            current_width = 0;
        } else if current_width >= max_width {
            lines.push(current);
            current = ch.to_string();
            current_width = 1;
        } else {
            current.push(ch);
            current_width += 1;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::render::{DiffLineType, ParsedDiffLine};

    #[test]
    fn test_wrap_diff_content_expands_tabs() {
        let lines = wrap_diff_content("a\tb", 80);
        assert_eq!(lines, vec!["a    b".to_string()]);
    }

    #[test]
    fn test_wrap_diff_content_respects_newlines() {
        let lines = wrap_diff_content("hello\nworld", 80);
        assert_eq!(lines, vec!["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn test_wrap_diff_content_hard_wraps_long_lines() {
        let content = "a".repeat(50);
        let lines = wrap_diff_content(&content, 20);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| l.chars().count() <= 20));
    }

    #[test]
    fn test_render_diff_line_wraps_long_content_into_multiple_lines() {
        let parsed = ParsedDiffLine {
            line_type: DiffLineType::Context,
            content: "a".repeat(200),
            old_line: Some(1),
            new_line: Some(1),
            current_path: String::new(),
        };
        let lines = render_diff_line_internal(&parsed, true, 20);
        assert!(
            lines.len() > 1,
            "long content should wrap into multiple lines, got: {}",
            lines.len()
        );
        for line in &lines {
            let rendered: String = line.spans.iter().map(|s| s.content.clone()).collect();
            assert!(
                rendered.chars().count() <= 34,
                "line overflowed: {}",
                rendered
            );
        }
    }

    #[test]
    fn test_wrapped_deletion_continuation_has_no_repeated_sign() {
        let parsed = ParsedDiffLine {
            line_type: DiffLineType::Deletion,
            content: "a".repeat(50),
            old_line: Some(1),
            new_line: None,
            current_path: String::new(),
        };
        let lines = render_diff_line_internal(&parsed, false, 20);
        assert!(lines.len() > 1, "expected content to wrap");

        // First row keeps the deletion sign.
        let first: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert!(first.starts_with("− "), "first row should carry the sign: {first}");

        // Continuation rows must use the wrap marker instead of repeating
        // the sign, or a wrapped deletion looks like a second deleted line.
        for line in &lines[1..] {
            assert_eq!(line.spans[0].content, WRAP_MARKER);
            let rendered: String = line.spans.iter().map(|s| s.content.clone()).collect();
            assert!(
                !rendered.starts_with('−') && !rendered.starts_with('+'),
                "continuation row repeated the diff sign: {rendered}"
            );
        }
    }

    #[test]
    fn test_wrapped_continuation_keeps_add_del_background() {
        let parsed = ParsedDiffLine {
            line_type: DiffLineType::Addition,
            content: "a".repeat(50),
            old_line: None,
            new_line: Some(1),
            current_path: String::new(),
        };
        let lines = render_diff_line_internal(&parsed, false, 20);
        assert!(lines.len() > 1, "expected content to wrap");

        // The continuation's content span keeps the addition coloring so the
        // wrapped text still reads as part of the same added line, while its
        // marker span is dimmed rather than colored like a sign.
        let marker_span = &lines[1].spans[0];
        assert_eq!(marker_span.style.fg, Some(MUTED));
        let content_span = &lines[1].spans[1];
        assert_eq!(content_span.style.fg, Some(ADD));
        assert_eq!(content_span.style.bg, Some(ADD_BG));
    }

    #[test]
    fn test_logical_line_for_visual_offset_maps_wrapped_rows_to_their_line() {
        // Logical line 0 spans visual rows 0..3 (wrapped 3x), line 1 spans
        // row 3, line 2 spans row 4.
        let line_starts = vec![0, 3, 4];
        assert_eq!(logical_line_for_visual_offset(&line_starts, 0), 1);
        assert_eq!(logical_line_for_visual_offset(&line_starts, 2), 1);
        assert_eq!(logical_line_for_visual_offset(&line_starts, 3), 2);
        assert_eq!(logical_line_for_visual_offset(&line_starts, 4), 3);
        // Past the end, clamp to the last logical line.
        assert_eq!(logical_line_for_visual_offset(&line_starts, 99), 3);
    }

    #[test]
    fn test_logical_line_for_visual_offset_empty_defaults_to_one() {
        assert_eq!(logical_line_for_visual_offset(&[], 0), 1);
    }

    #[test]
    fn test_render_diff_line_respects_zero_content_width() {
        let parsed = ParsedDiffLine {
            line_type: DiffLineType::Context,
            content: "should be hidden".to_string(),
            old_line: Some(1),
            new_line: Some(1),
            current_path: String::new(),
        };
        let lines = render_diff_line_internal(&parsed, true, 0);
        assert_eq!(lines.len(), 1);
        let rendered: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        let content_part = rendered
            .trim_start_matches(|c: char| c.is_ascii_digit() || c.is_whitespace() || c == '│');
        assert!(
            content_part.is_empty() || content_part.chars().all(|c| c == '+' || c.is_whitespace()),
            "content should be clipped when max_content_width is 0"
        );
    }
}
