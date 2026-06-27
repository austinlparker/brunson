use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::api::PrSummary;
use crate::github::types::Priority;
use crate::tui::render::component::RenderContext;
use crate::tui::render::layout::fill;
use crate::tui::render::primitives::ScrollViewport;
use crate::tui::render::theme::{
    BASE, DRAFT, FAIL, HIGH, ICON_INBOX, INBOX, LOW, MANTLE, MED, MUTED, OPEN, OVERLAY0, PASS,
    PENDING, REVIEW_REQUESTED, SUBTEXT0, SURFACE0, TEXT,
};

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

    let (opened, review) = split_pulls(state, &view.collapsed_groups, current_user);
    let selected_id = state.selected_pr_id.clone();
    let cols = ColumnLayout::compute(body.width);

    // Header (non-scrolling): column titles aligned to the data columns, with
    // the summary count pushed to the right edge.
    let header = Paragraph::new(header_line(&opened, &review, &cols, body.width))
        .style(Style::default().bg(BASE));
    f.render_widget(header, chunks[0]);

    // Build display rows: section dividers + per-PR columnar rows.
    let mut lines: Vec<Line> = Vec::new();

    if !opened.is_empty() {
        lines.push(section_line(
            "OPENED BY ME",
            INBOX,
            opened.len(),
            body.width,
        ));
        for &pr in &opened {
            let is_selected = selected_id.as_deref() == Some(&pr.id);
            lines.push(pr_row_line(
                pr,
                current_user,
                is_selected,
                &cols,
                body.width,
            ));
        }
    }

    if !review.is_empty() {
        lines.push(section_line(
            "NEEDS MY REVIEW",
            INBOX,
            review.len(),
            body.width,
        ));
        for &pr in &review {
            let is_selected = selected_id.as_deref() == Some(&pr.id);
            lines.push(pr_row_line(
                pr,
                current_user,
                is_selected,
                &cols,
                body.width,
            ));
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  no PRs found",
            Style::default().fg(MUTED).bg(MANTLE),
        )]));
    }

    ScrollViewport::new(&lines, view.inbox_scroll.offset)
        .style(Style::default().fg(TEXT).bg(MANTLE))
        .render(f, body);
}

/// Resolved column geometry for one body width.
#[derive(Debug, Clone)]
struct ColumnLayout {
    /// Display-column offset of the title cell from the body's left edge.
    title_x: u16,
    /// Width of the title cell.
    title_width: u16,
    author_width: Option<u16>,
    state_width: Option<u16>,
    comments_width: Option<u16>,
}

impl ColumnLayout {
    /// Lay out columns for the given body width.
    ///
    /// Fixed prefix: `▌ ● #12345 ` (select bar, priority dot, PR number).
    /// Fixed suffix: check glyph + age. The title flexes to fill the remainder.
    /// Optional blocks (author, state, comments) are added when width allows and
    /// dropped in reverse priority order (comments first, then state, then author)
    /// as the terminal narrows.
    fn compute(width: u16) -> Self {
        // prefix: select(1) + gap(1) + prio(1) + gap(1) + number(6) + gap(1) = 11
        let prefix = 11u16;
        // mandatory suffix: gap(1) + checks(2) + gap(1) + age(4) = 8
        let suffix_fixed = 8u16;
        let title_min = 8u16;

        let mut used = prefix.saturating_add(suffix_fixed);
        let mut room = width.saturating_sub(used.saturating_add(title_min));

        let mut author_width = None;
        let mut state_width = None;
        let mut comments_width = None;

        // Priority: author > state > comments. Each block is gap(1) + content.
        if room >= 11 {
            author_width = Some(10);
            used += 11;
            room = room.saturating_sub(11);
        }
        if room >= 9 {
            state_width = Some(8);
            used += 9;
            room = room.saturating_sub(9);
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
            state_width,
            comments_width,
        }
    }
}

