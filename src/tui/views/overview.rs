use ratatui::layout::{Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::api::PrDetailResponse;
use crate::github::types::{
    CheckStatus, MergeableState, Priority, ReviewDecision, TimelineEventType,
};
use crate::tui::render::component::RenderContext;
use crate::tui::render::layout::fill;
use crate::tui::render::primitives::ScrollViewport;
use crate::tui::render::theme::{
    ADD, BASE, DEL, DRAFT, MANTLE, MED, MUTED, OPEN, OVERLAY0, OVERVIEW, PENDING, SUBTEXT0,
    SURFACE0, TEXT,
};
use crate::tui::state::OverviewFocus;

/// Number of fixed chrome rows above the scrollable body (title, identity line,
/// status/stats row). Shared with `prepare` so scroll clamping matches layout.
pub const OVERVIEW_CHROME_ROWS: u16 = 3;

pub fn render_overview(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    fill(f, area, MANTLE);
    let state = ctx.state;
    let detail = state.pr_detail.as_ref();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(1), // #id title
            ratatui::layout::Constraint::Length(1), // state/author/branch/priority line
            ratatui::layout::Constraint::Length(1), // status + diff stats row
            ratatui::layout::Constraint::Min(1),    // body
        ])
        .split(area);

    render_title(f, chunks[0], detail);
    f.render_widget(header_paragraph(detail, chunks[1].width), chunks[1]);
    render_status_stats_row(f, chunks[2], detail);
    render_body_rows(f, chunks[3], detail, ctx);
}

fn display_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    UnicodeWidthStr::width(s)
}

fn render_title(f: &mut Frame, area: Rect, detail: Option<&PrDetailResponse>) {
    if area.height == 0 {
        return;
    }
    fill(f, area, BASE);
    match detail {
        Some(d) => {
            let label = format!("#{} {}", d.number, d.title);
            let truncated =
                crate::tui::views::text::truncate_to_display_width(&label, area.width as usize);
            // Styled as a prominent heading, not a hyperlink: nothing in this
            // blade is OSC-8 clickable, so underline/link color would mislead.
            f.render_widget(
                Paragraph::new(Span::styled(
                    truncated,
                    Style::default()
                        .fg(OVERVIEW)
                        .bg(BASE)
                        .add_modifier(Modifier::BOLD),
                ))
                .style(Style::default().bg(BASE)),
                area,
            );
        }
        None => {
            f.render_widget(
                Paragraph::new("Select a PR from the inbox")
                    .style(Style::default().fg(MUTED).bg(BASE)),
                area,
            );
        }
    }
}

fn header_paragraph(detail: Option<&PrDetailResponse>, width: u16) -> Paragraph<'static> {
    use crate::tui::render::theme::{HIGH, LOW};
    let Some(d) = detail else {
        // Empty state lives in the title row; keep this row blank.
        return Paragraph::new(Line::from("")).style(Style::default().bg(BASE));
    };

    let priority_color = match d.llm_priority {
        Some(Priority::High) => HIGH,
        Some(Priority::Medium) => MED,
        Some(Priority::Low) => LOW,
        None => OVERLAY0,
    };
    let priority_label = match d.llm_priority {
        Some(p) => format!("{:?}", p).to_lowercase(),
        None => "none".to_string(),
    };

    let state_color = if d.is_draft { DRAFT } else { OPEN };
    let state_seg = if d.is_draft { "[draft]" } else { "[open]" }.to_string();
    let priority_seg = format!("priority: {}", priority_label);
    let sep = " · ";

    // Budget the branch segment so long ref names truncate with an ellipsis
    // instead of being hard-clipped at the right edge.
    let fixed_w = display_width(&state_seg)
        + display_width(&d.author)
        + display_width(&priority_seg)
        + display_width(sep) * 3;
    let branch_full = format!("{} → {}", d.head_ref, d.base_ref);
    let avail = (width as usize).saturating_sub(fixed_w);
    let branch = crate::tui::views::text::truncate_to_display_width(&branch_full, avail);

    let mut segments: Vec<(String, Color)> =
        vec![(state_seg, state_color), (d.author.clone(), SUBTEXT0)];
    if !branch.is_empty() {
        segments.push((branch, SUBTEXT0));
    }
    segments.push((priority_seg, priority_color));

    let mut spans = Vec::new();
    for (i, (text, color)) in segments.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(sep, Style::default().fg(OVERLAY0).bg(BASE)));
        }
        spans.push(Span::styled(text, Style::default().fg(color).bg(BASE)));
    }

    Paragraph::new(Line::from(spans)).style(Style::default().bg(BASE))
}

