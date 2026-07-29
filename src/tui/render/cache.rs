use ratatui::text::Line;
use std::collections::HashMap;

use crate::api::PrDetailResponse;
use crate::github::types::ReviewComment;
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
    /// Cheap content fingerprint so a refetched detail whose activity content
    /// changed without an `updated_at` bump still rebuilds (same failure mode
    /// `OverviewKey` guards against with `has_summary`).
    fingerprint: (usize, usize, usize, String),
}

/// Fingerprint of the activity-relevant content of a detail: timeline length,
/// thread count, total body byte-length across timeline details and thread
/// comments, and the last timeline timestamp.
fn activity_fingerprint(detail: Option<&PrDetailResponse>) -> (usize, usize, usize, String) {
    match detail {
        Some(d) => (
            d.timeline.len(),
            d.review_threads.len(),
            d.timeline.iter().map(|e| e.detail.len()).sum::<usize>()
                + d.review_threads
                    .iter()
                    .flat_map(|t| &t.comments)
                    .map(|c| c.body.len())
                    .sum::<usize>(),
            d.timeline
                .last()
                .map(|e| e.created_at.clone())
                .unwrap_or_default(),
        ),
        None => (0, 0, 0, String::new()),
    }
}

/// One selectable event in the Activity blade: a timeline event or a whole
/// review thread. Cached immutably — header spans carry only base (unselected)
/// styling; selection styling is applied at flatten time
/// (`flatten_activity_events` in `src/tui/state.rs`).
#[derive(Debug, Clone, Default)]
pub struct ActivityEvent {
    /// `icon @actor verb  time` header spans, base styling only.
    pub header_spans: Vec<Span<'static>>,
    /// Full markdown-rendered body, indented.
    pub body: Vec<Line<'static>>,
    /// Plain-text body for `y` copy.
    pub raw_body: String,
    /// Event web URL; empty ⇒ fall back to the PR URL at action time.
    pub url: String,
    /// Raw ISO timestamp (the sort key); empty for legacy data.
    pub created_at: String,
    /// Login of the primary actor, for the copy confirmation toast.
    pub actor: String,
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
    /// Structured activity events, newest first, at the width used to build
    /// them. Empty when no detail is loaded or the PR has no activity — the
    /// renderer shows the placeholder, not a synthetic event.
    pub activity_events: Vec<ActivityEvent>,
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

    /// Overview section body lines, keyed by section. `overview_problems` is
    /// empty when nothing is wrong (green checks, mergeable), which makes the
    /// PROBLEMS section absent entirely.
    pub overview_summary: Vec<Line<'static>>,
    pub overview_description: Vec<Line<'static>>,
    pub overview_problems: Vec<Line<'static>>,
    overview_key: Option<OverviewKey>,

    /// Diff comments mapped by line index into the original parsed diff.
    pub diff_comments: HashMap<usize, Vec<ReviewComment>>,
}

impl RenderCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild activity cache if `detail`'s identity, content fingerprint, or
    /// `width` changed.
    pub fn rebuild_activity(&mut self, detail: Option<&PrDetailResponse>, width: u16) {
        let (id, updated_at) = detail_identity(detail);
        let key = ActivityKey {
            id,
            updated_at,
            width,
            fingerprint: activity_fingerprint(detail),
        };
        if self.activity_key.as_ref() == Some(&key) {
            return;
        }
        self.activity_events = build_activity_events(detail, width as usize);
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
        self.overview_problems = build_problems_lines(detail);
        self.overview_key = Some(key);
    }
}

/// The indent applied to activity event body lines under their header.
const ACTIVITY_BODY_INDENT: &str = "   ";

/// Format an event timestamp for the header; legacy data with no timestamp
/// renders as an em-dash.
fn activity_when(created_at: &str) -> String {
    if created_at.is_empty() {
        "—".to_string()
    } else {
        crate::tui::views::activity::short_time(created_at)
    }
}

