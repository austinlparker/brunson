use ratatui::text::Line;
use std::collections::HashMap;

use crate::api::{PrDetailResponse, ReviewCommentDto};
use crate::diff::render::{parse_diff, ParsedDiffLine};
use crate::tui::views::markdown::markdown_lines;

/// Cached render artifacts so markdown/diff string parsing happens only when
/// data or allocated width changes.
#[derive(Debug, Default, Clone)]
pub struct RenderCache {
    /// Flattened activity timeline lines at the width used to build them.
    pub activity_lines: Vec<Line<'static>>,
    pub activity_width: u16,

    /// Flattened diff display lines (one diff row + inline comments).
    pub diff_lines: Vec<Line<'static>>,
    pub diff_width: u16,
    pub diff_show_line_numbers: bool,

    /// Overview section body lines, keyed by section.
    pub overview_summary: Vec<Line<'static>>,
    pub overview_description: Vec<Line<'static>>,
    pub overview_checks: Vec<Line<'static>>,
    pub overview_description_width: u16,

    /// Diff comments mapped by line index into the original parsed diff.
    pub diff_comments: HashMap<usize, Vec<ReviewCommentDto>>,
}

impl RenderCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop all cached content. Called when the selected PR changes.
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Rebuild activity cache if detail or width changed.
    pub fn rebuild_activity(
        &mut self,
        detail: Option<&PrDetailResponse>,
        width: u16,
        max_body_lines: usize,
    ) {
        if self.activity_width == width && !self.activity_lines.is_empty() {
            // Detail identity is implicitly guarded by callers clearing the cache on PR change.
            return;
        }
        self.activity_lines = build_activity_lines(detail, width as usize, max_body_lines);
        self.activity_width = width;
    }

    /// Rebuild diff cache if diff text or width changed.
    pub fn rebuild_diff(
        &mut self,
        detail: Option<&PrDetailResponse>,
        diff_text: Option<&str>,
        width: u16,
        show_line_numbers: bool,
    ) {
        if self.diff_width == width
            && self.diff_show_line_numbers == show_line_numbers
            && !self.diff_lines.is_empty()
        {
            return;
        }
        self.diff_lines.clear();
        self.diff_comments.clear();

        let Some(diff_text) = diff_text else { return };
        let parsed = parse_diff(diff_text);
        if let Some(d) = detail {
            self.diff_comments =
                crate::diff::render::map_review_threads_to_diff_indices(&d.review_threads, &parsed);
        }
        self.diff_lines = build_diff_lines(
            &parsed,
            width as usize,
            show_line_numbers,
            &self.diff_comments,
        );
        self.diff_width = width;
        self.diff_show_line_numbers = show_line_numbers;
    }

    /// Rebuild overview caches if detail or width changed.
    pub fn rebuild_overview(&mut self, detail: Option<&PrDetailResponse>, description_width: u16) {
        if self.overview_description_width == description_width && !self.overview_summary.is_empty()
        {
            return;
        }
        self.overview_summary = build_summary_lines(detail);
        self.overview_description = build_description_lines(detail, description_width as usize);
        self.overview_checks = build_checks_lines(detail);
        self.overview_description_width = description_width;
    }
}

fn build_activity_lines(
    detail: Option<&PrDetailResponse>,
    width: usize,
    max_body_lines: usize,
) -> Vec<Line<'static>> {
    let Some(d) = detail else {
        return vec![Line::from("No activity")];
    };

    let mut events = Vec::new();
    for event in &d.timeline {
        let verb = crate::tui::views::activity::timeline_verb(&event.event_type);
        events.push((
            event.actor.clone(),
            verb.0.to_string(),
            verb.1,
            crate::tui::views::activity::short_time(&event.created_at),
            event.detail.clone(),
        ));
    }
    for thread in &d.review_threads {
        if let Some(comment) = thread.comments.first() {
            events.push((
                comment.author.clone(),
                "reviewed".to_string(),
                super::theme::ACTIVITY,
                "review".to_string(),
                comment.body.clone(),
            ));
        }
    }
    events.sort_by(|a, b| b.3.cmp(&a.3));

    if events.is_empty() {
        return vec![Line::from("No activity")];
    }

    let indent = "   ";
    let body_width = width.saturating_sub(indent.len());
    let mut out = Vec::new();
    for (actor, verb, color, when, body) in events {
        let icon = crate::tui::views::activity::event_icon(&verb);
        out.push(Line::from(vec![
            Span::styled(format!("{} ", icon), Style::default().fg(color)),
            Span::styled(format!("@{} ", actor), Style::default().fg(SUBTEXT0)),
            Span::styled(
                verb,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", when), Style::default().fg(OVERLAY0)),
        ]));
        if !body.is_empty() {
            for md_line in markdown_lines(&body, body_width)
                .into_iter()
                .take(max_body_lines)
            {
                let mut spans = vec![Span::styled(indent, Style::default())];
                spans.extend(md_line.spans);
                out.push(Line::from(spans));
            }
        }
    }
    out
}