/// Row of at-a-glance status chips (review decision, mergeability, CI) followed
/// by compact diff/comment counts. Status chips come first so they survive
/// width clipping — they answer "approved? conflicts? CI green?".
fn render_status_stats_row(f: &mut Frame, area: Rect, detail: Option<&PrDetailResponse>) {
    if area.height == 0 {
        return;
    }
    fill(f, area, SURFACE0);
    let Some(d) = detail else {
        return;
    };

    let mut spans = status_chips(d);
    if !spans.is_empty() {
        spans.push(Span::styled(
            " │ ",
            Style::default().fg(OVERLAY0).bg(SURFACE0),
        ));
    }
    spans.extend(diff_stat_spans(d));

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(SURFACE0)),
        area,
    );
}

/// Review/merge/CI status chip labels with their colors. Shared by the
/// Overview status row and the command line (`render_command_line`); each
/// call site applies its own background.
pub fn status_chip_data(d: &PrDetailResponse) -> Vec<(String, Color)> {
    let mut chips: Vec<(String, Color)> = Vec::new();

    let (review_text, review_color) = match d.review_decision {
        Some(ReviewDecision::Approved) => ("approved", ADD),
        Some(ReviewDecision::ChangesRequested) => ("changes req", DEL),
        Some(ReviewDecision::ReviewRequired) => ("review req", MED),
        None => ("no review", OVERLAY0),
    };
    chips.push((review_text.to_string(), review_color));

    match d.mergeable {
        MergeableState::Mergeable => chips.push(("mergeable".to_string(), ADD)),
        MergeableState::Conflicting => chips.push(("conflicts".to_string(), DEL)),
        MergeableState::Unknown => chips.push(("merge ?".to_string(), OVERLAY0)),
    }

    let (checks_text, checks_color) = match d.check_status {
        CheckStatus::Success => ("checks ✓", ADD),
        CheckStatus::Failure => ("checks ✗", DEL),
        CheckStatus::Pending => ("checks …", MED),
        CheckStatus::Neutral => ("checks ~", OVERLAY0),
        CheckStatus::None => ("no checks", OVERLAY0),
    };
    chips.push((checks_text.to_string(), checks_color));

    chips
}

fn status_chips(d: &PrDetailResponse) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, (text, color)) in status_chip_data(d).into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default().bg(SURFACE0)));
        }
        spans.push(Span::styled(
            text,
            Style::default()
                .fg(color)
                .bg(SURFACE0)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans
}

/// Compact diff/comment counts appended after the status chips. This data is
/// otherwise duplicated in full elsewhere (file count in the Files blade,
/// per-event detail in the Activity blade), so it's kept terse here rather
/// than given its own labeled row.
fn diff_stat_spans(d: &PrDetailResponse) -> Vec<Span<'static>> {
    let added: u64 = d.files.iter().map(|f| f.additions).sum();
    let removed: u64 = d.files.iter().map(|f| f.deletions).sum();
    let files = d.files.len() as u64;
    let comments = d.review_threads.len() as u64
        + d.timeline
            .iter()
            .filter(|e| e.event_type == TimelineEventType::Comment)
            .count() as u64;

    let dot = Span::styled(" · ", Style::default().fg(OVERLAY0).bg(SURFACE0));
    vec![
        Span::styled(format!("+{}", added), Style::default().fg(ADD).bg(SURFACE0)),
        Span::styled(" ", Style::default().bg(SURFACE0)),
        Span::styled(
            format!("−{}", removed),
            Style::default().fg(DEL).bg(SURFACE0),
        ),
        dot.clone(),
        Span::styled(
            format!("{} files", files),
            Style::default().fg(MUTED).bg(SURFACE0),
        ),
        dot,
        Span::styled(
            format!("{} comments", comments),
            Style::default().fg(MUTED).bg(SURFACE0),
        ),
    ]
}

/// Whether the Brunson-summary section is in its "loading" state: a rich
/// LLM summary fetch is in flight and the loaded detail doesn't have one yet.
/// Shared by `render_body_rows` and `ViewStateManager::prepare`.
pub fn summary_loading(state: &crate::tui::app::AppState) -> bool {
    state.llm_detail_loading
        && state
            .pr_detail
            .as_ref()
            .is_some_and(|d| d.llm_rich_summary.is_none())
}

/// Effective summary line count fed into [`overview_section_layout`]: while
/// the LLM sparkle is loading, an empty summary still claims one placeholder
/// row (the "Brunson is thinking..." line), so the section stays visible.
pub fn effective_summary_len(lines: usize, loading: bool) -> usize {
    if loading {
        lines.max(1)
    } else {
        lines
    }
}