/// Indent markdown-rendered `body` lines for display under an event header.
fn indented_markdown(body: &str, width: usize) -> Vec<Line<'static>> {
    let body_width = width.saturating_sub(ACTIVITY_BODY_INDENT.len());
    markdown_lines(body, body_width)
        .into_iter()
        .map(|md_line| {
            let mut spans = vec![Span::styled(ACTIVITY_BODY_INDENT, Style::default())];
            spans.extend(md_line.spans);
            Line::from(spans)
        })
        .collect()
}

/// Build the structured event list for the Activity blade: timeline events
/// map 1:1, each review thread becomes one event, and everything is sorted
/// by descending real timestamp (raw ISO strings sort lexicographically;
/// events with no timestamp — legacy data — sort last, stable among
/// themselves). Returns `[]` when there is no detail or no activity.
fn build_activity_events(detail: Option<&PrDetailResponse>, width: usize) -> Vec<ActivityEvent> {
    let Some(d) = detail else {
        return Vec::new();
    };

    let mut events: Vec<ActivityEvent> = Vec::new();

    for event in &d.timeline {
        let (verb, color) = crate::tui::views::activity::timeline_verb(&event.event_type);
        let icon = crate::tui::views::activity::event_icon(verb);
        let header_spans = vec![
            Span::styled(format!("{} ", icon), Style::default().fg(color)),
            Span::styled(format!("@{} ", event.actor), Style::default().fg(SUBTEXT0)),
            Span::styled(
                verb.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", activity_when(&event.created_at)),
                Style::default().fg(OVERLAY0),
            ),
        ];
        let body = if event.detail.is_empty() {
            Vec::new()
        } else {
            indented_markdown(&event.detail, width)
        };
        events.push(ActivityEvent {
            header_spans,
            body,
            raw_body: event.detail.clone(),
            url: event.url.clone(),
            created_at: event.created_at.clone(),
            actor: event.actor.clone(),
        });
    }

    for thread in &d.review_threads {
        let Some(first) = thread.comments.first() else {
            continue;
        };
        let color = super::theme::ACTIVITY;
        let icon = crate::tui::views::activity::event_icon("commented");
        let path = if first.path.is_empty() {
            "—"
        } else {
            first.path.as_str()
        };
        let line_label = first
            .line
            .map(|l| l.to_string())
            .unwrap_or_else(|| "—".to_string());
        // Legacy fallbacks: the first *non-empty* timestamp/URL across the
        // thread's comments, else empty (sorts last / falls back to PR URL).
        let created_at = thread
            .comments
            .iter()
            .map(|c| c.created_at.as_str())
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_string();
        let url = thread
            .comments
            .iter()
            .map(|c| c.url.as_str())
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_string();

        let mut header_spans = vec![
            Span::styled(format!("{} ", icon), Style::default().fg(color)),
            Span::styled(format!("@{} ", first.author), Style::default().fg(SUBTEXT0)),
            Span::styled(
                "thread".to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}:{}", path, line_label),
                Style::default().fg(super::theme::MUTED),
            ),
            Span::styled(
                format!("  {}", activity_when(&created_at)),
                Style::default().fg(OVERLAY0),
            ),
        ];
        if thread.is_resolved {
            header_spans.push(Span::styled(
                " (resolved)".to_string(),
                Style::default().fg(super::theme::MUTED),
            ));
        }

        let mut body = Vec::new();
        let mut raw_body = String::new();
        for comment in &thread.comments {
            body.push(Line::from(vec![
                Span::styled(ACTIVITY_BODY_INDENT, Style::default()),
                Span::styled(
                    format!("@{}:", comment.author),
                    Style::default().fg(SUBTEXT0).add_modifier(Modifier::BOLD),
                ),
            ]));
            body.extend(indented_markdown(&comment.body, width));
            if !raw_body.is_empty() {
                raw_body.push('\n');
            }
            raw_body.push_str(&format!("@{}: {}", comment.author, comment.body));
        }

        events.push(ActivityEvent {
            header_spans,
            body,
            raw_body,
            url,
            created_at,
            actor: first.author.clone(),
        });
    }

    // Newest first; empty timestamps last, stable among themselves.
    events.sort_by(
        |a, b| match (a.created_at.is_empty(), b.created_at.is_empty()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => b.created_at.cmp(&a.created_at),
        },
    );

    events
}

