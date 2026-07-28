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

fn status_chips(d: &PrDetailResponse) -> Vec<Span<'static>> {
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

    let mut spans = Vec::new();
    for (i, (text, color)) in chips.into_iter().enumerate() {
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
/// comment count in the Inbox's ✎ column), so it's kept terse here rather
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

/// Number of Description content lines shown when collapsed (the default).
/// The rest of the description is hidden behind the `d to expand` marker so a
/// long markdown body doesn't push Checks/Last Activity off screen.
const COLLAPSED_DESCRIPTION_LINES: usize = 4;

/// Number of lines [`description_display_lines`] would render for a description
/// of `total` lines. Shared with `ViewStateManager::prepare` so the Description
/// scroll clamp matches the rows actually shown (a collapsed preview claims only
/// the preview lines plus the expand marker, not the full body).
pub fn description_display_line_count(total: usize, expanded: bool) -> usize {
    if expanded || total <= COLLAPSED_DESCRIPTION_LINES {
        total
    } else {
        COLLAPSED_DESCRIPTION_LINES + 1
    }
}

/// Build the lines actually shown for the Description section: a short
/// preview plus an expand hint when collapsed, or everything when expanded
/// (or when the description is already short enough that collapsing it
/// wouldn't hide anything).
fn description_display_lines(lines: &[Line<'static>], expanded: bool) -> Vec<Line<'static>> {
    if expanded || lines.len() <= COLLAPSED_DESCRIPTION_LINES {
        return lines.to_vec();
    }
    let mut preview = lines[..COLLAPSED_DESCRIPTION_LINES].to_vec();
    let more = lines.len() - COLLAPSED_DESCRIPTION_LINES;
    preview.push(Line::from(Span::styled(
        format!(
            "… d to expand ({more} more line{})",
            if more == 1 { "" } else { "s" }
        ),
        Style::default().fg(MUTED),
    )));
    preview
}

/// Render the Overview body as stacked sections: Summary, Description, Checks,
/// Last Activity. Heights are computed by [`overview_section_heights`], which is
/// also used by `prepare` so scroll clamping matches the rendered viewport.
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
    let description_expanded = view.overview_description_expanded;
    let description_lines =
        description_display_lines(&cache.overview_description, description_expanded);
    // Feed the *displayed* line count (not the full description length) into
    // the height split, so a collapsed description only claims the couple of
    // rows it actually renders instead of ballooning to fit hidden content.
    let lens = [
        cache.overview_summary.len(),
        description_lines.len(),
        cache.overview_checks.len(),
    ];
    let heights = overview_section_heights(area.height, lens, view.overview_focus);

    // Lay sections out top-to-bottom using the shared heights.
    let mut y = area.y;
    let rect_for = |y: u16, h: u16| Rect::new(area.x, y, area.width, h);

    let loading_summary = ctx.state.llm_detail_loading
        && ctx
            .state
            .pr_detail
            .as_ref()
            .and_then(|d| d.llm_rich_summary.as_ref())
            .is_none();
    render_section(
        f,
        rect_for(y, heights[0]),
        "Brunson Says...",
        &cache.overview_summary,
        view.overview_summary_scroll.offset,
        view.overview_focus == OverviewFocus::Summary,
        loading_summary,
    );
    y = y.saturating_add(heights[0]);
    render_section(
        f,
        rect_for(y, heights[1]),
        "DESCRIPTION",
        &description_lines,
        // Scrolling a preview doesn't make sense; only honor the scroll
        // offset once the full description is showing.
        if description_expanded {
            view.overview_description_scroll.offset
        } else {
            0
        },
        view.overview_focus == OverviewFocus::Description,
        false,
    );
    y = y.saturating_add(heights[1]);
    render_section(
        f,
        rect_for(y, heights[2]),
        "CHECKS",
        &cache.overview_checks,
        view.overview_checks_scroll.offset,
        view.overview_focus == OverviewFocus::Checks,
        false,
    );
    y = y.saturating_add(heights[2]);
    render_last_activity(
        f,
        rect_for(y, heights[3]),
        d,
        view.overview_focus == OverviewFocus::LastActivity,
    );
}

/// Compute per-section heights (including each section's header row) for the
/// Overview body. The three scrollable sections (summary, description, checks)
/// grow to show their content; the focused section absorbs any leftover rows so
/// scrolling feels natural. Last Activity is always a single line.
///
/// The returned heights always sum to `body_height` (when it is at least 1), so
/// callers can lay sections out contiguously with no gaps or overflow.
pub fn overview_section_heights(
    body_height: u16,
    lens: [usize; 3],
    focus: OverviewFocus,
) -> [u16; 4] {
    if body_height == 0 {
        return [0, 0, 0, 0];
    }

    // Reserve one line for Last Activity.
    let last = 1u16.min(body_height);
    let mut remaining = body_height - last;
    let mut heights = [0u16, 0, 0];

    // 1. A header row for each scrollable section we can fit.
    for h in heights.iter_mut() {
        if remaining == 0 {
            break;
        }
        *h += 1;
        remaining -= 1;
    }
    // 2. A first body row each, so even a focused-but-short section shows content.
    for h in heights.iter_mut() {
        if remaining == 0 {
            break;
        }
        if *h >= 1 {
            *h += 1;
            remaining -= 1;
        }
    }

    let focus_idx = match focus {
        OverviewFocus::Summary => Some(0usize),
        OverviewFocus::Description => Some(1),
        OverviewFocus::Checks => Some(2),
        OverviewFocus::LastActivity => None,
    };

    // 3. Distribute remaining rows by content need, focused section first.
    let mut order: Vec<usize> = Vec::new();
    if let Some(fi) = focus_idx {
        order.push(fi);
    }
    for i in 0..3 {
        if Some(i) != focus_idx {
            order.push(i);
        }
    }
    for &i in &order {
        if remaining == 0 {
            break;
        }
        if heights[i] == 0 {
            continue;
        }
        // Rows still wanted to show all content: 1 header + lens[i] body lines.
        let want = 1u16.saturating_add(lens[i].min(u16::MAX as usize) as u16);
        let grant = remaining.min(want.saturating_sub(heights[i]));
        heights[i] += grant;
        remaining -= grant;
    }

    // 4. Any slack goes to the focused scrollable section (or description as a
    //    default when Last Activity is focused), so no rows are wasted as gaps.
    if remaining > 0 {
        let sink = focus_idx.unwrap_or(1);
        if heights[sink] > 0 {
            heights[sink] += remaining;
        } else if let Some(i) = heights.iter().position(|h| *h > 0) {
            heights[i] += remaining;
        }
    }

    [heights[0], heights[1], heights[2], last]
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

    #[test]
    fn section_heights_sum_to_body_height() {
        for height in 0..=40u16 {
            let h = overview_section_heights(height, [3, 50, 5], OverviewFocus::Description);
            assert_eq!(h.iter().sum::<u16>(), height, "height={height}");
        }
    }

    #[test]
    fn focused_section_absorbs_slack_when_all_content_is_short() {
        // Plenty of room, tiny content: the focused section should grow.
        let h = overview_section_heights(30, [1, 1, 1], OverviewFocus::Summary);
        assert_eq!(h.iter().sum::<u16>(), 30);
        assert!(
            h[0] > h[1] && h[0] > h[2],
            "focused summary should be tallest: {h:?}"
        );
    }

    #[test]
    fn long_section_grows_even_when_unfocused() {
        // Description has lots of content; it should claim most of the space
        // even though Summary is focused, because Summary doesn't need it.
        let h = overview_section_heights(30, [1, 50, 1], OverviewFocus::Summary);
        assert_eq!(h.iter().sum::<u16>(), 30);
        assert!(
            h[1] > h[0],
            "long description should outgrow summary: {h:?}"
        );
    }

    #[test]
    fn last_activity_is_single_line_when_room_exists() {
        let h = overview_section_heights(20, [2, 2, 2], OverviewFocus::Summary);
        assert_eq!(h[3], 1);
    }

    #[test]
    fn degrades_without_overflow_on_tiny_bodies() {
        for height in 0..=6u16 {
            let h = overview_section_heights(height, [10, 10, 10], OverviewFocus::Checks);
            assert_eq!(h.iter().sum::<u16>(), height, "height={height}");
        }
    }

    fn lines(n: usize) -> Vec<Line<'static>> {
        (0..n).map(|i| Line::from(format!("line {i}"))).collect()
    }

    #[test]
    fn collapsed_description_shows_preview_plus_marker() {
        let full = lines(40);
        let shown = description_display_lines(&full, false);
        assert_eq!(shown.len(), COLLAPSED_DESCRIPTION_LINES + 1);
        assert_eq!(shown[0].spans[0].content, "line 0");
        assert!(shown.last().unwrap().spans[0]
            .content
            .contains("36 more lines"));
    }

    #[test]
    fn expanded_description_shows_everything() {
        let full = lines(40);
        let shown = description_display_lines(&full, true);
        assert_eq!(shown.len(), 40);
    }

    #[test]
    fn short_description_is_not_truncated_even_when_collapsed() {
        let full = lines(COLLAPSED_DESCRIPTION_LINES);
        let shown = description_display_lines(&full, false);
        assert_eq!(shown.len(), COLLAPSED_DESCRIPTION_LINES);
    }
}
