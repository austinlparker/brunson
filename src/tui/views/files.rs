use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::render::component::RenderContext;
use crate::tui::render::layout::fill;
use crate::tui::render::primitives::ScrollViewport;
use crate::tui::render::theme::{
    ADD, BASE, DEL, FILES, ICON_DASH, ICON_DIFF_ADDED, ICON_DIFF_MODIFIED, ICON_DIFF_REMOVED,
    ICON_DIFF_RENAMED, ICON_FILES, MANTLE, MUTED, SUBTEXT0, SURFACE0, TEXT,
};

pub fn render_files(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    let state = ctx.state;
    let view = ctx.view;
    let detail = state.pr_detail.as_ref();

    if area.width < 8 || area.height < 3 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    let body = chunks[1];
    fill(f, body, MANTLE);

    let Some(d) = detail.filter(|d| !d.files.is_empty()) else {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                "No files",
                Style::default().fg(MUTED).bg(MANTLE),
            )]))
            .style(Style::default().bg(MANTLE)),
            body,
        );
        return;
    };

    let cols = FileColumnLayout::compute(body.width);

    // Header (non-scrolling): column titles aligned to the data columns, with
    // the summary count pushed to the right edge.
    let header = Paragraph::new(header_line(d, &cols, body.width)).style(Style::default().bg(BASE));
    f.render_widget(header, chunks[0]);

    let mut lines: Vec<Line> = Vec::with_capacity(d.files.len());
    for (i, file) in d.files.iter().enumerate() {
        let selected = i == view.selected_file_index;
        lines.push(file_line(d, file, selected, &cols, body.width));
    }

    ScrollViewport::new(&lines, view.files_scroll.offset)
        .style(Style::default().fg(TEXT).bg(MANTLE))
        .render(f, body);
}

/// Resolved column geometry for one body width.
#[derive(Debug, Clone)]
struct FileColumnLayout {
    /// Width of the path cell.
    path_width: u16,
    /// Width of the comments block, if there is room.
    comments_width: Option<u16>,
}

impl FileColumnLayout {
    /// Lay out columns for the given body width.
    ///
    /// Fixed prefix: `▌ S ` (select bar, status glyph) = 4 cells.
    /// Fixed suffix: gap + adds(5) + gap + dels(5) = 12 cells.
    /// Optional block: comments count, added when width allows.
    /// Path flexes to fill the remainder and is never narrower than 8 cells.
    fn compute(width: u16) -> Self {
        let prefix = 4u16; // select(1) + gap(1) + status(1) + gap(1)
        let add_width = 5u16; // +9999
        let del_width = 5u16; // −9999
        let suffix_fixed = 1 + add_width + 1 + del_width;
        let title_min = 8u16;

        let mut used = prefix.saturating_add(suffix_fixed);
        let room = width.saturating_sub(used.saturating_add(title_min));

        let mut comments_width = None;
        // Reserve gap(1) + content(5) for the comments block when we have room.
        if room >= 6 {
            comments_width = Some(5u16);
            used += 6;
        }

        let path_width = width.saturating_sub(used).max(title_min);
        Self {
            path_width,
            comments_width,
        }
    }
}

fn header_line(
    d: &crate::api::PrDetailResponse,
    cols: &FileColumnLayout,
    width: u16,
) -> Line<'static> {
    let label_style = Style::default().fg(SUBTEXT0).bg(BASE);
    let base_bg = Style::default().bg(BASE);
    let mut spans = vec![
        // Leave the row-prefix cells (select bar + gap + status glyph + gap) empty,
        // then label the status glyph column and path column from their actual
        // positions so they line up with the rows.
        Span::styled("  ", base_bg),   // x0-1: select bar + gap
        pad_span("S", 1, label_style), // x2: status glyph
        Span::styled(" ", base_bg),
        pad_span("PATH", cols.path_width, label_style),
    ];
    if let Some(cw) = cols.comments_width {
        spans.push(Span::styled(" ", base_bg));
        spans.push(pad_span("✎", cw, label_style));
    }
    spans.push(Span::styled(" ", base_bg));
    spans.push(pad_span("+", 5, label_style));
    spans.push(Span::styled(" ", base_bg));
    spans.push(pad_span("−", 5, label_style));

    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let summary = format!("{} FILES  {}", ICON_FILES, d.files.len());
    let summary_w = summary.chars().count();
    if used + summary_w <= width as usize {
        let gap = (width as usize).saturating_sub(used + summary_w);
        spans.push(Span::styled(" ".repeat(gap), base_bg));
        spans.push(Span::styled(
            summary,
            Style::default()
                .fg(FILES)
                .bg(BASE)
                .add_modifier(Modifier::BOLD),
        ));
    }

    Line::from(spans)
}

