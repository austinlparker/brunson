use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::tui::render::component::RenderContext;
use crate::tui::render::layout::fill;
use crate::tui::render::primitives::ScrollViewport;
use crate::tui::render::theme::{ACTIVITY, ADD, DRAFT, FAIL, MANTLE, MUTED, OVERVIEW, PASS};

pub fn render_activity(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    fill(f, area, MANTLE);
    let lines = &ctx.state.render_cache.activity_lines;
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

pub(crate) fn timeline_verb(event_type: &str) -> (&'static str, Color) {
    match event_type {
        "opened" => ("opened", ADD),
        "comment" => ("commented", ACTIVITY),
        "review" => ("reviewed", PASS),
        "commit" => ("pushed", OVERVIEW),
        "force_push" => ("force-pushed", FAIL),
        "ready_for_review" => ("readied", ADD),
        "review_requested" => ("requested", DRAFT),
        "merged" => ("merged", ADD),
        "closed" => ("closed", FAIL),
        "reopened" => ("reopened", ADD),
        "approved" => ("approved", PASS),
        "changes_requested" => ("changes-requested", FAIL),
        _ => ("acted", MUTED),
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
