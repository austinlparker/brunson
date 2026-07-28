use std::collections::HashMap;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::api::{PrListResponse, PrSummary};
use crate::github::types::{CheckStatus, Priority};
use crate::tui::render::component::RenderContext;
use crate::tui::render::layout::fill;
use crate::tui::render::primitives::ScrollViewport;
use crate::tui::render::theme::{
    BASE, FAIL, HIGH, INBOX, LOW, MANTLE, MED, MUTED, OVERLAY0, PASS, PENDING, SUBTEXT0, SURFACE0,
    TEXT,
};
use crate::tui::state::InboxRow;

pub fn render_inbox(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    let state = ctx.state;
    let view = ctx.view;

    if area.width < 8 || area.height < 3 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    let body = chunks[1];
    fill(f, body, MANTLE);

    let current_user = state
        .health
        .as_ref()
        .map(|h| h.current_user.as_str())
        .unwrap_or("");

    // `view.inbox_rows` is the single source of truth for grouping/order
    // (built once in `ViewStateManager::prepare`); this lookup only resolves
    // the `PrSummary` behind each row's id, it carries no ordering itself.
    let by_id: HashMap<&str, &PrSummary> = state
        .prs
        .groups
        .values()
        .flatten()
        .map(|pr| (pr.id.as_str(), pr))
        .collect();

    let cols = ColumnLayout::compute(body.width);
    let filter = state.search_filter.as_str();
    let (shown, total) = filter_counts(&state.prs, filter);

    // Header (non-scrolling): column titles aligned to the data columns, plus a
    // right-aligned filter chip when a filter is active.
    let header =
        Paragraph::new(header_line(filter, shown, total, &cols)).style(Style::default().bg(BASE));
    f.render_widget(header, chunks[0]);

    // Build display rows straight from `inbox_rows`: section headers + per-PR
    // columnar rows, in exactly the order `prepare` computed. The cursor
    // (`selected_row`) indexes this same row space and can land on a header.
    let mut lines: Vec<Line> = Vec::with_capacity(view.inbox_rows.len());
    for (idx, row) in view.inbox_rows.iter().enumerate() {
        let is_selected = idx == view.selected_row;
        match row {
            InboxRow::Header {
                section,
                count,
                collapsed,
            } => {
                lines.push(section_line(
                    section.label(),
                    INBOX,
                    *count,
                    *collapsed,
                    is_selected,
                    body.width,
                ));
            }
            InboxRow::Pr { id } => {
                if let Some(&pr) = by_id.get(id.as_str()) {
                    lines.push(pr_row_line(
                        pr,
                        current_user,
                        is_selected,
                        &cols,
                        body.width,
                    ));
                }
            }
        }
    }

    if lines.is_empty() {
        let msg = if filter.is_empty() {
            "  inbox empty — R refresh · w change targets".to_string()
        } else {
            format!("  no PRs match \"{}\" — / to clear · R refresh", filter)
        };
        lines.push(Line::from(vec![Span::styled(
            msg,
            Style::default().fg(MUTED).bg(MANTLE),
        )]));
    }

    ScrollViewport::new(&lines, view.inbox_scroll.offset)
        .style(Style::default().fg(TEXT).bg(MANTLE))
        .render(f, body);
}

/// Count PRs matching `filter` and PRs total, across all groups. Drives the
/// `<shown> of <total>` figure in the filter chip.
fn filter_counts(prs: &PrListResponse, filter: &str) -> (usize, usize) {
    let mut shown = 0;
    let mut total = 0;
    for list in prs.groups.values() {
        for pr in list {
            total += 1;
            if pr.matches_filter(filter) {
                shown += 1;
            }
        }
    }
    (shown, total)
}

/// Resolved column geometry for one body width.
#[derive(Debug, Clone)]
struct ColumnLayout {
    /// Display-column offset of the title cell from the body's left edge.
    title_x: u16,
    /// Width of the title cell.
    title_width: u16,
    author_width: Option<u16>,
    comments_width: Option<u16>,
}

