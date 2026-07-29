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
    BASE, FAIL, HIGH, INBOX, LOW, MANTLE, MED, MUTED, OVERLAY0, PASS, SUBTEXT0, SURFACE0, TEXT,
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

    // The bottom of the body is reserved for the selected-PR preview strip
    // (when tall enough); the list scrolls in the remainder.
    let body_full = chunks[1];
    let strip_rows = inbox_preview_rows(body_full.height);
    let body = Rect {
        height: body_full.height.saturating_sub(strip_rows),
        ..body_full
    };
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

    if strip_rows > 0 {
        let strip = Rect {
            y: body_full.y + body.height,
            height: strip_rows,
            ..body_full
        };
        let selected = state
            .selected_pr_id
            .as_deref()
            .and_then(|id| by_id.get(id).copied());
        let strip_lines = preview_strip_lines(selected, strip.width);
        f.render_widget(
            Paragraph::new(strip_lines).style(Style::default().bg(BASE)),
            strip,
        );
    }
}

/// Rows reserved at the bottom of the Inbox body for the selected-PR preview
/// strip. `body_height` is the Inbox body height (blade content minus the
/// 1-row column header). Shared by `render_inbox` and
/// `ViewStateManager::prepare` so scroll clamping matches rendering.
pub fn inbox_preview_rows(body_height: u16) -> u16 {
    if body_height >= 8 {
        2
    } else {
        0
    }
}

/// The two preview-strip lines for the selected PR: its one-line LLM summary,
/// then `repo · branch (age)` — the branch `^y` is about to copy, visible
/// before you copy it.
fn preview_strip_lines(pr: Option<&PrSummary>, width: u16) -> Vec<Line<'static>> {
    let Some(pr) = pr else {
        return vec![
            Line::from(Span::styled(
                " — no PR selected",
                Style::default().fg(MUTED).bg(BASE),
            )),
            Line::default(),
        ];
    };
    let max = (width as usize).saturating_sub(2);

    let summary = pr
        .llm_one_line
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("— no summary yet");
    let summary_line = Line::from(Span::styled(
        format!(
            " {}",
            crate::tui::views::text::truncate_to_display_width(summary, max)
        ),
        Style::default().fg(TEXT).bg(BASE),
    ));

    let branch = if pr.head_ref.trim().is_empty() {
        "—"
    } else {
        pr.head_ref.trim()
    };
    let branch_text = format!(" {} · {} ({})", pr.repo, branch, age(&pr.updated_at));
    let branch_line = Line::from(Span::styled(
        crate::tui::views::text::truncate_to_display_width(&branch_text, max + 2),
        Style::default().fg(MUTED).bg(BASE),
    ));

    vec![summary_line, branch_line]
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
    /// Width of the mandatory NEXT column (the daemon's triage verb).
    next_width: u16,
}

