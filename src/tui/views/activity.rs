use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::github::types::TimelineEventType;
use crate::tui::render::component::RenderContext;
use crate::tui::render::layout::fill;
use crate::tui::render::primitives::ScrollViewport;
use crate::tui::render::theme::{ACTIVITY, ADD, DRAFT, FAIL, MANTLE, MUTED, OVERVIEW, PASS};

pub fn render_activity(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    fill(f, area, MANTLE);
    // Flattened in `ViewStateManager::prepare` (`flatten_activity_events`)
    // from the cached structured events, with selection/collapse applied.
    let lines = &ctx.view.activity_display_lines;
    if lines.is_empty() {
        f.render_widget(
            ratatui::widgets::Paragraph::new(Line::from(vec![Span::styled(
                "No activity",
                Style::default().fg(MUTED).bg(MANTLE),
            )]))
            .style(Style::default().bg(MANTLE)),
            area,
        );
        return;
    }

    ScrollViewport::new(lines, ctx.view.activity_scroll.offset)
        .style(
            Style::default()
                .fg(crate::tui::render::theme::TEXT)
                .bg(MANTLE),
        )
        .render(f, area);
}

pub(crate) fn timeline_verb(event_type: &TimelineEventType) -> (&'static str, Color) {
    match event_type {
        TimelineEventType::Comment => ("commented", ACTIVITY),
        TimelineEventType::Review => ("reviewed", PASS),
        TimelineEventType::Commit => ("pushed", OVERVIEW),
        TimelineEventType::ForcePush => ("force-pushed", FAIL),
        TimelineEventType::ReadyForReview => ("readied", ADD),
        TimelineEventType::ReviewRequested => ("requested", DRAFT),
        TimelineEventType::Merged => ("merged", ADD),
        TimelineEventType::Closed => ("closed", FAIL),
        TimelineEventType::Reopened => ("reopened", ADD),
        // No daemon code emits `opened`/`approved`/`changes_requested` as a
        // timeline event type (those are represented via `Review`/other
        // events), so `Other` is the only remaining case.
        TimelineEventType::Other => ("acted", MUTED),
    }
}

pub(crate) fn event_icon(verb: &str) -> &'static str {
    use crate::tui::render::theme::*;
    match verb {
        "opened" => ICON_PR,
        "commented" => ICON_COMMENT,
        "reviewed" => ICON_EYE,
        "pushed" => ICON_COMMIT,
        "force-pushed" => ICON_FORCE_PUSH,
        "readied" => ICON_ROCKET,
        "requested" => ICON_PERSON_ADD,
        "merged" => ICON_MERGE,
        "closed" => ICON_PR_CLOSED,
        "reopened" => ICON_REOPEN,
        "approved" => ICON_CHECK,
        "changes-requested" => ICON_REQUEST_CHANGES,
        _ => ICON_DASH,
    }
}

/// Shorten an ISO 8601 timestamp to `MM-DD HH:MM` or return as-is.
pub(crate) fn short_time(iso: &str) -> String {
    if iso.len() >= 16 {
        format!("{} {}", &iso[5..10], &iso[11..16])
    } else {
        iso.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_time() {
        assert_eq!(short_time("2024-05-20T12:34:56Z"), "05-20 12:34");
        assert_eq!(short_time("now"), "now");
    }
}