impl ColumnLayout {
    /// Lay out columns for the given body width.
    ///
    /// Fixed prefix: `▌ ● #12345 ` (select bar, priority dot, PR number).
    /// Fixed suffix: check glyph + age. The title flexes to fill the remainder,
    /// but is guaranteed at least 40% of the body width before any optional
    /// column is granted. Optional blocks (author, comments) are added when room
    /// allows and dropped in reverse priority order (comments first, then
    /// author) as the terminal narrows.
    fn compute(width: u16) -> Self {
        // prefix: select(1) + gap(1) + prio(1) + gap(1) + number(6) + gap(1) = 11
        let prefix = 11u16;
        // mandatory suffix: gap(1) + check(1) + gap(1) + age(4) = 7
        let suffix_fixed = 7u16;
        // The title must hold at least 40% of the body (never below a hard floor
        // for very narrow terminals) before optional columns compete for space.
        let title_min = ((width as u32 * 40 / 100) as u16).max(8);

        let mut used = prefix.saturating_add(suffix_fixed);
        let mut room = width.saturating_sub(used.saturating_add(title_min));

        let mut author_width = None;
        let mut comments_width = None;

        // Priority: author > comments. Each block is gap(1) + content.
        if room >= 11 {
            author_width = Some(10);
            used += 11;
            room = room.saturating_sub(11);
        }
        if room >= 5 {
            comments_width = Some(4);
            used += 5;
        }

        let title_width = width.saturating_sub(used).max(title_min);
        Self {
            title_x: prefix,
            title_width,
            author_width,
            comments_width,
        }
    }
}

fn header_line(filter: &str, shown: usize, total: usize, cols: &ColumnLayout) -> Line<'static> {
    let label_style = Style::default().fg(SUBTEXT0).bg(BASE);
    let base_bg = Style::default().bg(BASE);
    let mut spans = vec![];

    // Pad across the title column so labels start directly above the first
    // data column (author, or checks if author is hidden). An active filter's
    // chip lives inside this otherwise-blank region, right-aligned against the
    // first column label — appended after the labels it would land past the
    // row width and be clipped.
    let after_title = (cols.title_x + cols.title_width) as usize;
    let chip =
        (!filter.is_empty()).then(|| format!("/ {} · {} of {} · / clear", filter, shown, total));
    match chip {
        Some(chip) if chip.chars().count() < after_title => {
            let pad = after_title - chip.chars().count() - 1;
            spans.push(Span::styled(" ".repeat(pad), base_bg));
            spans.push(Span::styled(
                chip,
                Style::default()
                    .fg(INBOX)
                    .bg(BASE)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(" ", base_bg));
        }
        _ => {
            if after_title > 0 {
                spans.push(Span::styled(" ".repeat(after_title), base_bg));
            }
        }
    }

    // Labels are added in the same order and spacing as `pr_row_line` so the
    // columns line up.
    if let Some(aw) = cols.author_width {
        spans.push(Span::styled(" ", base_bg));
        spans.push(pad_span("AUTHOR", aw, label_style));
    }
    spans.push(Span::styled(" ", base_bg));
    spans.push(pad_span("✓", 1, label_style));
    if let Some(cw) = cols.comments_width {
        spans.push(Span::styled(" ", base_bg));
        spans.push(pad_span("✎", cw, label_style));
    }
    spans.push(Span::styled(" ", base_bg));
    spans.push(pad_span("AGE", 4, label_style));

    Line::from(spans)
}

/// A section header row. Collapsed sections show a `▸` chevron; expanded ones
/// leave the marker slot blank so labels stay aligned across the fold. The row
/// highlights (like a PR row) when the cursor rests on it.
fn section_line(
    label: &str,
    accent: Color,
    count: usize,
    collapsed: bool,
    selected: bool,
    width: u16,
) -> Line<'static> {
    let bg = if selected { SURFACE0 } else { MANTLE };
    let marker = if collapsed { "▸ " } else { "  " };
    let label_span = format!("{} {}", label, count);
    let used = marker.chars().count() + label_span.chars().count();
    let rule_len = (width as usize).saturating_sub(used + 2).max(1);
    let mut spans = vec![
        Span::styled(marker, Style::default().fg(accent).bg(bg)),
        Span::styled(
            label_span,
            Style::default()
                .fg(accent)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", Style::default().bg(bg)),
        Span::styled("─".repeat(rule_len), Style::default().fg(SURFACE0).bg(bg)),
    ];
    // Trailing fill so a selected header's highlight spans the full width.
    let drawn: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if (drawn as u16) < width {
        spans.push(Span::styled(
            " ".repeat(width as usize - drawn),
            Style::default().bg(bg),
        ));
    }
    Line::from(spans)
}