use super::theme::{OVERLAY0, SUBTEXT0};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

fn build_summary_lines(detail: Option<&PrDetailResponse>, width: usize) -> Vec<Line<'static>> {
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

/// True when a check entry is success-ish and therefore *not* a problem.
/// Covers both wire shapes: CheckRun entries (`status == "COMPLETED"` plus a
/// conclusion) and legacy StatusContext entries (`parse_checks` maps their
/// `state` — SUCCESS/PENDING/FAILURE/ERROR — into `status` with
/// `conclusion = None`).
fn check_is_success(status: &str, conclusion: Option<&str>) -> bool {
    matches!(
        (status, conclusion),
        ("COMPLETED", Some("SUCCESS" | "SKIPPED" | "NEUTRAL")) | ("SUCCESS", None)
    )
}

/// Build the PROBLEMS section: merge conflicts plus every non-success check
/// (failures, errors, in-progress, queued, legacy pending). Returns `[]` when
/// nothing is wrong, which makes the section absent entirely.
fn build_problems_lines(detail: Option<&PrDetailResponse>) -> Vec<Line<'static>> {
    use super::theme::{FAIL, ICON_CLOSE, ICON_SYNC, MUTED, PENDING, TEXT};
    use crate::github::types::{CheckStatus, MergeableState};

    let Some(d) = detail else {
        return Vec::new();
    };

    let mut lines = Vec::new();

    if d.mergeable == MergeableState::Conflicting {
        lines.push(Line::from(vec![Span::styled(
            format!("{} conflicts with base branch", ICON_CLOSE),
            Style::default().fg(FAIL),
        )]));
    }

    for check in &d.checks {
        if check_is_success(&check.status, check.conclusion.as_deref()) {
            continue;
        }
        let pending = matches!(
            check.status.as_str(),
            "IN_PROGRESS" | "QUEUED" | "PENDING" | "WAITING" | "REQUESTED"
        );
        let (icon, color) = if pending {
            (ICON_SYNC, PENDING)
        } else {
            (ICON_CLOSE, FAIL)
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

    // Legacy/status-context-only data can carry an overall failing/pending
    // status with no per-check entries; surface that so the section isn't
    // misleadingly absent.
    if d.checks.is_empty() {
        match d.check_status {
            CheckStatus::Failure => lines.push(Line::from(vec![Span::styled(
                "checks: failure",
                Style::default().fg(FAIL),
            )])),
            CheckStatus::Pending => lines.push(Line::from(vec![Span::styled(
                "checks: pending",
                Style::default().fg(PENDING),
            )])),
            _ => {}
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
    comments: &HashMap<usize, Vec<ReviewComment>>,
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
        cache.rebuild_diff(
            Some(&detail),
            Some("diff --git a/f b/f\n@@ -1 +1 @@\n+x\n"),
            80,
            false,
        );

        // Mutate the cached output directly; if a rebuild with an identical
        // key were to run again it would overwrite this sentinel.
        cache.activity_events = vec![ActivityEvent {
            raw_body: "sentinel-activity".to_string(),
            ..Default::default()
        }];
        cache.overview_summary = vec![Line::from("sentinel-overview")];
        cache.diff_lines = vec![Line::from("sentinel-diff")];

        cache.rebuild_activity(Some(&detail), 80);
        cache.rebuild_overview(Some(&detail), 80);
        cache.rebuild_diff(
            Some(&detail),
            Some("diff --git a/f b/f\n@@ -1 +1 @@\n+x\n"),
            80,
            false,
        );

        assert_eq!(cache.activity_events.len(), 1);
        assert_eq!(cache.activity_events[0].raw_body, "sentinel-activity");
        assert_eq!(cache.overview_summary.len(), 1);
        assert_eq!(
            cache.overview_summary[0].spans[0].content,
            "sentinel-overview"
        );
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
        cache.overview_problems = vec![Line::from("sentinel-problems")];
        cache.rebuild_overview(Some(&detail), 80);

        assert_eq!(cache.overview_problems.len(), 1);
        assert_eq!(
            cache.overview_problems[0].spans[0].content,
            "sentinel-problems"
        );
    }

    fn timeline_event(actor: &str, created_at: &str, detail: &str) -> crate::github::types::TimelineEvent {
        crate::github::types::TimelineEvent {
            event_type: crate::github::types::TimelineEventType::Comment,
            actor: actor.to_string(),
            created_at: created_at.to_string(),
            detail: detail.to_string(),
            url: String::new(),
        }
    }

    fn thread_comment(author: &str, body: &str, created_at: &str) -> crate::github::types::ReviewComment {
        crate::github::types::ReviewComment {
            author: author.to_string(),
            body: body.to_string(),
            path: "src/lib.rs".to_string(),
            line: Some(7),
            created_at: created_at.to_string(),
            url: String::new(),
        }
    }

    #[test]
    fn activity_events_sorted_by_real_timestamp() {
        let mut detail = make_detail("org~repo~1", "2024-01-03T00:00:00Z");
        detail.timeline = vec![
            timeline_event("early", "2024-01-01T00:00:00Z", "first comment"),
            timeline_event("late", "2024-01-03T00:00:00Z", "third comment"),
        ];
        // Thread timestamp lands between the two timeline events.
        detail.review_threads = vec![crate::github::types::ReviewThread {
            is_resolved: false,
            is_outdated: false,
            comments: vec![thread_comment("mid", "thread body", "2024-01-02T00:00:00Z")],
        }];

        let events = build_activity_events(Some(&detail), 80);
        let actors: Vec<&str> = events.iter().map(|e| e.actor.as_str()).collect();
        assert_eq!(
            actors,
            vec!["late", "mid", "early"],
            "descending real timestamps, threads interleaved"
        );
    }

    #[test]
    fn activity_events_with_empty_timestamps_sort_last() {
        let mut detail = make_detail("org~repo~1", "2024-01-03T00:00:00Z");
        detail.timeline = vec![timeline_event("dated", "2024-01-01T00:00:00Z", "hello")];
        // Legacy data: thread comments carry no created_at.
        detail.review_threads = vec![
            crate::github::types::ReviewThread {
                is_resolved: false,
                is_outdated: false,
                comments: vec![thread_comment("legacy-a", "a", "")],
            },
            crate::github::types::ReviewThread {
                is_resolved: false,
                is_outdated: false,
                comments: vec![thread_comment("legacy-b", "b", "")],
            },
        ];

        let events = build_activity_events(Some(&detail), 80);
        let actors: Vec<&str> = events.iter().map(|e| e.actor.as_str()).collect();
        assert_eq!(
            actors,
            vec!["dated", "legacy-a", "legacy-b"],
            "empty timestamps sort last, stable among themselves"
        );
    }

    #[test]
    fn activity_events_empty_without_detail_or_events() {
        assert!(build_activity_events(None, 80).is_empty());
        let detail = make_detail("org~repo~1", "2024-01-01T00:00:00Z");
        assert!(
            build_activity_events(Some(&detail), 80).is_empty(),
            "no synthetic 'No activity' event"
        );
    }

    #[test]
    fn activity_thread_event_concatenates_comments_and_marks_resolved() {
        let mut detail = make_detail("org~repo~1", "2024-01-01T00:00:00Z");
        detail.review_threads = vec![crate::github::types::ReviewThread {
            is_resolved: true,
            is_outdated: false,
            comments: vec![
                thread_comment("alice", "please fix", "2024-01-01T10:00:00Z"),
                thread_comment("bob", "done!", "2024-01-01T11:00:00Z"),
            ],
        }];

        let events = build_activity_events(Some(&detail), 80);
        assert_eq!(events.len(), 1, "one event per thread");
        let event = &events[0];
        assert_eq!(event.raw_body, "@alice: please fix\n@bob: done!");
        assert_eq!(event.created_at, "2024-01-01T10:00:00Z");
        let header: String = event
            .header_spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header.contains("@alice"));
        assert!(header.contains("src/lib.rs:7"));
        assert!(header.contains("(resolved)"));
    }

    #[test]
    fn activity_rebuilds_when_content_changes_without_updated_at_bump() {
        let mut detail = make_detail("org~repo~1", "2024-01-01T00:00:00Z");
        detail.timeline = vec![timeline_event("a", "2024-01-01T00:00:00Z", "one")];
        let mut cache = RenderCache::new();
        cache.rebuild_activity(Some(&detail), 80);
        assert_eq!(cache.activity_events.len(), 1);

        // Same id + updated_at, but a refetch grew the timeline.
        detail
            .timeline
            .push(timeline_event("b", "2024-01-02T00:00:00Z", "two"));
        cache.rebuild_activity(Some(&detail), 80);
        assert_eq!(
            cache.activity_events.len(),
            2,
            "content fingerprint must trigger a rebuild"
        );
    }

    fn check(status: &str, conclusion: Option<&str>) -> crate::github::types::CheckEntry {
        crate::github::types::CheckEntry {
            name: format!("check-{}-{}", status, conclusion.unwrap_or("none")),
            status: status.to_string(),
            conclusion: conclusion.map(String::from),
            url: String::new(),
        }
    }

    fn problems_text(detail: &PrDetailResponse) -> Vec<String> {
        build_problems_lines(Some(detail))
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn problems_lines_cover_failure_pending_conflict_and_zero_check_cases() {
        use crate::github::types::{CheckStatus, MergeableState};

        // Green: all success-ish checks + mergeable ⇒ no problems at all.
        let mut d = make_detail("org~repo~1", "2024-01-01T00:00:00Z");
        d.check_status = CheckStatus::Success;
        d.checks = vec![
            check("COMPLETED", Some("SUCCESS")),
            check("COMPLETED", Some("SKIPPED")),
            check("COMPLETED", Some("NEUTRAL")),
        ];
        assert!(problems_text(&d).is_empty(), "green PR has no problems");

        // Mixed: only the non-success checks are listed.
        d.checks = vec![
            check("COMPLETED", Some("SUCCESS")),
            check("COMPLETED", Some("FAILURE")),
            check("IN_PROGRESS", None),
            check("QUEUED", None),
        ];
        let text = problems_text(&d);
        assert_eq!(text.len(), 3, "one failure + two pending: {text:?}");
        assert!(!text.iter().any(|t| t.contains("SUCCESS")));

        // Conflict only: green checks but a conflicting merge state.
        d.checks = vec![check("COMPLETED", Some("SUCCESS"))];
        d.mergeable = MergeableState::Conflicting;
        let text = problems_text(&d);
        assert_eq!(text.len(), 1);
        assert!(text[0].contains("conflicts with base branch"));
        d.mergeable = MergeableState::Mergeable;

        // Legacy status-context shape: state mapped into `status`, no conclusion.
        d.checks = vec![check("SUCCESS", None)];
        assert!(problems_text(&d).is_empty(), "legacy green is success-ish");
        d.checks = vec![
            check("PENDING", None),
            check("FAILURE", None),
            check("ERROR", None),
        ];
        assert_eq!(problems_text(&d).len(), 3, "legacy pending/fail/error");

        // Empty checks but a failing/pending overall status still surfaces.
        d.checks = vec![];
        d.check_status = CheckStatus::Failure;
        assert_eq!(problems_text(&d), vec!["checks: failure".to_string()]);
        d.check_status = CheckStatus::Pending;
        assert_eq!(problems_text(&d), vec!["checks: pending".to_string()]);
        d.check_status = CheckStatus::Success;
        assert!(
            problems_text(&d).is_empty(),
            "empty checks + green status ⇒ absent"
        );
    }
}
