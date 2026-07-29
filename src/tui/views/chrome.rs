use chrono::{DateTime, Local, Utc};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::render::component::RenderContext;
use crate::tui::render::theme::{
    Blade, BASE, FAIL, ICON_PR, OVERLAY0, PENDING, SUBTEXT0, SURFACE0, TEXT,
};

/// Render the top tab line: one entry per blade on the left, a right-aligned
/// refresh spinner, data-age indicator, and clock. The active tab is bold in
/// its blade accent; inactive tabs are dim, prefixed with their `1-5` jump key.
pub fn render_tab_line(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    let active = ctx.view.active_blade;

    let mut spans = vec![Span::styled("  ", Style::default().bg(BASE))];
    let mut used = 2usize;
    for i in 0..Blade::count() {
        let blade = Blade::from_index(i);
        if i > 0 {
            spans.push(Span::styled("   ", Style::default().bg(BASE)));
            used += 3;
        }
        if blade == active {
            let name = blade.name();
            used += name.chars().count();
            spans.push(Span::styled(
                name,
                Style::default()
                    .fg(blade.accent())
                    .bg(BASE)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            // The leading digit doubles as the `1-5` jump-key hint.
            let label = format!("{} {}", i + 1, blade.name());
            used += label.chars().count();
            spans.push(Span::styled(label, Style::default().fg(OVERLAY0).bg(BASE)));
        }
    }

    let (meta_spans, meta_width) = tab_meta_spans(ctx.state);
    let gap = area.width.saturating_sub((used + meta_width) as u16).max(1) as usize;
    spans.push(Span::styled(" ".repeat(gap), Style::default().bg(BASE)));
    spans.extend(meta_spans);

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(BASE)),
        area,
    );
}

/// Build the right-hand meta for the tab line (refresh spinner + data age +
/// clock). Returns the spans and the character width they consume.
fn tab_meta_spans(state: &crate::tui::app::AppState) -> (Vec<Span<'static>>, usize) {
    let mut spans = Vec::new();
    let mut width = 0usize;

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
    let age_color = stale_age_color(age);

    if refresh_active {
        let spinner = refresh_spinner(state.ui_tick);
        spans.push(Span::styled(
            spinner.to_string(),
            Style::default()
                .fg(PENDING)
                .bg(BASE)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {}", age_label),
            Style::default()
                .fg(age_color)
                .bg(BASE)
                .add_modifier(Modifier::DIM),
        ));
        width += spinner.chars().count() + 1 + age_label.chars().count();
    } else {
        spans.push(Span::styled(
            age_label.clone(),
            Style::default()
                .fg(age_color)
                .bg(BASE)
                .add_modifier(Modifier::DIM),
        ));
        width += age_label.chars().count();
    }

    let time_block = format!("  {} ", now_time());
    width += time_block.chars().count();
    spans.push(Span::styled(
        time_block,
        Style::default()
            .fg(OVERLAY0)
            .bg(BASE)
            .add_modifier(Modifier::DIM),
    ));

    (spans, width)
}

/// Escalate the data-age color from fresh (<5m) through stale (<15m) to old.
fn stale_age_color(age: Option<chrono::Duration>) -> Color {
    age.map(|d| {
        let secs = d.num_seconds();
        if secs < 300 {
            SUBTEXT0
        } else if secs < 900 {
            PENDING
        } else {
            FAIL
        }
    })
    .unwrap_or(OVERLAY0)
}

/// Render the statusline just above the keybar:
/// `❯ #<number> <title> · <blade> [· <review> · <merge> · <CI>]`.
/// The number and title come from the loaded detail when available, and
/// otherwise from the selected summary in the list, so the grammar stays stable
/// while the detail fetch is in flight. The review/merge/CI status chips mirror
/// the Overview status row so PR health is visible from every blade; they are
/// dropped entirely on narrow terminals (see [`command_line_budget`]).
pub fn render_command_line(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    let state = ctx.state;
    let accent = ctx.view.active_blade.accent();

    let title = if let Some(detail) = state.pr_detail.as_ref() {
        format!("{} #{} {}", ICON_PR, detail.number, detail.title)
    } else if let Some(summary) = selected_summary(state) {
        format!("{} #{} {}", ICON_PR, summary.number, summary.title)
    } else {
        crate::daemon::SERVICE_NAME.to_string()
    };

    let chips: Vec<(String, Color)> = state
        .pr_detail
        .as_ref()
        .map(crate::tui::views::overview::status_chip_data)
        .unwrap_or_default();

    let spans = command_line_spans(
        &title,
        ctx.view.active_blade.name(),
        accent,
        &chips,
        area.width,
    );

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(BASE)),
        area,
    );
}