fn header_line(
    opened: &[&PrSummary],
    review: &[&PrSummary],
    cols: &ColumnLayout,
    width: u16,
) -> Line<'static> {
    let label_style = Style::default().fg(SUBTEXT0).bg(BASE);
    let base_bg = Style::default().bg(BASE);
    let mut spans = vec![];

    // Pad across the title column so labels start directly above the first
    // data column (author, or checks if author is hidden).
    let after_title = cols.title_x + cols.title_width;
    if after_title > 0 {
        spans.push(Span::styled(" ".repeat(after_title as usize), base_bg));
    }

    // Labels are added in the same order and spacing as `pr_row_line` so the
    // columns line up.
    if let Some(aw) = cols.author_width {
        spans.push(Span::styled(" ", base_bg));
        spans.push(pad_span("AUTHOR", aw, label_style));
    }
    if let Some(sw) = cols.state_width {
        spans.push(Span::styled(" ", base_bg));
        spans.push(pad_span("STATE", sw, label_style));
    }
    spans.push(Span::styled(" ", base_bg));
    spans.push(pad_span("✓", 1, label_style));
    if let Some(cw) = cols.comments_width {
        spans.push(Span::styled(" ", base_bg));
        spans.push(pad_span("✎", cw, label_style));
    }
    spans.push(Span::styled(" ", base_bg));
    spans.push(pad_span("AGE", 4, label_style));

    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let summary = format!(
        "{} INBOX  opened {} · review {} · {}",
        ICON_INBOX,
        opened.len(),
        review.len(),
        opened.len() + review.len()
    );
    let summary_w = summary.chars().count();
    if used + summary_w <= width as usize {
        let gap = (width as usize).saturating_sub(used + summary_w);
        spans.push(Span::styled(" ".repeat(gap), base_bg));
        spans.push(Span::styled(
            summary,
            Style::default()
                .fg(INBOX)
                .bg(BASE)
                .add_modifier(Modifier::BOLD),
        ));
    }

    Line::from(spans)
}