/// Resolved Overview body layout. `None` means the section is absent this
/// frame (no content), not merely zero-height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverviewSectionLayout {
    /// `Brunson Says...` — absent when there is no summary and none loading.
    pub summary: Option<u16>,
    /// `PROBLEMS` — absent when nothing is wrong.
    pub problems: Option<u16>,
    /// `DESCRIPTION` — always present; absorbs all remaining rows.
    pub description: u16,
    /// `LAST ACTIVITY` — 1 when it fits, else 0.
    pub last_activity: u16,
}

/// Compute the Overview body layout (each present section's height includes
/// its 1-row header). Deterministic and focus-independent: the description
/// always absorbs the remainder, so `OverviewFocus` only decides which
/// section's scroll offset `j`/`k` drives. Present-section heights plus
/// `description` plus `last_activity` always sum exactly to `body_height`.
///
/// Shared by `render_body_rows` and `ViewStateManager::prepare` so scroll
/// clamping matches the rendered viewport.
pub fn overview_section_layout(
    body_height: u16,
    summary_len: usize,
    problems_len: usize,
) -> OverviewSectionLayout {
    let mut remaining = body_height;

    // Last Activity: a single reserved row.
    let last_activity = remaining.min(1);
    remaining -= last_activity;

    // Summary: capped at min(content + header, 40% of the body); scrollable
    // when capped.
    let summary = (summary_len > 0).then(|| {
        let cap = ((body_height as u32) * 40 / 100) as u16;
        let want = (summary_len.min(u16::MAX as usize - 1) as u16).saturating_add(1);
        want.min(cap).min(remaining)
    });
    remaining -= summary.unwrap_or(0);

    // Problems: capped at min(content + header, 6 rows); scrollable.
    let problems = (problems_len > 0).then(|| {
        let want = (problems_len.min(u16::MAX as usize - 1) as u16).saturating_add(1);
        want.min(6).min(remaining)
    });
    remaining -= problems.unwrap_or(0);

    // Description owns everything left.
    OverviewSectionLayout {
        summary,
        problems,
        description: remaining,
        last_activity,
    }
}

/// Render the Overview body as stacked sections: Summary (when present),
/// Problems (when present), Description, Last Activity. Heights come from
/// [`overview_section_layout`], which is also used by `prepare` so scroll
/// clamping matches the rendered viewport.
fn render_body_rows(
    f: &mut Frame,
    area: Rect,
    detail: Option<&PrDetailResponse>,
    ctx: &RenderContext,
) {
    let view = ctx.view;
    fill(f, area, MANTLE);

    let Some(d) = detail else {
        // Empty state is shown in the title row; keep the body blank.
        return;
    };

    let cache = &ctx.state.render_cache;
    let loading_summary = summary_loading(ctx.state);
    let summary_len = effective_summary_len(cache.overview_summary.len(), loading_summary);
    let sections = overview_section_layout(area.height, summary_len, cache.overview_problems.len());

    // Lay sections out top-to-bottom using the shared heights.
    let mut y = area.y;
    let rect_for = |y: u16, h: u16| Rect::new(area.x, y, area.width, h);

    if let Some(h) = sections.summary {
        render_section(
            f,
            rect_for(y, h),
            "Brunson Says...",
            &cache.overview_summary,
            view.overview_summary_scroll.offset,
            view.overview_focus == OverviewFocus::Summary,
            loading_summary,
        );
        y = y.saturating_add(h);
    }
    if let Some(h) = sections.problems {
        render_section(
            f,
            rect_for(y, h),
            "PROBLEMS",
            &cache.overview_problems,
            view.overview_problems_scroll.offset,
            view.overview_focus == OverviewFocus::Problems,
            false,
        );
        y = y.saturating_add(h);
    }
    render_section(
        f,
        rect_for(y, sections.description),
        "DESCRIPTION",
        &cache.overview_description,
        view.overview_description_scroll.offset,
        view.overview_focus == OverviewFocus::Description,
        false,
    );
    y = y.saturating_add(sections.description);
    render_last_activity(
        f,
        rect_for(y, sections.last_activity),
        d,
        view.overview_focus == OverviewFocus::LastActivity,
    );
}