/// Pure width budget for the command line: chips reserve their width first,
/// the title truncates to the remainder, and chips are dropped entirely when
/// `area_width < 60`. Returns `(show_chips, max_title_width)`.
fn command_line_budget(
    area_width: u16,
    blade_suffix_width: usize,
    chips: &[(String, Color)],
) -> (bool, usize) {
    // "❯ " prefix + blade suffix + a trailing space.
    let overhead = 2 + blade_suffix_width + 1;
    let chips_width: usize = chips.iter().map(|(t, _)| 3 + t.chars().count()).sum();
    let show_chips = area_width >= 60 && !chips.is_empty();
    let reserved = overhead + if show_chips { chips_width } else { 0 };
    let max_title = (area_width as usize).saturating_sub(reserved).max(1);
    (show_chips, max_title)
}

/// Build the command-line spans from resolved inputs. Pure so the chip
/// appending/dropping behavior is unit-testable.
fn command_line_spans(
    title: &str,
    blade_name: &str,
    accent: Color,
    chips: &[(String, Color)],
    width: u16,
) -> Vec<Span<'static>> {
    let blade_suffix = format!(" · {}", blade_name);
    let (show_chips, max_title_width) =
        command_line_budget(width, blade_suffix.chars().count(), chips);
    let display_title = crate::tui::views::text::truncate_to_display_width(title, max_title_width);

    let mut spans = vec![
        Span::styled("❯ ", Style::default().fg(TEXT).add_modifier(Modifier::DIM)),
        Span::styled(
            display_title,
            Style::default().fg(TEXT).add_modifier(Modifier::DIM),
        ),
        Span::styled(blade_suffix, Style::default().fg(accent)),
    ];
    if show_chips {
        for (text, color) in chips {
            spans.push(Span::styled(" · ", Style::default().fg(OVERLAY0).bg(BASE)));
            spans.push(Span::styled(
                text.clone(),
                Style::default()
                    .fg(*color)
                    .bg(BASE)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    spans
}

/// Find the currently selected PR in the list snapshot the inbox renders from.
fn selected_summary(state: &crate::tui::app::AppState) -> Option<&crate::api::PrSummary> {
    let id = state.selected_pr_id.as_ref()?;
    state.prs.groups.values().flatten().find(|p| &p.id == id)
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

/// Return a single-character spinner frame for the current animation tick.
fn refresh_spinner(tick: u64) -> &'static str {
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[(tick as usize) % FRAMES.len()]
}

/// Render the persistent keybar at the bottom of the screen. The bindings are
/// contextual: config-specific while the config overlay is open, otherwise
/// per-blade for the active blade.
pub fn render_keybar(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    let accent = ctx.view.active_blade.accent();

    let keys: &[(&str, &str)] = if ctx.state.show_config_view {
        &[
            ("j/k", "scroll"),
            ("R", "reload"),
            ("c/esc", "close"),
            ("q", "quit"),
        ]
    } else {
        blade_keys(ctx.view.active_blade)
    };

    let mut spans = Vec::new();
    for (i, (key, action)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default().fg(SURFACE0).bg(BASE)));
        }
        spans.push(Span::styled(
            *key,
            Style::default()
                .fg(accent)
                .bg(BASE)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {}", action),
            Style::default()
                .fg(SUBTEXT0)
                .bg(BASE)
                .add_modifier(Modifier::DIM),
        ));
    }

    // NOTE: error messages are rendered as a centered InlineToast overlay over
    // the body by the render loop; the keybar always shows the binding list.
    f.render_widget(
        Paragraph::new(Line::from(spans))
            .style(Style::default().bg(BASE))
            .alignment(Alignment::Left),
        area,
    );
}

