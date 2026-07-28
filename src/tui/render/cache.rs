use ratatui::text::Line;
use std::collections::HashMap;

use crate::api::{PrDetailResponse, ReviewCommentDto};
use crate::diff::render::{parse_diff, ParsedDiffLine};
use crate::tui::views::markdown::markdown_lines;

/// Identity for a PR detail: enough to tell whether the *content* a section
/// depends on changed, without comparing (or cloning) the content itself.
/// `id` alone isn't enough because a caller can pass `None` (no PR selected)
/// between two different PRs; folding that into an empty-string sentinel
/// keeps every key `Default`-constructible and comparable.
fn detail_identity(detail: Option<&PrDetailResponse>) -> (String, String) {
    match detail {
        Some(d) => (d.id.clone(), d.updated_at.clone()),
        None => (String::new(), String::new()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ActivityKey {
    id: String,
    updated_at: String,
    width: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct OverviewKey {
    id: String,
    updated_at: String,
    // The LLM classify path can refetch a detail with the same `updated_at`
    // but a newly populated summary, so both must be part of the key.
    rich_generated_at: Option<chrono::DateTime<chrono::Utc>>,
    has_summary: bool,
    width: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct DiffKey {
    id: String,
    updated_at: String,
    // The diff text for a given (pr, updated_at) is stable, so its length is
    // enough to distinguish "not loaded yet" from "loaded" without cloning
    // the whole diff string into the key.
    diff_len: Option<usize>,
    width: u16,
    show_line_numbers: bool,
}

/// Cached render artifacts so markdown/diff string parsing happens only when
/// the data it was built from actually changes. Each section is keyed on the
/// identity (PR id/`updated_at`, plus whatever else determines its output —
/// see the `*Key` structs) it was last built from; a `rebuild_*` call is a
/// no-op unless that key differs from the one on file. Callers no longer
/// need to remember to clear the cache on PR change — passing `None`/a new
/// detail naturally changes the key and triggers a rebuild.
#[derive(Debug, Default, Clone)]
pub struct RenderCache {
    /// Flattened activity timeline lines at the width used to build them.
    pub activity_lines: Vec<Line<'static>>,
    activity_key: Option<ActivityKey>,

    /// Flattened diff display lines (one diff row + inline comments).
    pub diff_lines: Vec<Line<'static>>,
    diff_key: Option<DiffKey>,

    /// File-boundary indices into `diff_lines` (rendered/wrapped space), in
    /// the same order as `find_file_boundaries` over the parsed diff. Diff
    /// lines can wrap into multiple rendered rows, so these do not line up
    /// with `find_file_boundaries` indices into the raw parsed diff.
    pub diff_file_boundaries: Vec<usize>,

    /// For each logical (parsed) diff line, the rendered-row index at which
    /// it begins. Lets callers map a scroll offset (which lives in wrapped
    /// row space) back to a stable logical line number for display — e.g.
    /// the "line N / M" indicator shouldn't change just because the
    /// terminal got narrower and lines wrapped differently.
    pub diff_line_starts: Vec<usize>,

    /// Overview section body lines, keyed by section.
    pub overview_summary: Vec<Line<'static>>,
    pub overview_description: Vec<Line<'static>>,
    pub overview_checks: Vec<Line<'static>>,
    overview_key: Option<OverviewKey>,

    /// Diff comments mapped by line index into the original parsed diff.
    pub diff_comments: HashMap<usize, Vec<ReviewCommentDto>>,
}

impl RenderCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild activity cache if `detail`'s identity or `width` changed.
    pub fn rebuild_activity(&mut self, detail: Option<&PrDetailResponse>, width: u16) {
        let (id, updated_at) = detail_identity(detail);
        let key = ActivityKey { id, updated_at, width };
        if self.activity_key.as_ref() == Some(&key) {
            return;
        }
        self.activity_lines = build_activity_lines(detail, width as usize);
        self.activity_key = Some(key);
    }

    /// Rebuild diff cache if `detail`'s identity, the diff text, `width`, or
    /// `show_line_numbers` changed.
    pub fn rebuild_diff(
        &mut self,
        detail: Option<&PrDetailResponse>,
        diff_text: Option<&str>,
        width: u16,
        show_line_numbers: bool,
    ) {
        let (id, updated_at) = detail_identity(detail);
        let key = DiffKey {
            id,
            updated_at,
            diff_len: diff_text.map(str::len),
            width,
            show_line_numbers,
        };
        if self.diff_key.as_ref() == Some(&key) {
            return;
        }
        self.diff_lines.clear();
        self.diff_comments.clear();
        self.diff_file_boundaries.clear();
        self.diff_line_starts.clear();

        let Some(diff_text) = diff_text else {
            self.diff_key = Some(key);
            return;
        };
        let parsed = parse_diff(diff_text);
        if let Some(d) = detail {
            self.diff_comments =
                crate::diff::render::map_review_threads_to_diff_indices(&d.review_threads, &parsed);
        }
        let (lines, rendered_offsets) = build_diff_lines(
            &parsed,
            width as usize,
            show_line_numbers,
            &self.diff_comments,
        );
        self.diff_lines = lines;
        self.diff_file_boundaries = crate::diff::render::find_file_boundaries(&parsed)
            .into_iter()
            .map(|parsed_idx| rendered_offsets[parsed_idx])
            .collect();
        self.diff_line_starts = rendered_offsets;
        self.diff_key = Some(key);
    }

    /// Rebuild overview caches if `detail`'s identity (including whether an
    /// LLM summary has since arrived) or `description_width` changed.
    pub fn rebuild_overview(&mut self, detail: Option<&PrDetailResponse>, description_width: u16) {
        let (id, updated_at) = detail_identity(detail);
        let key = OverviewKey {
            id,
            updated_at,
            rich_generated_at: detail
                .and_then(|d| d.llm_rich_summary.as_ref())
                .map(|r| r.generated_at),
            has_summary: detail.is_some_and(|d| d.llm_summary.is_some()),
            width: description_width,
        };
        if self.overview_key.as_ref() == Some(&key) {
            return;
        }
        self.overview_summary = build_summary_lines(detail, description_width as usize);
        self.overview_description = build_description_lines(detail, description_width as usize);
        self.overview_checks = build_checks_lines(detail);
        self.overview_key = Some(key);
    }
}

fn build_activity_lines(
    detail: Option<&PrDetailResponse>,
    width: usize,
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
            const MAX_BODY_LINES: usize = 16;
            for md_line in markdown_lines(&body, body_width)
                .into_iter()
                .take(MAX_BODY_LINES)
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

fn build_summary_lines(
    detail: Option<&PrDetailResponse>,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(d) = detail else { return Vec::new() };

    // Prefer the richer catch-up / next-steps summary if available.
    if let Some(rich) = d.llm_rich_summary.as_ref() {
        let mut out = Vec::new();
        if !rich.one_line.is_empty() {
            out.push(Line::from(vec![Span::styled(
                rich.one_line.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )]));
        }
        if !rich.catch_up.is_empty() {
            out.push(Line::from(vec![Span::styled(
                "Catch up".to_string(),
                Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )]));
            out.extend(markdown_lines(&rich.catch_up, width));
        }
        if !rich.next_steps.is_empty() {
            out.push(Line::from(vec![Span::styled(
                "Next steps".to_string(),
                Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )]));
            out.extend(markdown_lines(&rich.next_steps, width));
        }
        return out;
    }

    let Some(summary) = d.llm_summary.as_ref() else {
        return Vec::new();
    };
    markdown_lines(summary, width)
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
    use crate::github::types::CheckStatus;

    let Some(d) = detail else {
        return vec![Line::from("No PR selected")];
    };

    let mut lines = Vec::new();
    let (check_status_label, check_status_color) = match d.check_status {
        CheckStatus::Success => ("success", PASS),
        CheckStatus::Failure => ("failure", FAIL),
        CheckStatus::Pending => ("pending", PENDING),
        CheckStatus::Neutral => ("neutral", MUTED),
        CheckStatus::None => ("none", MUTED),
    };
    lines.push(Line::from(vec![Span::styled(
        format!("({})", check_status_label),
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

/// Builds rendered diff lines, plus for each parsed line its starting index
/// in the rendered (possibly wrapped) output. `rendered_offsets[i]` is the
/// rendered-line index at which parsed line `i` begins.
fn build_diff_lines(
    parsed: &[ParsedDiffLine],
    width: usize,
    show_line_numbers: bool,
    comments: &HashMap<usize, Vec<ReviewCommentDto>>,
) -> (Vec<Line<'static>>, Vec<usize>) {
    use crate::tui::views::diff::{render_diff_line_internal, render_inline_comments_internal};

    let mut lines = Vec::new();
    let mut rendered_offsets = Vec::with_capacity(parsed.len());
    if parsed.is_empty() {
        return (lines, rendered_offsets);
    }
    let prefix_cols = if show_line_numbers {
        " 9999 9999 │ + ".chars().count()
    } else {
        "+ ".chars().count()
    };
    let max_content_width = width.saturating_sub(prefix_cols);
    let usable_width = width;

    for (idx, parsed_line) in parsed.iter().enumerate() {
        rendered_offsets.push(lines.len());
        lines.extend(render_diff_line_internal(
            parsed_line,
            show_line_numbers,
            max_content_width,
        ));
        if let Some(comments) = comments.get(&idx) {
            lines.extend(render_inline_comments_internal(comments, usable_width));
        }
    }
    (lines, rendered_offsets)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression test: a diff line that wraps into multiple rendered rows
    // must push later files' boundaries forward in rendered space, not just
    // in parsed-line space (see src/tui/app.rs's diff scroll/jump helpers).
    #[test]
    fn diff_file_boundaries_account_for_wrapped_lines() {
        let long_line = "x".repeat(200);
        let diff_text = format!(
            "diff --git a/one.rs b/one.rs\n@@ -1,1 +1,1 @@\n+{}\ndiff --git a/two.rs b/two.rs\n@@ -1,1 +1,1 @@\n+short\n",
            long_line
        );

        let mut cache = RenderCache::new();
        cache.rebuild_diff(None, Some(&diff_text), 40, false);

        let parsed = parse_diff(&diff_text);
        let raw_boundaries = crate::diff::render::find_file_boundaries(&parsed);
        assert_eq!(raw_boundaries, vec![0, 3]);

        // The long line wraps into more than one rendered row at width 40, so
        // the second file's rendered-space boundary must be pushed past its
        // raw parsed-line index of 2.
        assert_eq!(cache.diff_file_boundaries.len(), 2);
        assert_eq!(cache.diff_file_boundaries[0], 0);
        assert!(
            cache.diff_file_boundaries[1] > raw_boundaries[1],
            "expected wrapped rendering to push the second file's boundary past {}, got {}",
            raw_boundaries[1],
            cache.diff_file_boundaries[1]
        );
    }

    fn make_detail(id: &str, updated_at: &str) -> PrDetailResponse {
        PrDetailResponse {
            id: id.to_string(),
            node_id: "node".to_string(),
            owner: "org".to_string(),
            repo: "repo".to_string(),
            number: 1,
            title: "Test PR".to_string(),
            body: String::new(),
            url: "https://example.com".to_string(),
            author: "author".to_string(),
            is_draft: false,
            updated_at: updated_at.to_string(),
            head_ref: "feature".to_string(),
            base_ref: "main".to_string(),
            mergeable: crate::github::types::MergeableState::Mergeable,
            review_decision: None,
            review_requests: vec![],
            team_review_requests: vec![],
            viewer_latest_review: None,
            latest_reviews: vec![],
            check_status: crate::github::types::CheckStatus::None,
            checks: vec![],
            review_threads: vec![],
            files: vec![],
            timeline: vec![],
            llm_priority: None,
            llm_summary: None,
            llm_rich_summary: None,
            last_seen_at: None,
        }
    }

    #[test]
    fn rebuild_is_a_noop_when_the_key_is_unchanged() {
        let detail = make_detail("org~repo~1", "2024-01-01T00:00:00Z");
        let mut cache = RenderCache::new();
        cache.rebuild_activity(Some(&detail), 80);
        cache.rebuild_overview(Some(&detail), 80);
        cache.rebuild_diff(Some(&detail), Some("diff --git a/f b/f\n@@ -1 +1 @@\n+x\n"), 80, false);

        // Mutate the cached output directly; if a rebuild with an identical
        // key were to run again it would overwrite this sentinel.
        cache.activity_lines = vec![Line::from("sentinel-activity")];
        cache.overview_summary = vec![Line::from("sentinel-overview")];
        cache.diff_lines = vec![Line::from("sentinel-diff")];

        cache.rebuild_activity(Some(&detail), 80);
        cache.rebuild_overview(Some(&detail), 80);
        cache.rebuild_diff(Some(&detail), Some("diff --git a/f b/f\n@@ -1 +1 @@\n+x\n"), 80, false);

        assert_eq!(cache.activity_lines.len(), 1);
        assert_eq!(cache.activity_lines[0].spans[0].content, "sentinel-activity");
        assert_eq!(cache.overview_summary.len(), 1);
        assert_eq!(cache.overview_summary[0].spans[0].content, "sentinel-overview");
        assert_eq!(cache.diff_lines.len(), 1);
        assert_eq!(cache.diff_lines[0].spans[0].content, "sentinel-diff");
    }

    #[test]
    fn overview_rebuilds_when_llm_summary_arrives_with_unchanged_updated_at() {
        let mut detail = make_detail("org~repo~1", "2024-01-01T00:00:00Z");
        let mut cache = RenderCache::new();
        cache.rebuild_overview(Some(&detail), 80);
        assert!(
            cache.overview_summary.is_empty(),
            "no summary yet, so overview_summary should be empty"
        );

        // The daemon classifies the PR and the detail refetch has the same
        // `updated_at` but a newly populated `llm_summary`.
        detail.llm_summary = Some("A concise summary.".to_string());
        cache.rebuild_overview(Some(&detail), 80);

        assert!(
            !cache.overview_summary.is_empty(),
            "a newly-arrived llm_summary at unchanged updated_at must trigger a rebuild"
        );
    }

    #[test]
    fn overview_does_not_rebuild_every_call_when_content_is_legitimately_empty() {
        // A PR with no LLM summary and no body: `overview_summary` is `[]`
        // both before and after, but the rebuild must still be recognized as
        // a no-op via the key, not re-run `markdown_lines` every frame.
        let detail = make_detail("org~repo~1", "2024-01-01T00:00:00Z");
        let mut cache = RenderCache::new();
        cache.rebuild_overview(Some(&detail), 80);
        assert!(cache.overview_summary.is_empty());
        assert!(cache.overview_description.is_empty());

        // Poison the checks section (unrelated to summary/description) to
        // prove the second call is a no-op rather than an "empty so rebuild
        // anyway" fallback.
        cache.overview_checks = vec![Line::from("sentinel-checks")];
        cache.rebuild_overview(Some(&detail), 80);

        assert_eq!(cache.overview_checks.len(), 1);
        assert_eq!(cache.overview_checks[0].spans[0].content, "sentinel-checks");
    }
}