fn pr_row_line(
    pr: &PrSummary,
    current_user: &str,
    selected: bool,
    cols: &ColumnLayout,
    width: u16,
) -> Line<'static> {
    let row_bg = if selected { SURFACE0 } else { MANTLE };
    let base = Style::default()
        .fg(TEXT)
        .bg(row_bg)
        .add_modifier(Modifier::BOLD);

    let (check_glyph, check_color) = check_glyph(&pr.check_status);

    let mut spans: Vec<Span> = Vec::new();

    // select bar
    spans.push(Span::styled(if selected { "▌" } else { " " }, base));
    spans.push(Span::styled(" ", base));
    // priority dot
    spans.push(Span::styled(
        "●",
        Style::default()
            .fg(priority_dot_color(pr.llm_priority.as_ref()))
            .bg(row_bg),
    ));
    spans.push(Span::styled(" ", base));
    // number (fixed 6)
    spans.push(pad_span(
        &format!("#{}", pr.number),
        6,
        Style::default().fg(SUBTEXT0).bg(row_bg),
    ));
    spans.push(Span::styled(" ", base));

    // title (flex, truncated to the title column width)
    let title =
        crate::tui::views::text::truncate_to_display_width(&pr.title, cols.title_width as usize);
    spans.push(pad_span(
        &title,
        cols.title_width,
        Style::default().fg(TEXT).bg(row_bg),
    ));

    // author (optional)
    if let Some(aw) = cols.author_width {
        spans.push(Span::styled(" ", base));
        let author = if pr.author == current_user {
            String::new()
        } else {
            format!("@{}", pr.author)
        };
        spans.push(pad_span(
            &author,
            aw,
            Style::default().fg(OVERLAY0).bg(row_bg),
        ));
    }

    // checks (mandatory)
    spans.push(Span::styled(" ", base));
    spans.push(Span::styled(
        check_glyph.to_string(),
        Style::default().fg(check_color).bg(row_bg),
    ));

    // comments (optional) — muted so it doesn't read as an interactive accent.
    if let Some(cw) = cols.comments_width {
        spans.push(Span::styled(" ", base));
        let c = if pr.comments > 0 {
            format!("✎{}", pr.comments)
        } else {
            String::new()
        };
        spans.push(pad_span(&c, cw, Style::default().fg(MUTED).bg(row_bg)));
    }

    // age (mandatory)
    spans.push(Span::styled(" ", base));
    spans.push(pad_span(
        &age(&pr.updated_at),
        4,
        Style::default().fg(OVERLAY0).bg(row_bg),
    ));

    // Trailing pad so the row background fills the full body width (this is what
    // makes the selection highlight span the entire row).
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

fn priority_dot_color(p: Option<&Priority>) -> Color {
    match p {
        Some(Priority::High) => HIGH,
        Some(Priority::Medium) => MED,
        Some(Priority::Low) => LOW,
        None => OVERLAY0,
    }
}

fn check_glyph(status: &CheckStatus) -> (&'static str, Color) {
    match status {
        CheckStatus::Success => ("✓", PASS),
        CheckStatus::Failure => ("✕", FAIL),
        CheckStatus::Pending => ("◌", PENDING),
        CheckStatus::Neutral | CheckStatus::None => ("·", MUTED),
    }
}

fn age(updated_at: &str) -> String {
    use chrono::DateTime;
    let updated = DateTime::parse_from_rfc3339(updated_at).ok();
    let now = chrono::Local::now();
    match updated {
        Some(dt) => {
            let secs = (now.with_timezone(&chrono::Utc) - dt.with_timezone(&chrono::Utc))
                .num_seconds()
                .max(0);
            if secs < 60 {
                format!("{}s", secs)
            } else if secs < 3600 {
                format!("{}m", secs / 60)
            } else if secs < 86400 {
                format!("{}h", secs / 3600)
            } else {
                format!("{}d", secs / 86400)
            }
        }
        None => "?".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_layout_reserves_40pct_for_title_then_grants_optional_blocks() {
        // Wide: both optional blocks present, title flexes past its 40% floor.
        let wide = ColumnLayout::compute(160);
        assert!(wide.author_width.is_some());
        assert!(wide.comments_width.is_some());
        assert!(wide.title_width > 80, "title should flex on wide terminals");
        assert!(
            wide.title_width >= (160 * 40 / 100),
            "title keeps at least 40% of body width"
        );

        // Mid: comments drop first, author survives.
        let mid = ColumnLayout::compute(48);
        assert!(mid.author_width.is_some(), "author survives at 48 cols");
        assert!(mid.comments_width.is_none(), "comments drop first");
        assert!(mid.title_width >= (48 * 40 / 100));
        assert_eq!(mid.title_x, 11);

        // Narrow: both optional blocks dropped, title clamps to its floor.
        let narrow = ColumnLayout::compute(30);
        assert!(narrow.author_width.is_none());
        assert!(narrow.comments_width.is_none());
        assert!(narrow.title_width >= 8);
    }

    #[test]
    fn pad_span_pads_short_text_and_truncates_long_text() {
        assert_eq!(pad_span("ab", 5, Style::default()).content, "ab   ");
        assert_eq!(pad_span("abcdef", 3, Style::default()).content, "abc");
    }
}