/// Blade-specific key hints. Some bindings (g/G outside diff, `r` refresh)
/// are documented ahead of the handlers that implement them.
fn blade_keys(blade: Blade) -> &'static [(&'static str, &'static str)] {
    match blade {
        Blade::Inbox => &[
            ("j/k", "move"),
            ("g/G", "top/bot"),
            ("space", "fold"),
            ("⏎", "open"),
            ("/", "filter"),
            ("o", "browser"),
            ("^y", "copy branch"),
            ("r", "refresh"),
            ("c", "config"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Blade::Overview => &[
            ("j/k", "scroll"),
            ("tab", "section"),
            ("←/→", "blade"),
            ("o", "browser"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Blade::Activity => &[
            ("j/k", "event"),
            ("g/G", "top/bot"),
            ("⏎/space", "expand"),
            ("y", "copy comment"),
            ("o", "open event"),
            ("←/→", "blade"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Blade::Files => &[
            ("j/k", "move"),
            ("⏎", "diff"),
            ("←/→", "blade"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Blade::Diff => &[
            ("j/k", "scroll"),
            ("g/G", "top/bot"),
            ("n", "numbers"),
            ("tab", "file"),
            ("^d/^u", "page"),
            ("?", "help"),
            ("q", "quit"),
        ],
    }
}

fn now_time() -> String {
    Local::now().format("%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_spinner_cycles_through_frames() {
        let first = refresh_spinner(0);
        let second = refresh_spinner(1);
        let wrap = refresh_spinner(10);
        assert!(!first.is_empty());
        assert_ne!(first, second);
        assert_eq!(first, wrap);
    }

    #[test]
    fn stale_age_color_escalates_with_age() {
        use chrono::Duration;
        assert_eq!(stale_age_color(None), OVERLAY0);
        assert_eq!(stale_age_color(Some(Duration::seconds(10))), SUBTEXT0);
        assert_eq!(stale_age_color(Some(Duration::seconds(299))), SUBTEXT0);
        assert_eq!(stale_age_color(Some(Duration::seconds(300))), PENDING);
        assert_eq!(stale_age_color(Some(Duration::seconds(899))), PENDING);
        assert_eq!(stale_age_color(Some(Duration::seconds(900))), FAIL);
    }

    #[test]
    fn blade_keys_cover_every_blade() {
        for i in 0..Blade::count() {
            assert!(!blade_keys(Blade::from_index(i)).is_empty());
        }
    }

    #[test]
    fn keybar_activity_includes_event_actions() {
        let keys = blade_keys(Blade::Activity);
        let find = |key: &str| keys.iter().find(|(k, _)| *k == key).map(|(_, a)| *a);
        assert_eq!(find("⏎/space"), Some("expand"));
        assert_eq!(find("y"), Some("copy comment"));
        assert_eq!(find("o"), Some("open event"));
    }

    #[test]
    fn keybar_overview_no_longer_lists_d() {
        assert!(!blade_keys(Blade::Overview).iter().any(|(k, _)| *k == "d"));
    }

    fn sample_chips() -> Vec<(String, Color)> {
        vec![
            ("approved".to_string(), Color::Green),
            ("mergeable".to_string(), Color::Green),
            ("checks ✓".to_string(), Color::Green),
        ]
    }

    #[test]
    fn command_line_appends_status_chips_when_wide() {
        let spans = command_line_spans(
            "❖ #1 Test PR",
            "OVERVIEW",
            Color::Cyan,
            &sample_chips(),
            120,
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("approved"));
        assert!(text.contains("mergeable"));
        assert!(text.contains("checks ✓"));
    }

    #[test]
    fn command_line_drops_chips_when_narrow() {
        // Below the 60-col threshold the chips vanish and the title keeps
        // the full remaining budget.
        let (show, max_title) = command_line_budget(59, 11, &sample_chips());
        assert!(!show);
        assert_eq!(max_title, 59 - (2 + 11 + 1));

        let spans =
            command_line_spans("❖ #1 Test PR", "OVERVIEW", Color::Cyan, &sample_chips(), 59);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains("approved"));

        // At or above the threshold the chips reserve width first.
        let (show, max_title) = command_line_budget(80, 11, &sample_chips());
        assert!(show);
        let chips_width = sample_chips()
            .iter()
            .map(|(t, _)| 3 + t.chars().count())
            .sum::<usize>();
        assert_eq!(max_title, 80 - (2 + 11 + 1) - chips_width);
    }

    #[test]
    fn command_line_without_chips_matches_old_budget() {
        let (show, max_title) = command_line_budget(120, 11, &[]);
        assert!(!show, "no detail loaded ⇒ no chips even when wide");
        assert_eq!(max_title, 120 - (2 + 11 + 1));
    }
}
