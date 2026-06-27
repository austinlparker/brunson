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
                view.diff_scroll.offset.saturating_add(1),
                state.diff_lines.len().max(1)
            ),
            Style::default().fg(MUTED).bg(BASE),
        ),
    ]))
    .style(Style::default().bg(BASE));
    f.render_widget(header, chunks[0]);

    let body_area = chunks[1];
    if state.diff_lines.is_empty() {
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
) -> Line<'static> {
    let mut spans = Vec::new();
    let style = line_style(parsed);

    if show_line_numbers {
        let old_ln = parsed
            .old_line
            .map(|n| format!("{:>4}", n))
            .unwrap_or_else(|| "    ".to_string());
        let new_ln = parsed
            .new_line
            .map(|n| format!("{:>4}", n))
            .unwrap_or_else(|| "    ".to_string());
        spans.push(Span::styled(
            format!("{} {} │ ", old_ln, new_ln),
            Style::default().fg(OVERLAY0),
        ));
    }

    let marker = match parsed.line_type {
        DiffLineType::Addition => "+",
        DiffLineType::Deletion => "−",
        _ => " ",
    };

    let content = sanitize_diff_content(&parsed.content, max_content_width);
    spans.push(Span::styled(format!("{} {}", marker, content), style));

    Line::from(spans)
}

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

/// Expand tabs and truncate the diff content so it never exceeds the pane width.
fn sanitize_diff_content(content: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let expanded = content.replace('\t', "    ");
    truncate_to(&expanded, max_width)
}

/// Truncate a line to at most `max_width` display columns, adding an ellipsis
/// when truncation occurs.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::render::{DiffLineType, ParsedDiffLine};

    #[test]
    fn test_truncate_to_short_line_unchanged() {
        assert_eq!(truncate_to("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_to_long_line_adds_ellipsis() {
        assert_eq!(truncate_to("hello world", 8), "hello w…");
    }

    #[test]
    fn test_sanitize_diff_content_expands_tabs_before_truncating() {
        assert_eq!(sanitize_diff_content("a\tb", 6), "a    b");
    }

    #[test]
    fn test_render_diff_line_does_not_exceed_width_with_line_numbers() {
        let parsed = ParsedDiffLine {
            line_type: DiffLineType::Context,
            content: "a".repeat(200),
            old_line: Some(1),
            new_line: Some(1),
            current_path: String::new(),
        };
        let line = render_diff_line_internal(&parsed, true, 20);
        let rendered: String = line.spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            rendered.chars().count() <= 34,
            "line overflowed: {}",
            rendered
        );
        assert!(
            rendered.ends_with('…'),
            "truncated lines should end with an ellipsis"
        );
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
        let line = render_diff_line_internal(&parsed, true, 0);
        let rendered: String = line.spans.iter().map(|s| s.content.clone()).collect();
        let content_part = rendered
            .trim_start_matches(|c: char| c.is_ascii_digit() || c.is_whitespace() || c == '│');
        assert!(
            content_part.is_empty() || content_part.chars().all(|c| c == '+' || c.is_whitespace()),
            "content should be clipped when max_content_width is 0"
        );
    }
}