fn file_line(
    d: &crate::api::PrDetailResponse,
    file: &crate::api::FileDto,
    selected: bool,
    cols: &FileColumnLayout,
    width: u16,
) -> Line<'static> {
    let file_comments: usize = d
        .review_threads
        .iter()
        .flat_map(|t| t.comments.iter())
        .filter(|c| c.path == file.path)
        .count();

    let row_bg = if selected { SURFACE0 } else { MANTLE };
    let base = Style::default()
        .fg(TEXT)
        .bg(row_bg)
        .add_modifier(Modifier::BOLD);

    let status_color = match file.status {
        'A' => ADD,
        'D' => DEL,
        'M' | 'R' => FILES,
        _ => MUTED,
    };
    let status_glyph = match file.status {
        'A' => ICON_DIFF_ADDED,
        'D' => ICON_DIFF_REMOVED,
        'M' => ICON_DIFF_MODIFIED,
        'R' => ICON_DIFF_RENAMED,
        _ => ICON_DASH,
    };

    let mut spans = vec![
        Span::styled(if selected { "▌" } else { " " }, base),
        Span::styled(" ", base),
        Span::styled(
            status_glyph.to_string(),
            Style::default().fg(status_color).bg(row_bg),
        ),
        Span::styled(" ", base),
    ];

    // Path (flex, truncated to the path column width)
    let path = crate::tui::views::text::truncate_to_display_width(&file.path, cols.path_width as usize);
    spans.push(pad_span(
        &path,
        cols.path_width,
        Style::default().fg(TEXT).bg(row_bg),
    ));

    // Comments (optional)
    if let Some(cw) = cols.comments_width {
        spans.push(Span::styled(" ", base));
        let c = if file_comments > 0 {
            format!("✎{}", file_comments)
        } else {
            String::new()
        };
        spans.push(pad_span(&c, cw, Style::default().fg(FILES).bg(row_bg)));
    }

    // Additions / deletions
    spans.push(Span::styled(" ", base));
    spans.push(pad_span(
        &format!("+{}", file.additions),
        5,
        Style::default().fg(ADD).bg(row_bg),
    ));
    spans.push(Span::styled(" ", base));
    spans.push(pad_span(
        &format!("−{}", file.deletions),
        5,
        Style::default().fg(DEL).bg(row_bg),
    ));

    // Trailing pad so the row background fills the full body width.
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if (used as u16) < width {
        spans.push(Span::styled(
            " ".repeat(width as usize - used),
            Style::default().bg(row_bg),
        ));
    }

    Line::from(spans)
}

fn pad_span(text: &str, width: u16, style: Style) -> Span<'static> {
    let w = width as usize;
    let chars: Vec<char> = text.chars().collect();
    let content = if chars.len() >= w {
        chars.iter().take(w).collect()
    } else {
        let mut s: String = chars.iter().collect();
        s.push_str(&" ".repeat(w - chars.len()));
        s
    };
    Span::styled(content, style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_column_layout_flexes_path_and_drops_comments_when_narrow() {
        let wide = FileColumnLayout::compute(120);
        assert!(wide.comments_width.is_some());
        assert!(wide.path_width > 40, "path should flex on wide terminals");

        let narrow = FileColumnLayout::compute(28);
        assert!(narrow.path_width >= 8);
        assert!(narrow.comments_width.is_none());
    }

    #[test]
    fn pad_span_pads_short_text_and_truncates_long_text() {
        assert_eq!(pad_span("ab", 5, Style::default()).content, "ab   ");
        assert_eq!(pad_span("abcdef", 3, Style::default()).content, "abc");
    }
}