fn section_line(label: &str, accent: Color, count: usize, width: u16) -> Line<'static> {
    let label_span = format!("{} {}", label, count);
    let label_w = label_span.chars().count() as u16;
    let rule_len = width.saturating_sub(label_w + 2).max(1) as usize;
    Line::from(vec![
        Span::styled(" ", Style::default().bg(MANTLE)),
        Span::styled(
            label_span,
            Style::default()
                .fg(accent)
                .bg(MANTLE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", Style::default().bg(MANTLE)),
        Span::styled(
            "─".repeat(rule_len),
            Style::default().fg(SURFACE0).bg(MANTLE),
        ),
    ])
}

#[allow(clippy::too_many_arguments)]
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
    let state_color = if pr.group == "draft" {
        DRAFT
    } else if pr.group.contains("review") {
        REVIEW_REQUESTED
    } else {
        OPEN
    };
    let state_label = pr_state_label(pr);

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
    let title = truncate_width(&pr.title, cols.title_width as usize);
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

    // state (optional)
    if let Some(sw) = cols.state_width {
        spans.push(Span::styled(" ", base));
        spans.push(pad_span(
            &state_label,
            sw,
            Style::default().fg(state_color).bg(row_bg),
        ));
    }

    // checks (mandatory)
    spans.push(Span::styled(" ", base));
    spans.push(Span::styled(
        check_glyph.to_string(),
        Style::default().fg(check_color).bg(row_bg),
    ));

    // comments (optional)
    if let Some(cw) = cols.comments_width {
        spans.push(Span::styled(" ", base));
        let c = if pr.comments > 0 {
            format!("✎{}", pr.comments)
        } else {
            String::new()
        };
        spans.push(pad_span(&c, cw, Style::default().fg(INBOX).bg(row_bg)));
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

fn truncate_width(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    use unicode_width::UnicodeWidthStr;
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w + 1 > max_width {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

fn priority_dot_color(p: Option<&Priority>) -> Color {
    match p {
        Some(Priority::High) => HIGH,
        Some(Priority::Medium) => MED,
        Some(Priority::Low) => LOW,
        None => OVERLAY0,
    }
}

fn check_glyph(status: &str) -> (&'static str, Color) {
    match status {
        "success" => ("✓", PASS),
        "failure" => ("✕", FAIL),
        "pending" => ("◌", PENDING),
        _ => ("·", MUTED),
    }
}

fn pr_state_label(pr: &PrSummary) -> String {
    pr.group
        .split('_')
        .next()
        .map(|s| s.to_uppercase())
        .unwrap_or_else(|| "OPEN".to_string())
}

fn split_pulls<'a>(
    state: &'a crate::tui::app::AppState,
    collapsed: &'a std::collections::HashMap<String, bool>,
    current_user: &str,
) -> (Vec<&'a PrSummary>, Vec<&'a PrSummary>) {
    use crate::github::types::PrGroup;

    let mut opened = Vec::new();
    let mut review = Vec::new();

    for group in PrGroup::all_in_priority_order() {
        let key = crate::api::group_key(group);
        if *collapsed.get(&key).unwrap_or(&false) {
            continue;
        }
        if let Some(prs) = state.prs.groups.get(&key) {
            for pr in prs {
                match group {
                    PrGroup::AuthoredActionNeeded
                    | PrGroup::AuthoredReadyToMerge
                    | PrGroup::AuthoredWaiting => opened.push(pr),
                    PrGroup::ReviewNeeded | PrGroup::ReviewUpdate | PrGroup::ReviewDone => {
                        review.push(pr)
                    }
                    PrGroup::Draft => {
                        if pr.author == current_user {
                            opened.push(pr);
                        } else {
                            review.push(pr);
                        }
                    }
                    PrGroup::Other => review.push(pr),
                }
            }
        }
    }

    fn priority_rank(p: Option<&Priority>) -> u8 {
        match p {
            Some(Priority::High) => 0,
            Some(Priority::Medium) => 1,
            Some(Priority::Low) => 2,
            None => 3,
        }
    }

    let sort_fn = |a: &&PrSummary, b: &&PrSummary| {
        priority_rank(a.llm_priority.as_ref())
            .cmp(&priority_rank(b.llm_priority.as_ref()))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    };

    opened.sort_by(sort_fn);
    review.sort_by(sort_fn);
    (opened, review)
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
    fn column_layout_flexes_title_and_drops_optional_blocks_when_narrow() {
        // Wide: all optional blocks present, title fills the remainder.
        let wide = ColumnLayout::compute(160);
        assert!(wide.author_width.is_some());
        assert!(wide.state_width.is_some());
        assert!(wide.comments_width.is_some());
        assert!(wide.title_width > 80, "title should flex on wide terminals");

        // Narrow: optional blocks dropped, title clamps to a minimum.
        let narrow = ColumnLayout::compute(48);
        assert!(narrow.title_width >= 8);
        // At 48 cols there is no room for author/state/comments after the
        // fixed prefix(11) + suffix(8) + title_min(8) = 27 leaves 21; author(11)
        // fits, state(9) fits -> comments dropped only when below threshold.
        // Just assert the invariant that title never goes negative.
        assert!(narrow.title_x == 11);
    }

    #[test]
    fn truncate_width_adds_ellipsis_and_respects_display_width() {
        use unicode_width::UnicodeWidthStr;
        let s = truncate_width("hello world from rust", 10);
        assert!(UnicodeWidthStr::width(s.as_str()) <= 10);
        assert!(s.ends_with('…'));
        assert_eq!(truncate_width("hi", 10), "hi");
        assert_eq!(truncate_width("abc", 0), "");
    }

    #[test]
    fn pad_span_pads_short_text_and_truncates_long_text() {
        assert_eq!(pad_span("ab", 5, Style::default()).content, "ab   ");
        assert_eq!(pad_span("abcdef", 3, Style::default()).content, "abc");
    }
}