fn render_section(
    f: &mut Frame,
    area: Rect,
    label: &str,
    lines: &[Line<'static>],
    scroll: usize,
    focused: bool,
    loading: bool,
) {
    if area.height == 0 {
        return;
    }
    // Tint the focused section so "where am I" is obvious at a glance.
    let bg = if focused { BASE } else { MANTLE };
    fill(f, area, bg);

    let label_color = if focused { OVERVIEW } else { OVERLAY0 };
    let bar = if focused { "▌" } else { " " };
    let sparkle = if loading { "✨ " } else { "" };
    let header = Paragraph::new(Line::from(vec![
        Span::styled(bar, Style::default().fg(OVERVIEW).bg(bg)),
        Span::styled(
            format!("{}{}", sparkle, label),
            Style::default()
                .fg(label_color)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
    ]))
    .style(Style::default().bg(bg));
    f.render_widget(header, Rect::new(area.x, area.y, area.width, 1));

    let content_height = area.height.saturating_sub(1);
    if content_height == 0 {
        return;
    }
    let content_area = Rect::new(area.x, area.y + 1, area.width, content_height);

    if loading && lines.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                "Brunson is thinking about this PR...",
                Style::default().fg(PENDING).bg(bg),
            )]))
            .style(Style::default().bg(bg)),
            content_area,
        );
        return;
    }

    if lines.is_empty() {
        return;
    }
    ScrollViewport::new(lines, scroll)
        .style(Style::default().fg(TEXT).bg(bg))
        .scrollbar(true)
        .render(f, content_area);
}

fn render_last_activity(f: &mut Frame, area: Rect, detail: &PrDetailResponse, focused: bool) {
    if area.height == 0 {
        return;
    }
    let bg = if focused { BASE } else { MANTLE };
    fill(f, area, bg);
    let last_activity = detail
        .timeline
        .iter()
        .map(|e| e.created_at.as_str())
        .max()
        .unwrap_or("—");
    let accent = if focused { OVERVIEW } else { OVERLAY0 };
    let bar = if focused { "▌" } else { " " };
    let line = Line::from(vec![
        Span::styled(bar, Style::default().fg(OVERVIEW).bg(bg)),
        Span::styled(
            "LAST ACTIVITY",
            Style::default()
                .fg(accent)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "  {}",
                crate::tui::views::activity::short_time(last_activity)
            ),
            Style::default().fg(SUBTEXT0).bg(bg),
        ),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().fg(TEXT).bg(bg)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout_sum(l: &OverviewSectionLayout) -> u16 {
        l.summary.unwrap_or(0) + l.problems.unwrap_or(0) + l.description + l.last_activity
    }

    #[test]
    fn section_layout_sums_to_body_height() {
        for height in 0..=40u16 {
            for (summary, problems) in [(0, 0), (3, 0), (0, 5), (3, 5), (100, 100)] {
                let l = overview_section_layout(height, summary, problems);
                assert_eq!(
                    layout_sum(&l),
                    height,
                    "height={height} summary={summary} problems={problems}"
                );
            }
        }
    }

    #[test]
    fn section_layout_description_absorbs_remainder() {
        let l = overview_section_layout(30, 3, 2);
        assert_eq!(l.summary, Some(4), "summary = content + header");
        assert_eq!(l.problems, Some(3), "problems = content + header");
        assert_eq!(l.last_activity, 1);
        assert_eq!(l.description, 30 - 4 - 3 - 1);

        // With no summary/problems, the description owns everything but the
        // Last Activity row.
        let l = overview_section_layout(30, 0, 0);
        assert_eq!(l.summary, None);
        assert_eq!(l.problems, None);
        assert_eq!(l.description, 29);
    }

    #[test]
    fn section_layout_caps_summary_and_problems() {
        let l = overview_section_layout(30, 100, 100);
        // Summary caps at 40% of the body.
        assert_eq!(l.summary, Some(12));
        // Problems caps at 6 rows.
        assert_eq!(l.problems, Some(6));
        assert!(l.description > 0, "description keeps the remainder");
    }

    #[test]
    fn problems_section_absent_when_checks_green() {
        let l = overview_section_layout(30, 3, 0);
        assert_eq!(l.problems, None);
    }

    #[test]
    fn section_layout_degrades_without_overflow_on_tiny_bodies() {
        for height in 0..=6u16 {
            let l = overview_section_layout(height, 10, 10);
            assert_eq!(layout_sum(&l), height, "height={height}");
        }
    }

    #[test]
    fn effective_summary_len_reserves_a_row_while_loading() {
        assert_eq!(effective_summary_len(0, true), 1);
        assert_eq!(effective_summary_len(0, false), 0);
        assert_eq!(effective_summary_len(5, true), 5);
        assert_eq!(effective_summary_len(5, false), 5);
    }
}
