use chrono::{DateTime, Local, Utc};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::render::component::RenderContext;
use crate::tui::render::theme::{
    BASE, FAIL, ICON_PR, ICON_SYNC, OVERLAY0, PENDING, SUBTEXT0, SURFACE0, TEXT,
};

/// Render the statusline just above the keybar.
/// Shows the current PR title/number, a staleness indicator, and blade name:
/// `❯ #id title · <blade> · <sync> 2m █`.
pub fn render_command_line(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    let state = ctx.state;
    let accent = ctx.view.active_blade.accent();

    let title = if let Some(detail) = state.pr_detail.as_ref() {
        format!("{} #{} {}", ICON_PR, detail.number, detail.title)
    } else if let Some(id) = state.selected_pr_id.as_ref() {
        format!("{} {}", ICON_PR, id.replace('~', "/"))
    } else {
        crate::daemon::SERVICE_NAME.to_string()
    };

    let cursor = "█";
    let blade_name = ctx.view.active_blade.name();
    let (right_spans, right_width) = status_right_spans(state, accent, blade_name);
    // Account for "❯ " prefix, the right-hand suffix, a space, and the cursor.
    let overhead = 2 + right_width + 1 + 1;
    let max_title_width = area.width.saturating_sub(overhead as u16).max(1) as usize;
    let display_title = truncate_to_char_width(&title, max_title_width);

    let mut spans = vec![
        Span::styled("❯ ", Style::default().fg(TEXT).add_modifier(Modifier::DIM)),
        Span::styled(
            display_title,
            Style::default().fg(TEXT).add_modifier(Modifier::DIM),
        ),
    ];
    spans.extend(right_spans);
    spans.push(Span::styled(" ", Style::default()));
    spans.push(Span::styled(cursor, Style::default().fg(accent)));

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(BASE)),
        area,
    );
}

/// Build the right-hand portion of the statusline (blade name + sync/age meta).
/// Returns the spans and the character width they consume.
fn status_right_spans(
    state: &crate::tui::app::AppState,
    accent: ratatui::style::Color,
    blade_name: &'static str,
) -> (Vec<Span<'static>>, usize) {
    let mut spans = Vec::new();
    let mut width = 0usize;

    let blade_suffix = format!(" · {}", blade_name);
    spans.push(Span::styled(
        blade_suffix.clone(),
        Style::default().fg(accent),
    ));
    width += blade_suffix.chars().count();

    let refresh_active = state
        .health
        .as_ref()
        .map_or(state.loading, |h| h.refresh_in_progress);

    let source = state
        .health
        .as_ref()
        .and_then(|h| h.last_poll_at.as_deref())
        .filter(|s| !s.is_empty())
        .or(if state.prs.updated_at.is_empty() {
            None
        } else {
            Some(state.prs.updated_at.as_str())
        });
    let age = source
        .and_then(parse_timestamp)
        .map(|dt| Utc::now().signed_duration_since(dt));
    let age_label = age.map(format_stale_age).unwrap_or_else(|| "—".to_string());
    let age_color = age
        .map(|d| {
            let secs = d.num_seconds();
            if secs < 300 {
                SUBTEXT0
            } else if secs < 900 {
                PENDING
            } else {
                FAIL
            }
        })
        .unwrap_or(OVERLAY0);

    let sep = " · ";
    spans.push(Span::styled(
        sep.to_string(),
        Style::default().fg(SUBTEXT0).add_modifier(Modifier::DIM),
    ));
    width += sep.chars().count();

    if refresh_active {
        spans.push(Span::styled(
            ICON_SYNC.to_string(),
            Style::default().fg(PENDING).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {}", age_label),
            Style::default().fg(age_color).add_modifier(Modifier::DIM),
        ));
        width += ICON_SYNC.chars().count() + 1 + age_label.chars().count();
    } else {
        spans.push(Span::styled(
            age_label.clone(),
            Style::default().fg(age_color).add_modifier(Modifier::DIM),
        ));
        width += age_label.chars().count();
    }

    (spans, width)
}

/// Parse an RFC 3339 timestamp, returning `None` on failure.
fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Format a positive duration as a short staleness label.
fn format_stale_age(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    if secs < 60 {
        "<1m".to_string()
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

fn truncate_to_char_width(s: &str, width: usize) -> String {
    let count = s.chars().count();
    if count <= width {
        s.to_string()
    } else if width <= 3 {
        s.chars().take(width).collect()
    } else {
        format!(
            "{}…",
            s.chars().take(width.saturating_sub(1)).collect::<String>()
        )
    }
}

/// Render the persistent keybar at the bottom of the screen.
pub fn render_keybar(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    let state = ctx.state;
    let accent = ctx.view.active_blade.accent();

    let keys = [
        ("←/→", "blade"),
        ("1-5", "jump"),
        ("↑↓/jk", "scroll"),
        ("⏎", "drill"),
        ("a/r/m", "act"),
        ("o", "open"),
        ("c", "config"),
        ("R", "refresh"),
        ("/", "search"),
        ("?", "help"),
        ("q", "quit"),
    ];

    let mut spans = vec![];
    let mut used_width = 0usize;
    for (i, (key, action)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default().fg(SURFACE0)));
            used_width += 2;
        }
        spans.push(Span::styled(
            *key,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {}", action),
            Style::default().fg(SUBTEXT0).add_modifier(Modifier::DIM),
        ));
        used_width += key.chars().count() + 1 + action.chars().count();
    }

    if state.loading {
        spans.push(Span::styled("  ", Style::default().fg(SURFACE0).bg(BASE)));
        spans.push(Span::styled(
            "refresh…",
            Style::default()
                .fg(PENDING)
                .bg(BASE)
                .add_modifier(Modifier::BOLD),
        ));
        used_width += 2 + "refresh…".chars().count();
    }

    // NOTE: error messages are rendered as a centered InlineToast overlay over
    // the body by the render loop; the keybar always shows the binding list.

    // Push the current time to the right edge.
    let time = now_time();
    let time_block = format!(" {} ", time);
    let gap = area
        .width
        .saturating_sub((used_width + time_block.chars().count()) as u16)
        .max(1) as usize;
    spans.push(Span::styled(" ".repeat(gap), Style::default()));
    spans.push(Span::styled(
        time_block,
        Style::default().fg(OVERLAY0).add_modifier(Modifier::DIM),
    ));

    f.render_widget(
        Paragraph::new(Line::from(spans))
            .style(Style::default().bg(BASE))
            .alignment(Alignment::Left),
        area,
    );
}

fn now_time() -> String {
    Local::now().format("%H:%M").to_string()
}
