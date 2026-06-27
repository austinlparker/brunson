use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::api::PrDetailResponse;
use crate::github::types::Priority;
use crate::tui::render::component::RenderContext;
use crate::tui::render::layout::fill;
use crate::tui::render::primitives::ScrollViewport;
use crate::tui::render::theme::{
    ADD, BASE, DEL, DRAFT, HUNK, ICON_COMMENT, ICON_DIFF_ADDED, ICON_DIFF_REMOVED, ICON_FILES,
    LINK, MANTLE, MUTED, OPEN, OVERLAY0, OVERVIEW, SUBTEXT0, SURFACE0, TEXT,
};
use crate::tui::state::OverviewFocus;

pub fn render_overview(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    fill(f, area, MANTLE);
    let state = ctx.state;
    let detail = state.pr_detail.as_ref();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // #id title (clickable link)
            Constraint::Length(1), // state/author/branch/priority line
            Constraint::Length(1), // stat tiles
            Constraint::Min(1),    // body
        ])
        .split(area);

    render_title(f, chunks[0], detail);
    f.render_widget(header_paragraph(detail), chunks[1]);
    f.render_widget(stat_tiles(detail), chunks[2]);
    render_body_rows(f, chunks[3], detail, ctx);
}

fn truncate_to_display(s: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
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

fn render_title(f: &mut Frame, area: Rect, detail: Option<&PrDetailResponse>) {
    if area.height == 0 {
        return;
    }
    fill(f, area, BASE);
    match detail {
        Some(d) => {
            let label = format!("#{} {}", d.number, d.title);
            let truncated = truncate_to_display(&label, area.width as usize);
            f.render_widget(
                Paragraph::new(Span::styled(
                    truncated,
                    Style::default()
                        .fg(LINK)
                        .bg(BASE)
                        .add_modifier(Modifier::BOLD)
                        .add_modifier(Modifier::UNDERLINED),
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

fn header_paragraph(detail: Option<&PrDetailResponse>) -> Paragraph<'static> {
    use crate::tui::render::theme::{HIGH, LOW, MED};
    let lines = match detail {
        Some(d) => {
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
            let state_label = if d.is_draft { "draft" } else { "open" };

            vec![Line::from(vec![
                Span::styled(
                    format!("[{}]", state_label),
                    Style::default().fg(state_color).bg(BASE),
                ),
                Span::styled(" · ", Style::default().fg(OVERLAY0).bg(BASE)),
                Span::styled(d.author.clone(), Style::default().fg(SUBTEXT0).bg(BASE)),
                Span::styled(" · ", Style::default().fg(OVERLAY0).bg(BASE)),
                Span::styled(
                    format!("{} → {}", d.head_ref, d.base_ref),
                    Style::default().fg(SUBTEXT0).bg(BASE),
                ),
                Span::styled(" · ", Style::default().fg(OVERLAY0).bg(BASE)),
                Span::styled(
                    format!("priority: {}", priority_label),
                    Style::default().fg(priority_color).bg(BASE),
                ),
            ])]
        }
        None => vec![Line::from(vec![Span::styled(
            "Select a PR from the inbox",
            Style::default().fg(MUTED).bg(BASE),
        )])],
    };

    Paragraph::new(lines).style(Style::default().bg(BASE))
}

fn stat_tiles(detail: Option<&PrDetailResponse>) -> Paragraph<'static> {
    let (added, removed, files, comments) = match detail {
        Some(d) => (
            d.files.iter().map(|f| f.additions).sum(),
            d.files.iter().map(|f| f.deletions).sum(),
            d.files.len() as u64,
            d.review_threads.len() as u64
                + d.timeline
                    .iter()
                    .filter(|e| e.event_type == "comment")
                    .count() as u64,
        ),
        None => (0, 0, 0, 0),
    };

    let tiles = [
        (
            format!("{} ADDED", ICON_DIFF_ADDED),
            format!("+{}", added),
            ADD,
        ),
        (
            format!("{} REMOVED", ICON_DIFF_REMOVED),
            format!("−{}", removed),
            DEL,
        ),
        (format!("{} FILES", ICON_FILES), files.to_string(), HUNK),
        (
            format!("{} COMMENTS", ICON_COMMENT),
            comments.to_string(),
            OVERVIEW,
        ),
    ];

    let spans: Vec<Span> = tiles
        .iter()
        .enumerate()
        .flat_map(|(i, (label, value, color))| {
            let mut s = vec![];
            if i > 0 {
                s.push(Span::styled("  ", Style::default().bg(SURFACE0)));
            }
            s.push(Span::styled(
                label.to_string(),
                Style::default()
                    .fg(*color)
                    .bg(SURFACE0)
                    .add_modifier(Modifier::BOLD),
            ));
            s.push(Span::styled(" ", Style::default().bg(SURFACE0)));
            s.push(Span::styled(
                value.clone(),
                Style::default().fg(TEXT).bg(SURFACE0),
            ));
            s
        })
        .collect();

    Paragraph::new(Line::from(spans)).style(Style::default().bg(SURFACE0))
}

/// Render the Overview body as stacked sections: Summary, Description, Checks, Last Activity.
fn render_body_rows(
    f: &mut Frame,
    area: Rect,
    detail: Option<&PrDetailResponse>,
    ctx: &RenderContext,
) {
    let view = ctx.view;
    fill(f, area, MANTLE);

    let Some(d) = detail else {
        f.render_widget(
            Paragraph::new("No PR selected").style(Style::default().fg(MUTED).bg(MANTLE)),
            area,
        );
        return;
    };

    // Always allocate at least one content row to each section so narrow
    // terminals still scroll instead of dropping sections.
    let constraints = [
        Constraint::Length(2), // summary: 1 header + ≥1 body (grown below)
        Constraint::Length(2), // description
        Constraint::Length(2), // checks
        Constraint::Length(1), // last activity
    ];
    // Grow the sections to fill the available height.
    let laid = Layout::default()
        .direction(Direction::Vertical)
        .constraints(grow_constraints(area.height, &constraints))
        .split(area);

    render_section(
        f,
        laid[0],
        "SUMMARY",
        &ctx.state.render_cache.overview_summary,
        view.overview_summary_scroll.offset,
        view.overview_focus == OverviewFocus::Summary,
    );
    render_section(
        f,
        laid[1],
        "DESCRIPTION",
        &ctx.state.render_cache.overview_description,
        view.overview_description_scroll.offset,
        view.overview_focus == OverviewFocus::Description,
    );
    render_section(
        f,
        laid[2],
        "CHECKS",
        &ctx.state.render_cache.overview_checks,
        view.overview_checks_scroll.offset,
        view.overview_focus == OverviewFocus::Checks,
    );
    render_last_activity(
        f,
        laid[3],
        d,
        view.overview_focus == OverviewFocus::LastActivity,
    );
}

/// Convert the per-section minimums into constraints that fill the body height,
/// distributing leftover rows across the three scrollable sections.
fn grow_constraints(height: u16, base: &[Constraint; 4]) -> [Constraint; 4] {
    let min_total: u16 = base.iter().map(constraint_min).sum();
    let extra = height.saturating_sub(min_total);
    // Distribute extra rows evenly across summary/description/checks (indices 0..3).
    let per = extra / 3;
    let rem = extra % 3;
    let mut out = [Constraint::Length(0); 4];
    for (i, c) in base.iter().enumerate() {
        let m = constraint_min(c);
        if i < 3 {
            let bump = per + if i < rem as usize { 1 } else { 0 };
            out[i] = Constraint::Length(m + bump);
        } else {
            out[i] = *c;
        }
    }
    out
}

fn constraint_min(c: &Constraint) -> u16 {
    match c {
        Constraint::Length(n) => *n,
        Constraint::Min(n) => *n,
        _ => 1,
    }
}

fn render_section(
    f: &mut Frame,
    area: Rect,
    label: &str,
    lines: &[Line<'static>],
    scroll: usize,
    focused: bool,
) {
    if area.height == 0 {
        return;
    }
    fill(f, area, MANTLE);

    let label_color = if focused { OVERVIEW } else { OVERLAY0 };
    let bar = if focused { "▌" } else { " " };
    let header = Paragraph::new(Line::from(vec![
        Span::styled(bar, Style::default().fg(OVERVIEW).bg(MANTLE)),
        Span::styled(
            label,
            Style::default()
                .fg(label_color)
                .bg(MANTLE)
                .add_modifier(Modifier::BOLD),
        ),
    ]))
    .style(Style::default().bg(MANTLE));
    f.render_widget(header, Rect::new(area.x, area.y, area.width, 1));

    let content_height = area.height.saturating_sub(1);
    if content_height == 0 || lines.is_empty() {
        return;
    }
    let content_area = Rect::new(area.x, area.y + 1, area.width, content_height);
    ScrollViewport::new(lines, scroll)
        .style(Style::default().fg(TEXT).bg(MANTLE))
        .scrollbar(true)
        .render(f, content_area);
}

fn render_last_activity(f: &mut Frame, area: Rect, detail: &PrDetailResponse, focused: bool) {
    fill(f, area, MANTLE);
    let last_activity = detail
        .timeline
        .iter()
        .map(|e| e.created_at.as_str())
        .max()
        .unwrap_or("—");
    let accent = if focused { OVERVIEW } else { OVERLAY0 };
    let bar = if focused { "▌" } else { " " };
    let line = Line::from(vec![
        Span::styled(bar, Style::default().fg(OVERVIEW).bg(MANTLE)),
        Span::styled(
            "LAST ACTIVITY",
            Style::default()
                .fg(accent)
                .bg(MANTLE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "  {}",
                crate::tui::views::activity::short_time(last_activity)
            ),
            Style::default().fg(SUBTEXT0).bg(MANTLE),
        ),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().fg(TEXT).bg(MANTLE)),
        area,
    );
}