use super::theme::{OVERLAY0, SUBTEXT0};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

fn build_summary_lines(detail: Option<&PrDetailResponse>) -> Vec<Line<'static>> {
    let Some(d) = detail else { return Vec::new() };
    d.llm_summary
        .as_ref()
        .map(|s| s.lines().map(|l| Line::from(l.to_string())).collect())
        .unwrap_or_default()
}

fn build_description_lines(detail: Option<&PrDetailResponse>, width: usize) -> Vec<Line<'static>> {
    let Some(d) = detail else { return Vec::new() };
    if d.body.is_empty() {
        return Vec::new();
    }
    markdown_lines(&d.body, width)
}

fn build_checks_lines(detail: Option<&PrDetailResponse>) -> Vec<Line<'static>> {
    use super::theme::{
        FAIL, ICON_CHECK, ICON_CIRCLE_SLASH, ICON_CLOSE, ICON_SYNC, MUTED, PASS, PENDING, TEXT,
    };

    let Some(d) = detail else {
        return vec![Line::from("No PR selected")];
    };

    let mut lines = Vec::new();
    let check_status_color = match d.check_status.as_str() {
        "success" => PASS,
        "failure" => FAIL,
        "pending" => PENDING,
        _ => MUTED,
    };
    lines.push(Line::from(vec![Span::styled(
        format!("({})", d.check_status),
        Style::default().fg(check_status_color),
    )]));

    if d.checks.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "none",
            Style::default().fg(MUTED),
        )]));
    } else {
        for check in &d.checks {
            let (icon, color) = match (check.status.as_str(), check.conclusion.as_deref()) {
                ("COMPLETED", Some("SUCCESS")) => (ICON_CHECK, PASS),
                ("COMPLETED", Some("FAILURE")) => (ICON_CLOSE, FAIL),
                ("COMPLETED", Some("SKIPPED")) | ("COMPLETED", Some("NEUTRAL")) => {
                    (ICON_CIRCLE_SLASH, MUTED)
                }
                ("IN_PROGRESS", _) | ("QUEUED", _) => (ICON_SYNC, PENDING),
                _ => (ICON_CIRCLE_SLASH, MUTED),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", icon), Style::default().fg(color)),
                Span::styled(check.name.clone(), Style::default().fg(TEXT)),
                Span::styled(
                    format!("  {}", check.conclusion.as_deref().unwrap_or(&check.status)),
                    Style::default().fg(MUTED),
                ),
            ]));
        }
    }
    lines
}

fn build_diff_lines(
    parsed: &[ParsedDiffLine],
    width: usize,
    show_line_numbers: bool,
    comments: &HashMap<usize, Vec<ReviewCommentDto>>,
) -> Vec<Line<'static>> {
    use crate::tui::views::diff::{render_diff_line_internal, render_inline_comments_internal};

    let mut lines = Vec::new();
    if parsed.is_empty() {
        return lines;
    }
    let prefix_cols = if show_line_numbers {
        " 9999 9999 │ + ".chars().count()
    } else {
        "+ ".chars().count()
    };
    let max_content_width = width.saturating_sub(prefix_cols);
    let usable_width = width;

    for (idx, parsed_line) in parsed.iter().enumerate() {
        lines.push(render_diff_line_internal(
            parsed_line,
            show_line_numbers,
            max_content_width,
        ));
        if let Some(comments) = comments.get(&idx) {
            lines.extend(render_inline_comments_internal(comments, usable_width));
        }
    }
    lines
}