impl ColumnLayout {
    /// Lay out columns for the given body width.
    ///
    /// Fixed prefix: `▌ ● #12345 ` (select bar, priority dot, PR number).
    /// Fixed suffix: NEXT action label + age. The title flexes to fill the
    /// remainder, but is guaranteed at least 40% of the body width before the
    /// optional author column is granted; author is dropped first as the
    /// terminal narrows. NEXT is mandatory: 16 cols on bodies ≥ 64 cols (fits
    /// the longest label, `Address feedback`, untruncated), else 8 cols with
    /// truncation.
    fn compute(width: u16) -> Self {
        // prefix: select(1) + gap(1) + prio(1) + gap(1) + number(6) + gap(1) = 11
        let prefix = 11u16;
        let next_width: u16 = if width >= 64 { 16 } else { 8 };
        // mandatory suffix: gap(1) + next + gap(1) + age(4)
        let suffix_fixed = 1 + next_width + 1 + 4;
        // The title must hold at least 40% of the body (never below a hard floor
        // for very narrow terminals) before optional columns compete for space.
        let title_min = ((width as u32 * 40 / 100) as u16).max(8);

        let mut used = prefix.saturating_add(suffix_fixed);
        let room = width.saturating_sub(used.saturating_add(title_min));

        let mut author_width = None;
        // Author is the only optional block: gap(1) + content(10).
        if room >= 11 {
            author_width = Some(10);
            used += 11;
        }

        let title_width = width.saturating_sub(used).max(title_min);
        Self {
            title_x: prefix,
            title_width,
            author_width,
            next_width,
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
    spans.push(pad_span("NEXT", cols.next_width, label_style));
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

    // NEXT action (mandatory) — the daemon-computed triage verb.
    spans.push(Span::styled(" ", base));
    spans.extend(next_action_spans(pr, cols.next_width, row_bg));

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

/// Style for a `next_action` label, mapped by urgency: red for "your CI/your
/// feedback is blocking", accent for "review work", green for "ship it",
/// muted for "nothing to do". Unknown labels fall back to a neutral subtext.
fn next_action_style(label: &str) -> Style {
    match label {
        "Fix CI" | "Address feedback" => Style::default().fg(FAIL),
        "Review now" | "Re-review" | "Respond" => {
            Style::default().fg(INBOX).add_modifier(Modifier::BOLD)
        }
        "Merge" => Style::default().fg(PASS),
        "Waiting" => Style::default().fg(MUTED),
        _ => Style::default().fg(SUBTEXT0),
    }
}

/// Spans for the fixed-width NEXT cell. Failing CI on a row whose action isn't
/// already `Fix CI` (e.g. a review-lane PR) gets a compact red `✕` appended so
/// reviewers still see red CI without a dedicated checks column.
fn next_action_spans(pr: &PrSummary, width: u16, row_bg: Color) -> Vec<Span<'static>> {
    let style = next_action_style(&pr.next_action).bg(row_bg);
    let ci_flag = pr.check_status == CheckStatus::Failure && pr.next_action != "Fix CI";
    if ci_flag && width >= 4 {
        let label_width = width - 2;
        vec![
            pad_span(&pr.next_action, label_width, style),
            Span::styled(" ", Style::default().bg(row_bg)),
            Span::styled("✕", Style::default().fg(FAIL).bg(row_bg)),
        ]
    } else {
        vec![pad_span(&pr.next_action, width, style)]
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

    use crate::github::types::PrGroup;

    fn sample_pr(next_action: &str, check_status: CheckStatus) -> PrSummary {
        PrSummary {
            id: "org~repo~7".into(),
            node_id: "n7".into(),
            owner: "org".into(),
            repo: "repo".into(),
            number: 7,
            title: "Add feature".into(),
            author: "alice".into(),
            author_is_bot: false,
            group: PrGroup::ReviewNeeded,
            next_action: next_action.into(),
            check_status,
            llm_priority: None,
            updated_at: "2024-01-01T00:00:00Z".into(),
            url: "https://example.com".into(),
            comments: 0,
            head_ref: "feature/thing".into(),
            llm_one_line: Some("Adds the thing".into()),
        }
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Char (display-column) index of `needle` in `text`. Byte offsets would
    /// disagree between header and data rows because rows contain multibyte
    /// glyphs like the priority dot.
    fn char_pos(text: &str, needle: &str) -> Option<usize> {
        text.find(needle).map(|b| text[..b].chars().count())
    }

    #[test]
    fn column_layout_next_width_by_body_width() {
        // ≥ 64 cols: NEXT is 16 wide (fits "Address feedback" untruncated).
        assert_eq!(ColumnLayout::compute(64).next_width, 16);
        assert_eq!(ColumnLayout::compute(160).next_width, 16);
        // < 64 cols: NEXT shrinks to 8 with truncation.
        assert_eq!(ColumnLayout::compute(63).next_width, 8);
        assert_eq!(ColumnLayout::compute(30).next_width, 8);
    }

    #[test]
    fn column_layout_drops_author_before_next() {
        // Wide: author present, title flexes past its 40% floor.
        let wide = ColumnLayout::compute(160);
        assert!(wide.author_width.is_some());
        assert!(wide.title_width > 80, "title should flex on wide terminals");
        assert!(
            wide.title_width >= (160 * 40 / 100),
            "title keeps at least 40% of body width"
        );
        assert_eq!(wide.title_x, 11);

        // Narrow: author drops, NEXT survives (it is mandatory).
        let narrow = ColumnLayout::compute(40);
        assert!(narrow.author_width.is_none());
        assert_eq!(narrow.next_width, 8);
        assert!(narrow.title_width >= 8);
    }

    #[test]
    fn pr_row_renders_next_action_column() {
        let pr = sample_pr("Review now", CheckStatus::Success);
        let cols = ColumnLayout::compute(100);
        let line = pr_row_line(&pr, "me", false, &cols, 100);
        let text = line_text(&line);
        assert!(text.contains("Review now"), "row shows the NEXT action");
        assert!(!text.contains('✎'), "comment-count column has been removed");
        // NEXT sits between the author column and the age column.
        let next_pos = text.find("Review now").unwrap();
        let author_pos = text.find("@alice").unwrap();
        assert!(author_pos < next_pos);
    }

    #[test]
    fn pr_row_appends_ci_cross_when_checks_fail_on_non_fix_ci_action() {
        let pr = sample_pr("Review now", CheckStatus::Failure);
        let cols = ColumnLayout::compute(100);
        let line = pr_row_line(&pr, "me", false, &cols, 100);
        assert!(line_text(&line).contains('✕'), "red CI flag in NEXT cell");

        // A "Fix CI" row already communicates failing CI; no extra flag.
        let pr = sample_pr("Fix CI", CheckStatus::Failure);
        let line = pr_row_line(&pr, "me", false, &cols, 100);
        assert!(!line_text(&line).contains('✕'));
    }

    #[test]
    fn next_action_style_maps_urgency_colors() {
        assert_eq!(next_action_style("Fix CI").fg, Some(FAIL));
        assert_eq!(next_action_style("Address feedback").fg, Some(FAIL));
        assert_eq!(next_action_style("Review now").fg, Some(INBOX));
        assert_eq!(next_action_style("Re-review").fg, Some(INBOX));
        assert_eq!(next_action_style("Respond").fg, Some(INBOX));
        assert_eq!(next_action_style("Merge").fg, Some(PASS));
        assert_eq!(next_action_style("Waiting").fg, Some(MUTED));
        assert_eq!(next_action_style("Something new").fg, Some(SUBTEXT0));
    }

    #[test]
    fn header_labels_align_with_columns() {
        let cols = ColumnLayout::compute(100);
        let header = line_text(&header_line("", 0, 0, &cols));
        let row = line_text(&pr_row_line(
            &sample_pr("Review now", CheckStatus::Success),
            "me",
            false,
            &cols,
            100,
        ));
        assert!(header.contains("AUTHOR"));
        assert!(header.contains("NEXT"));
        assert!(header.contains("AGE"));
        assert!(!header.contains('✓'));
        assert!(!header.contains('✎'));
        // The NEXT label starts at the same display column as the action text.
        assert_eq!(char_pos(&header, "NEXT"), char_pos(&row, "Review now"));
    }

    #[test]
    fn preview_strip_shows_summary_and_head_ref() {
        let pr = sample_pr("Review now", CheckStatus::Success);
        let lines = preview_strip_lines(Some(&pr), 80);
        assert_eq!(lines.len(), 2);
        assert!(line_text(&lines[0]).contains("Adds the thing"));
        let branch_row = line_text(&lines[1]);
        assert!(branch_row.contains("repo"));
        assert!(branch_row.contains("feature/thing"));
    }

    #[test]
    fn preview_strip_falls_back_when_summary_missing() {
        let mut pr = sample_pr("Review now", CheckStatus::Success);
        pr.llm_one_line = None;
        pr.head_ref = String::new();
        let lines = preview_strip_lines(Some(&pr), 80);
        assert!(line_text(&lines[0]).contains("— no summary yet"));
        assert!(line_text(&lines[1]).contains('—'), "missing branch is —");
    }

    #[test]
    fn preview_strip_hidden_below_height_threshold() {
        assert_eq!(inbox_preview_rows(7), 0);
        assert_eq!(inbox_preview_rows(8), 2);
        assert_eq!(inbox_preview_rows(40), 2);
    }

    #[test]
    fn pad_span_pads_short_text_and_truncates_long_text() {
        assert_eq!(pad_span("ab", 5, Style::default()).content, "ab   ");
        assert_eq!(pad_span("abcdef", 3, Style::default()).content, "abc");
    }
}
