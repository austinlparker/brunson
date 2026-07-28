use std::collections::HashMap;

use crate::api::{PrListResponse, PrSummary};
use crate::github::types::{PrGroup, Priority};
use crate::tui::app::AppState;
use crate::tui::render::layout::{Blade, ViewLayout};

/// Focusable section inside the Overview blade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverviewFocus {
    #[default]
    Summary,
    Description,
    Checks,
    LastActivity,
}

impl OverviewFocus {
    pub fn next(self) -> Self {
        match self {
            OverviewFocus::Summary => OverviewFocus::Description,
            OverviewFocus::Description => OverviewFocus::Checks,
            OverviewFocus::Checks => OverviewFocus::LastActivity,
            OverviewFocus::LastActivity => OverviewFocus::Summary,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            OverviewFocus::Summary => OverviewFocus::LastActivity,
            OverviewFocus::Description => OverviewFocus::Summary,
            OverviewFocus::Checks => OverviewFocus::Description,
            OverviewFocus::LastActivity => OverviewFocus::Checks,
        }
    }
}

/// Scroll offset for one scrollable pane.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollState {
    pub offset: usize,
    pub content_height: usize,
    pub viewport_height: usize,
}

impl ScrollState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set content and viewport height and re-clamp the offset in one step.
    pub fn set_content_viewport(&mut self, content: usize, viewport: usize) {
        self.content_height = content;
        self.viewport_height = viewport;
        self.clamp();
    }

    /// Clamp `offset` so it does not scroll past the rendered content.
    pub fn clamp(&mut self) {
        self.offset = self.offset.min(self.max_scroll());
    }

    /// Maximum valid scroll offset.
    pub fn max_scroll(&self) -> usize {
        self.content_height.saturating_sub(self.viewport_height)
    }

    /// Scroll relative to the current offset, then clamp.
    pub fn scroll_by(&mut self, delta: isize) {
        if delta >= 0 {
            self.offset = self.offset.saturating_add(delta as usize);
        } else {
            self.offset = self.offset.saturating_sub(delta.unsigned_abs());
        }
        self.clamp();
    }

    /// Scroll to an absolute offset, clamped to the valid range.
    pub fn scroll_to(&mut self, pos: usize) {
        self.offset = pos.min(self.max_scroll());
    }

    /// Adjust the offset so `index` is within the viewport.
    pub fn keep_visible(&mut self, index: usize) {
        if self.viewport_height == 0 {
            return;
        }
        if index < self.offset {
            self.offset = index;
        } else if index >= self.offset + self.viewport_height {
            self.offset = index.saturating_sub(self.viewport_height - 1);
        }
    }
}

/// A rendered section of the Inbox. Most map 1:1 to a `PrGroup`; `Dependencies`
/// is synthetic — bot-authored PRs pulled out of the review lane so they don't
/// bury human review work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InboxSection {
    Group(PrGroup),
    Dependencies,
}

impl InboxSection {
    /// Human-readable section header label.
    pub fn label(&self) -> &'static str {
        match self {
            InboxSection::Group(PrGroup::AuthoredActionNeeded) => "MINE — ACTION NEEDED",
            InboxSection::Group(PrGroup::AuthoredReadyToMerge) => "MINE — READY TO MERGE",
            InboxSection::Group(PrGroup::AuthoredWaiting) => "MINE — WAITING",
            InboxSection::Group(PrGroup::ReviewNeeded) => "REVIEW — BLOCKED ON ME",
            InboxSection::Group(PrGroup::ReviewUpdate) => "REVIEW — NEW COMMITS",
            InboxSection::Group(PrGroup::ReviewDone) => "REVIEW — DONE",
            InboxSection::Group(PrGroup::Draft) => "DRAFTS",
            InboxSection::Group(PrGroup::Other) => "INVOLVED",
            InboxSection::Dependencies => "DEPENDENCIES",
        }
    }

    /// Sections that start folded on launch: low-signal buckets the user rarely
    /// needs open. A section with no explicit entry in `collapsed_sections`
    /// inherits this default.
    pub fn default_collapsed(&self) -> bool {
        matches!(
            self,
            InboxSection::Group(PrGroup::ReviewDone)
                | InboxSection::Group(PrGroup::Draft)
                | InboxSection::Dependencies
        )
    }

    /// Sections in render order. `Dependencies` sits between the review lane's
    /// active work and its done pile.
    pub fn render_order() -> &'static [InboxSection] {
        &[
            InboxSection::Group(PrGroup::AuthoredActionNeeded),
            InboxSection::Group(PrGroup::AuthoredReadyToMerge),
            InboxSection::Group(PrGroup::AuthoredWaiting),
            InboxSection::Group(PrGroup::ReviewNeeded),
            InboxSection::Group(PrGroup::ReviewUpdate),
            InboxSection::Dependencies,
            InboxSection::Group(PrGroup::ReviewDone),
            InboxSection::Group(PrGroup::Draft),
            InboxSection::Group(PrGroup::Other),
        ]
    }
}

/// One rendered line of the Inbox body, in display order. This is the single
/// source of truth for what the Inbox shows: `ViewStateManager::prepare`
/// builds it once via `build_inbox_rows`, and both selection/scroll math and
/// `render_inbox` consume it rather than recomputing grouping/order.
///
/// A `Header` is always emitted for a non-empty section, even when collapsed;
/// its `count` is the section's TRUE PR count regardless of fold state. When
/// `collapsed`, the section's `Pr` rows are omitted.
#[derive(Debug, Clone, PartialEq)]
pub enum InboxRow {
    Header {
        section: InboxSection,
        count: usize,
        collapsed: bool,
    },
    Pr {
        id: String,
    },
}

/// The Inbox cursor's identity. Section headers are selectable, so the cursor
/// can rest on a header (`Section`) or a PR row (`Pr`). This id-based anchor
/// (not a raw row index) is authoritative: `prepare` re-resolves it to a row
/// index every frame, so a rebuild that reorders or filters rows can't silently
/// move the cursor onto a different item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxCursor {
    Section(InboxSection),
    Pr(String),
}

/// All transient view state for the TUI.
#[derive(Debug, Clone)]
pub struct ViewState {
    pub active_blade: Blade,
    /// Cursor position in `inbox_rows` (row space, so it includes header rows).
    /// Derived every frame from `inbox_cursor`; see that field.
    pub selected_row: usize,
    pub selected_file_index: usize,
    pub overview_focus: OverviewFocus,
    /// Whether the Overview DESCRIPTION section is fully expanded (toggled by `d`).
    /// Defaults to collapsed so the catch-up sections aren't pushed below the fold.
    pub overview_description_expanded: bool,
    /// Per-section fold state. Sections absent from the map inherit
    /// `InboxSection::default_collapsed()`.
    pub collapsed_sections: HashMap<InboxSection, bool>,
    /// Authoritative Inbox cursor identity; `selected_row` is its resolved index.
    pub inbox_cursor: InboxCursor,

    pub inbox_scroll: ScrollState,
    pub overview_summary_scroll: ScrollState,
    pub overview_description_scroll: ScrollState,
    pub overview_checks_scroll: ScrollState,
    pub overview_last_activity_scroll: ScrollState,
    pub activity_scroll: ScrollState,
    pub files_scroll: ScrollState,
    pub diff_scroll: ScrollState,

    /// Rendered Inbox rows (section headers + PR rows), in display order.
    /// The single source of truth for Inbox grouping/order; see `InboxRow`.
    pub inbox_rows: Vec<InboxRow>,
}

impl Default for ViewState {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewState {
    pub fn new() -> Self {
        Self {
            active_blade: Blade::Inbox,
            selected_row: 0,
            selected_file_index: 0,
            overview_focus: OverviewFocus::Summary,
            overview_description_expanded: false,
            collapsed_sections: HashMap::new(),
            // Sentinel that matches no row; the first `prepare` falls back to the
            // first PR row.
            inbox_cursor: InboxCursor::Pr(String::new()),
            inbox_scroll: ScrollState::new(),
            overview_summary_scroll: ScrollState::new(),
            overview_description_scroll: ScrollState::new(),
            overview_checks_scroll: ScrollState::new(),
            overview_last_activity_scroll: ScrollState::new(),
            activity_scroll: ScrollState::new(),
            files_scroll: ScrollState::new(),
            diff_scroll: ScrollState::new(),
            inbox_rows: Vec::new(),
        }
    }

    /// Borrow the scroll state for the currently active blade/focus.
    pub fn active_scroll(&self) -> &ScrollState {
        match self.active_blade {
            Blade::Inbox => &self.inbox_scroll,
            Blade::Overview => match self.overview_focus {
                OverviewFocus::Summary => &self.overview_summary_scroll,
                OverviewFocus::Description => &self.overview_description_scroll,
                OverviewFocus::Checks => &self.overview_checks_scroll,
                OverviewFocus::LastActivity => &self.overview_last_activity_scroll,
            },
            Blade::Activity => &self.activity_scroll,
            Blade::Files => &self.files_scroll,
            Blade::Diff => &self.diff_scroll,
        }
    }

    /// Mutable borrow of the scroll state for the currently active blade/focus.
    pub fn active_scroll_mut(&mut self) -> &mut ScrollState {
        match self.active_blade {
            Blade::Inbox => &mut self.inbox_scroll,
            Blade::Overview => match self.overview_focus {
                OverviewFocus::Summary => &mut self.overview_summary_scroll,
                OverviewFocus::Description => &mut self.overview_description_scroll,
                OverviewFocus::Checks => &mut self.overview_checks_scroll,
                OverviewFocus::LastActivity => &mut self.overview_last_activity_scroll,
            },
            Blade::Activity => &mut self.activity_scroll,
            Blade::Files => &mut self.files_scroll,
            Blade::Diff => &mut self.diff_scroll,
        }
    }

    pub fn reset_scroll_offsets(&mut self) {
        self.inbox_scroll.offset = 0;
        self.overview_summary_scroll.offset = 0;
        self.overview_description_scroll.offset = 0;
        self.overview_checks_scroll.offset = 0;
        self.overview_last_activity_scroll.offset = 0;
        self.activity_scroll.offset = 0;
        self.files_scroll.offset = 0;
        self.diff_scroll.offset = 0;
    }
}

/// Owns `ViewState` and reconciles it with domain state before each render.
pub struct ViewStateManager {
    pub view: ViewState,
    /// The `selected_pr_id` last seen by `prepare`, used to detect a genuine
    /// selection change (navigation, or the selected PR disappearing) versus
    /// the same PR simply moving to a new position in a rebuilt `flat_prs`
    /// (e.g. after an LLM reclassification changes sort order).
    last_selected_pr_id: Option<String>,
}

impl Default for ViewStateManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewStateManager {
    pub fn new() -> Self {
        Self {
            view: ViewState::new(),
            last_selected_pr_id: None,
        }
    }

    /// Recompute derived view state from `app`, clamp scroll offsets, and trigger
    /// detail/diff reloads when the selected PR changes.
    ///
    /// Contract:
    /// 1. `view.inbox_rows` is rebuilt from `app.prs.groups`, `view.collapsed_sections`, and the
    ///    active filter: one section per `PrGroup` in `InboxSection::render_order` (bot review-lane
    ///    PRs split into `DEPENDENCIES`), each sorted by priority then most-recent `updated_at`,
    ///    with a selectable header row before each non-empty section.
    /// 2. `view.inbox_cursor` (a section- or PR-identity anchor) is authoritative. `view.selected_row`
    ///    is a derived cache of that anchor's row index, resynced here every call so a data refresh
    ///    that reorders/filters the list can't silently move the cursor onto a different item. If the
    ///    anchor no longer resolves (PR disappeared, section emptied, filtered out), the cursor falls
    ///    back to the first PR row. `app.selected_pr_id` — the PR whose detail is shown — follows the
    ///    cursor: the selected PR, or the first PR of the section when the cursor rests on a header.
    ///    Only a genuine identity change (not just a reordering) triggers detail/diff reload: cached
    ///    detail/diff/scroll state is cleared, and `view.selected_file_index` and all scroll offsets
    ///    are reset.
    /// 3. Scroll offsets are clamped to the currently rendered content lengths held in
    ///    `app.render_cache` and the blade viewport geometries in `layout`.
    /// 4. The selected row and selected file are kept visible within their scroll viewports.
    pub fn prepare(&mut self, app: &mut AppState, layout: &ViewLayout) {
        // 1. Recompute the Inbox row list (headers + PRs).
        self.view.inbox_rows =
            build_inbox_rows(&app.prs, &self.view.collapsed_sections, &app.search_filter);

        // 2. Reconcile the cursor. See the anchor-vs-index contract note above.
        let selected_row = resolve_cursor_row(&self.view.inbox_rows, &self.view.inbox_cursor)
            .or_else(|| {
                self.view
                    .inbox_rows
                    .iter()
                    .position(|r| matches!(r, InboxRow::Pr { .. }))
            })
            .unwrap_or(0);
        self.view.selected_row = selected_row;
        // Re-anchor to whatever the cursor actually resolved to, so a fallback
        // updates the identity we track from here on.
        if let Some(cursor) = cursor_of_row(&self.view.inbox_rows, selected_row) {
            self.view.inbox_cursor = cursor;
        }
        app.selected_pr_id = selected_pr_for_row(&self.view.inbox_rows, selected_row);

        if app.selected_pr_id != self.last_selected_pr_id {
            app.detail_needs_reload = true;
            app.diff_needs_reload = true;
            app.pr_detail = None;
            app.pr_diff = None;
            self.view.selected_file_index = 0;
            self.view.reset_scroll_offsets();
            self.last_selected_pr_id = app.selected_pr_id.clone();
        }

        // 4. Rebuild render caches so content lengths are current for this
        // frame. Each `rebuild_*` call is keyed on `app.pr_detail`'s identity
        // (see `RenderCache`), so passing `None` here (no PR selected, or one
        // just deselected) naturally rebuilds sections back down to their
        // empty/placeholder content instead of leaving stale output behind.
        app.render_cache.rebuild_activity(
            app.pr_detail.as_ref(),
            layout.blade(Blade::Activity).content.width,
        );
        app.render_cache.rebuild_overview(
            app.pr_detail.as_ref(),
            layout.blade(Blade::Overview).content.width,
        );
        app.render_cache.rebuild_diff(
            app.pr_detail.as_ref(),
            app.pr_diff.as_ref().map(|d| d.diff.as_str()),
            layout.blade(Blade::Diff).content.width,
            app.show_line_numbers,
        );

        // 5. Clamp scroll offsets using the rebuilt cache lengths and current viewports.
        // The Inbox blade reserves one row for its non-scrolling column header,
        // so the scrollable viewport is one row shorter than the active content area.
        let inbox_viewport = layout.blade(Blade::Inbox).content.height.saturating_sub(1) as usize;
        self.view
            .inbox_scroll
            .set_content_viewport(self.view.inbox_rows.len(), inbox_viewport);

        let file_count = app.pr_detail.as_ref().map_or(0, |d| d.files.len());
        // The Files blade reserves one row for its non-scrolling header, so the
        // scrollable viewport is one row shorter than the active content area.
        let files_viewport = layout.blade(Blade::Files).content.height.saturating_sub(1) as usize;
        self.view
            .files_scroll
            .set_content_viewport(file_count, files_viewport);

        self.view.activity_scroll.set_content_viewport(
            app.render_cache.activity_lines.len(),
            layout.blade(Blade::Activity).content.height as usize,
        );

        // The Diff blade reserves one row for its stats header, so the scrollable
        // viewport is one row shorter than the active content area.
        let diff_viewport = layout.blade(Blade::Diff).content.height.saturating_sub(1) as usize;
        self.view
            .diff_scroll
            .set_content_viewport(app.render_cache.diff_lines.len(), diff_viewport);

        // Clamp each Overview section against the height it is actually rendered
        // into, not the full body. `render_body_rows` splits the body via
        // `overview_section_heights`; mirror that here so scrolling can reach
        // content that overflows a section's slice.
        let lens = [
            app.render_cache.overview_summary.len(),
            // Collapse-aware: match the Description rows `render_body_rows`
            // actually shows, so scroll clamping doesn't reserve height for
            // lines hidden behind the "d to expand" marker.
            crate::tui::views::overview::description_display_line_count(
                app.render_cache.overview_description.len(),
                self.view.overview_description_expanded,
            ),
            app.render_cache.overview_checks.len(),
        ];
        let overview_body = layout
            .blade(Blade::Overview)
            .content
            .height
            .saturating_sub(crate::tui::views::overview::OVERVIEW_CHROME_ROWS);
        let section_heights = crate::tui::views::overview::overview_section_heights(
            overview_body,
            lens,
            self.view.overview_focus,
        );
        // Each scrollable section reserves one row for its header label.
        self.view
            .overview_summary_scroll
            .set_content_viewport(lens[0], section_heights[0].saturating_sub(1) as usize);
        self.view
            .overview_description_scroll
            .set_content_viewport(lens[1], section_heights[1].saturating_sub(1) as usize);
        self.view
            .overview_checks_scroll
            .set_content_viewport(lens[2], section_heights[2].saturating_sub(1) as usize);
        self.view
            .overview_last_activity_scroll
            .set_content_viewport(1, section_heights[3] as usize);

        // 6. Keep selected items visible. `selected_row` already indexes the
        // rendered line space (`inbox_rows` includes header rows).
        if !self.view.inbox_rows.is_empty() {
            self.view.inbox_scroll.keep_visible(self.view.selected_row);
        }
        if file_count > 0 {
            self.view
                .files_scroll
                .keep_visible(self.view.selected_file_index.min(file_count - 1));
        }
    }
}

fn priority_rank(p: Option<&Priority>) -> u8 {
    match p {
        Some(Priority::High) => 0,
        Some(Priority::Medium) => 1,
        Some(Priority::Low) => 2,
        None => 3,
    }
}

/// Row index the cursor anchor resolves to in the current row list, if present.
fn resolve_cursor_row(rows: &[InboxRow], cursor: &InboxCursor) -> Option<usize> {
    rows.iter().position(|row| match (row, cursor) {
        (InboxRow::Header { section, .. }, InboxCursor::Section(s)) => section == s,
        (InboxRow::Pr { id }, InboxCursor::Pr(cid)) => id == cid,
        _ => false,
    })
}

/// The cursor anchor identity for the row at `row`, if any.
fn cursor_of_row(rows: &[InboxRow], row: usize) -> Option<InboxCursor> {
    match rows.get(row)? {
        InboxRow::Header { section, .. } => Some(InboxCursor::Section(*section)),
        InboxRow::Pr { id } => Some(InboxCursor::Pr(id.clone())),
    }
}

/// The PR whose detail the cursor implies: the PR at `row` when it's a PR row,
/// otherwise the first PR of the section whose header sits at `row`. `None` when
/// the row is a header of a folded (or empty) section.
fn selected_pr_for_row(rows: &[InboxRow], row: usize) -> Option<String> {
    match rows.get(row)? {
        InboxRow::Pr { id } => Some(id.clone()),
        InboxRow::Header { .. } => match rows.get(row + 1)? {
            InboxRow::Pr { id } => Some(id.clone()),
            InboxRow::Header { .. } => None,
        },
    }
}

/// True for machine authors, whose review-lane PRs are routed into the
/// `DEPENDENCIES` section. Prefers the GraphQL actor type carried on the summary;
/// the `[bot]` suffix is a fallback for PRs cached before that field existed
/// (note the daemon's GraphQL returns bot logins WITHOUT a `[bot]` suffix, so
/// the flag is what actually fires in normal operation).
fn is_bot_author(pr: &PrSummary) -> bool {
    pr.author_is_bot || pr.author.ends_with("[bot]")
}

/// Build the Inbox row list (section headers + PR rows) in display order. This
/// is the ONLY place that decides Inbox grouping/order: `prepare` uses it for
/// selection/scroll math, and `render_inbox` maps it straight to screen lines.
///
/// One section per `PrGroup` (in `InboxSection::render_order`), except review-lane
/// PRs authored by bots, which are pulled into a synthetic `DEPENDENCIES` section.
/// Empty sections are skipped. Each non-empty section emits a `Header` (with its
/// TRUE count) followed by its PR rows, unless the section is folded, in which
/// case only the header is emitted. PRs within a section are sorted by priority
/// then most-recent `updated_at`.
fn build_inbox_rows(
    prs: &PrListResponse,
    collapsed: &HashMap<InboxSection, bool>,
    filter: &str,
) -> Vec<InboxRow> {
    let mut sections: HashMap<InboxSection, Vec<&PrSummary>> = HashMap::new();
    for group in PrGroup::all_in_priority_order() {
        let Some(list) = prs.groups.get(group) else {
            continue;
        };
        for pr in list {
            if !pr.matches_filter(filter) {
                continue;
            }
            // Bot-authored PRs anyone might be asked to look at (review lane or
            // merely involved) fold into DEPENDENCIES so the human sections stay
            // human. Authored/draft lanes keep bot PRs in place: those are the
            // user's own responsibility regardless of who opened them.
            let is_review_or_involved = matches!(
                group,
                PrGroup::ReviewNeeded
                    | PrGroup::ReviewUpdate
                    | PrGroup::ReviewDone
                    | PrGroup::Other
            );
            let section = if is_review_or_involved && is_bot_author(pr) {
                InboxSection::Dependencies
            } else {
                InboxSection::Group(*group)
            };
            sections.entry(section).or_default().push(pr);
        }
    }

    let sort_fn = |a: &&PrSummary, b: &&PrSummary| {
        priority_rank(a.llm_priority.as_ref())
            .cmp(&priority_rank(b.llm_priority.as_ref()))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    };

    let mut rows: Vec<InboxRow> = Vec::new();
    for section in InboxSection::render_order() {
        let Some(list) = sections.get_mut(section) else {
            continue;
        };
        if list.is_empty() {
            continue;
        }
        list.sort_by(sort_fn);
        let is_collapsed = collapsed
            .get(section)
            .copied()
            .unwrap_or_else(|| section.default_collapsed());
        rows.push(InboxRow::Header {
            section: *section,
            count: list.len(),
            collapsed: is_collapsed,
        });
        if !is_collapsed {
            rows.extend(list.iter().map(|p| InboxRow::Pr { id: p.id.clone() }));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{HealthResponse, PrSummary};
    use crate::config::Config;
    use crate::github::types::{CheckStatus, PrGroup, Priority};
    use crate::tui::client::DaemonClient;

    fn make_summary(
        id: &str,
        group: PrGroup,
        author: &str,
        priority: Option<Priority>,
    ) -> PrSummary {
        PrSummary {
            id: id.to_string(),
            node_id: "node".to_string(),
            owner: "org".to_string(),
            repo: "repo".to_string(),
            number: id.split('~').next_back().unwrap().parse().unwrap_or(1),
            title: format!("Test {}", id),
            author: author.to_string(),
            author_is_bot: false,
            group,
            next_action: "Review now".to_string(),
            check_status: CheckStatus::None,
            llm_priority: priority,
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            url: "https://example.com".to_string(),
            comments: 0,
        }
    }

    fn make_test_state(groups: HashMap<PrGroup, Vec<PrSummary>>) -> AppState {
        let config = Config::default();
        let client = DaemonClient::new(config.daemon.port).unwrap();
        let mut state = AppState::new(config, client);
        state.prs = PrListResponse {
            groups,
            updated_at: "2024-01-01T00:00:00Z".to_string(),
        };
        state.health = Some(HealthResponse {
            service: crate::daemon::SERVICE_NAME.to_string(),
            version: "0.1.0".to_string(),
            status: "ok".to_string(),
            current_user: "me".to_string(),
            last_poll_at: None,
            last_poll_error: None,
            rate_limit_remaining: None,
            refresh_in_progress: false,
            setup_status: "ready".to_string(),
            setup_message: None,
            binary_mtime: None,
        });
        state
    }

    #[allow(clippy::too_many_arguments)]
    fn summary_with_fields(
        id: &str,
        group: PrGroup,
        title: &str,
        author: &str,
        owner: &str,
        repo: &str,
        number: u64,
        next_action: &str,
    ) -> PrSummary {
        PrSummary {
            id: id.to_string(),
            node_id: "node".to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            number,
            title: title.to_string(),
            author: author.to_string(),
            author_is_bot: false,
            group,
            next_action: next_action.to_string(),
            check_status: CheckStatus::None,
            llm_priority: None,
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            url: "https://example.com".to_string(),
            comments: 0,
        }
    }

    fn make_layout(width: u16, height: u16, active: Blade) -> ViewLayout {
        let area = ratatui::layout::Rect::new(0, 0, width, height);
        crate::tui::render::layout::RootLayout::new(active).compute(area)
    }

    #[test]
    fn pr_matches_filter_case_insensitive_fields() {
        let pr = summary_with_fields(
            "Acme~widgets~42",
            PrGroup::ReviewNeeded,
            "Fix Login Bug",
            "Alice",
            "Acme",
            "widgets",
            42,
            "Review now",
        );

        assert!(pr.matches_filter(""));
        assert!(pr.matches_filter("bug"));
        assert!(pr.matches_filter("BUG"));
        assert!(pr.matches_filter("alice"));
        assert!(pr.matches_filter("ALICE"));
        assert!(pr.matches_filter("widgets"));
        assert!(pr.matches_filter("acme"));
        assert!(pr.matches_filter("42"));
        assert!(pr.matches_filter("acme~widgets~42"));
        assert!(pr.matches_filter("review now"));
        assert!(!pr.matches_filter("zzz"));
        assert!(!pr.matches_filter("99"));
    }

    /// Extract just the PR ids (in order) from a row list, dropping headers.
    fn pr_ids(rows: &[InboxRow]) -> Vec<String> {
        rows.iter()
            .filter_map(|row| match row {
                InboxRow::Pr { id } => Some(id.clone()),
                InboxRow::Header { .. } => None,
            })
            .collect()
    }

    #[test]
    fn build_flat_prs_filters_and_preserves_lane_counts() {
        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::AuthoredWaiting,
            vec![
                summary_with_fields(
                    "x~y~1",
                    PrGroup::AuthoredWaiting,
                    "Alpha feature",
                    "me",
                    "x",
                    "y",
                    1,
                    "Waiting",
                ),
                summary_with_fields(
                    "x~y~2",
                    PrGroup::AuthoredWaiting,
                    "Beta patch",
                    "me",
                    "x",
                    "y",
                    2,
                    "Waiting",
                ),
            ],
        );
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![
                summary_with_fields(
                    "x~y~3",
                    PrGroup::ReviewNeeded,
                    "Gamma fix",
                    "other",
                    "x",
                    "y",
                    3,
                    "Review now",
                ),
                summary_with_fields(
                    "x~y~4",
                    PrGroup::ReviewNeeded,
                    "Delta docs",
                    "other",
                    "x",
                    "y",
                    4,
                    "Review now",
                ),
            ],
        );

        let prs = PrListResponse {
            groups,
            updated_at: String::new(),
        };
        let collapsed = HashMap::new();

        let rows = build_inbox_rows(&prs, &collapsed, "alpha");
        assert_eq!(pr_ids(&rows), vec!["x~y~1"], "only one authored PR matches");

        let rows = build_inbox_rows(&prs, &collapsed, "3");
        assert_eq!(pr_ids(&rows), vec!["x~y~3"]);

        let rows = build_inbox_rows(&prs, &collapsed, "");
        assert_eq!(pr_ids(&rows), vec!["x~y~1", "x~y~2", "x~y~3", "x~y~4"]);
    }

    #[test]
    fn prepare_preserves_selected_pr_id_on_filter_change() {
        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![
                summary_with_fields(
                    "o~r~1",
                    PrGroup::ReviewNeeded,
                    "First",
                    "other",
                    "o",
                    "r",
                    1,
                    "Review now",
                ),
                summary_with_fields(
                    "o~r~2",
                    PrGroup::ReviewNeeded,
                    "Second",
                    "other",
                    "o",
                    "r",
                    2,
                    "Review now",
                ),
                summary_with_fields(
                    "o~r~3",
                    PrGroup::ReviewNeeded,
                    "Third",
                    "other",
                    "o",
                    "r",
                    3,
                    "Review now",
                ),
            ],
        );
        let mut app = make_test_state(groups);
        let mut view = ViewStateManager::new();
        let layout = make_layout(80, 24, Blade::Inbox);

        view.prepare(&mut app, &layout);
        // Row 0 is the section header; the cursor starts on the first PR row.
        assert_eq!(view.view.selected_row, 1);
        assert_eq!(app.selected_pr_id, Some("o~r~1".to_string()));
        // Row 3 is the third PR (rows: header, o~r~1, o~r~2, o~r~3).
        app.set_selected_row(&mut view, 3);
        view.prepare(&mut app, &layout);
        assert_eq!(app.selected_pr_id, Some("o~r~3".to_string()));

        // Filter to a subset that still includes the selected PR ("First" and
        // "Third" both contain "ir"); the selected PR follows to its new row.
        app.search_filter = "ir".to_string();
        view.prepare(&mut app, &layout);
        assert_eq!(view.view.selected_row, 2);
        assert_eq!(app.selected_pr_id, Some("o~r~3".to_string()));
    }

    #[test]
    fn prepare_keeps_selection_stable_when_list_reorders_without_navigation() {
        // Regression test: a data refresh that reorders the list (e.g. an
        // LLM reclassification changing sort order) must not silently move
        // the selection to a different PR just because the PR that used to
        // sit at the selected index changed.
        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![
                make_summary("o~r~a", PrGroup::ReviewNeeded, "other", Some(Priority::Low)),
                make_summary(
                    "o~r~b",
                    PrGroup::ReviewNeeded,
                    "other",
                    Some(Priority::High),
                ),
            ],
        );
        let mut app = make_test_state(groups);
        let mut view = ViewStateManager::new();
        let layout = make_layout(80, 24, Blade::Inbox);

        view.prepare(&mut app, &layout);
        // High priority sorts first (row 0 is the section header).
        assert_eq!(pr_ids(&view.view.inbox_rows), vec!["o~r~b", "o~r~a"]);

        // Select the second PR ("a", currently at row 2).
        app.set_selected_row(&mut view, 2);
        view.prepare(&mut app, &layout);
        assert_eq!(app.selected_pr_id, Some("o~r~a".to_string()));
        app.detail_needs_reload = false; // simulate detail already loaded

        // Reclassify "a" to High priority: it now sorts first, at index 0,
        // even though the user's selection (by identity) hasn't moved.
        for prs in app.prs.groups.values_mut() {
            for pr in prs.iter_mut() {
                if pr.id == "o~r~a" {
                    pr.llm_priority = Some(Priority::High);
                }
            }
        }
        view.prepare(&mut app, &layout);

        // The list reordered...
        assert_eq!(pr_ids(&view.view.inbox_rows), vec!["o~r~a", "o~r~b"]);
        // ...but the selection followed the PR's new position (now row 1, after
        // the header) instead of staying pinned to a row that changed identity.
        assert_eq!(view.view.selected_row, 1);
        assert_eq!(app.selected_pr_id, Some("o~r~a".to_string()));
        assert!(
            !app.detail_needs_reload,
            "reordering under a stable selection must not trigger a reload"
        );
    }

    #[test]
    fn prepare_resets_to_first_match_when_selected_filtered_out() {
        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![
                summary_with_fields(
                    "o~r~1",
                    PrGroup::ReviewNeeded,
                    "First",
                    "other",
                    "o",
                    "r",
                    1,
                    "Review now",
                ),
                summary_with_fields(
                    "o~r~2",
                    PrGroup::ReviewNeeded,
                    "Second",
                    "other",
                    "o",
                    "r",
                    2,
                    "Review now",
                ),
            ],
        );
        let mut app = make_test_state(groups);
        let mut view = ViewStateManager::new();
        let layout = make_layout(80, 24, Blade::Inbox);

        view.prepare(&mut app, &layout);
        // Row 2 is the second PR (rows: header, o~r~1, o~r~2).
        app.set_selected_row(&mut view, 2);
        view.prepare(&mut app, &layout);
        assert_eq!(app.selected_pr_id, Some("o~r~2".to_string()));

        // Filter removes the selected PR; selection should move to the first match.
        app.search_filter = "first".to_string();
        view.prepare(&mut app, &layout);
        assert_eq!(view.view.selected_row, 1);
        assert_eq!(app.selected_pr_id, Some("o~r~1".to_string()));
    }

    #[test]
    fn inbox_rows_order_mine_before_review() {
        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![make_summary(
                "a~b~1",
                PrGroup::ReviewNeeded,
                "other",
                Some(Priority::High),
            )],
        );
        groups.insert(
            PrGroup::AuthoredWaiting,
            vec![make_summary("a~b~2", PrGroup::AuthoredWaiting, "me", None)],
        );

        let mut app = make_test_state(groups);
        let mut view = ViewStateManager::new();
        let layout = make_layout(80, 24, Blade::Inbox);
        view.prepare(&mut app, &layout);
        // Authored ("MINE — WAITING") sections render before review sections.
        assert_eq!(pr_ids(&view.view.inbox_rows), vec!["a~b~2", "a~b~1"]);
    }

    #[test]
    fn build_inbox_rows_routes_bots_to_dependencies_and_defaults_collapsed() {
        // A plain "dependabot" login with the bot flag set (the daemon's GraphQL
        // returns bot logins WITHOUT a "[bot]" suffix), plus a human whose login
        // merely ends in "bot" — the latter must NOT be routed as a dependency.
        let mut bot = make_summary("o~r~2", PrGroup::ReviewNeeded, "dependabot", None);
        bot.author_is_bot = true;
        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![
                make_summary("o~r~1", PrGroup::ReviewNeeded, "cabot", None),
                bot,
            ],
        );
        let prs = PrListResponse {
            groups,
            updated_at: String::new(),
        };
        let collapsed = HashMap::new();
        let rows = build_inbox_rows(&prs, &collapsed, "");

        // The human PR (login "cabot", flag false) stays in the review section;
        // the bot PR is pulled into a DEPENDENCIES section collapsed by default.
        let sections: Vec<InboxSection> = rows
            .iter()
            .filter_map(|r| match r {
                InboxRow::Header { section, .. } => Some(*section),
                InboxRow::Pr { .. } => None,
            })
            .collect();
        assert!(sections.contains(&InboxSection::Group(PrGroup::ReviewNeeded)));
        assert!(sections.contains(&InboxSection::Dependencies));

        let dep_header = rows
            .iter()
            .find(|r| {
                matches!(
                    r,
                    InboxRow::Header {
                        section: InboxSection::Dependencies,
                        ..
                    }
                )
            })
            .unwrap();
        assert_eq!(
            *dep_header,
            InboxRow::Header {
                section: InboxSection::Dependencies,
                count: 1,
                collapsed: true,
            }
        );
        // Only the human PR is visible; the bot PR is folded away by default.
        assert_eq!(pr_ids(&rows), vec!["o~r~1"]);
    }

    #[test]
    fn prepare_resets_selection_when_pr_disappears() {
        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![make_summary("a~b~1", PrGroup::ReviewNeeded, "other", None)],
        );
        let mut app = make_test_state(groups);
        app.selected_pr_id = Some("a~b~1".to_string());

        let mut view = ViewStateManager::new();
        let layout = make_layout(80, 24, Blade::Inbox);
        view.prepare(&mut app, &layout);
        assert_eq!(app.selected_pr_id, Some("a~b~1".to_string()));
        // First time this PR becomes selected from the manager's
        // perspective: detail/diff need to be fetched.
        assert!(app.detail_needs_reload);

        // Simulate the detail having been fetched (as the real event loop
        // would do after a DetailLoaded event), then re-run prepare with
        // nothing changed — an unchanged selection must not re-trigger reload.
        app.detail_needs_reload = false;
        app.diff_needs_reload = false;
        view.prepare(&mut app, &layout);
        assert!(!app.detail_needs_reload);

        // Now remove the PR and run prepare again.
        app.prs.groups.clear();
        view.prepare(&mut app, &layout);

        assert!(app.selected_pr_id.is_none());
        assert!(app.detail_needs_reload);
        assert!(app.diff_needs_reload);
        assert!(app.pr_detail.is_none());
        assert!(app.pr_diff.is_none());
        assert!(app.render_cache.diff_lines.is_empty());
        assert_eq!(view.view.selected_row, 0);
        assert_eq!(view.view.selected_file_index, 0);
        assert_eq!(view.view.inbox_scroll.offset, 0);
        assert_eq!(view.view.files_scroll.offset, 0);
    }

    #[test]
    fn prepare_clamps_scroll_offsets_after_resize() {
        let mut groups = HashMap::new();
        let mut prs = Vec::new();
        for i in 0..100 {
            prs.push(make_summary(
                &format!("a~b~{}", i),
                PrGroup::ReviewNeeded,
                "other",
                None,
            ));
        }
        groups.insert(PrGroup::ReviewNeeded, prs);
        let mut app = make_test_state(groups);
        app.selected_pr_id = Some("a~b~0".to_string());

        let mut view = ViewStateManager::new();
        // Large terminal lets the inbox scroll freely.
        let big = make_layout(200, 200, Blade::Inbox);
        view.prepare(&mut app, &big);
        view.view.inbox_scroll.scroll_to(50);

        // Shrink the terminal: the offset must be clamped back down.
        let small = make_layout(80, 12, Blade::Inbox);
        view.prepare(&mut app, &small);
        assert!(
            view.view.inbox_scroll.offset <= view.view.inbox_scroll.max_scroll(),
            "inbox scroll {} exceeds max {}",
            view.view.inbox_scroll.offset,
            view.view.inbox_scroll.max_scroll()
        );
        assert!(
            view.view.inbox_scroll.viewport_height > 0,
            "viewport should be non-zero"
        );
    }

    /// Build a `PrListResponse` with `opened_count` authored PRs and
    /// `review_count` review-requested PRs, for row-shape tests below.
    fn make_prs_of_sizes(opened_count: usize, review_count: usize) -> PrListResponse {
        let mut groups = HashMap::new();
        if opened_count > 0 {
            groups.insert(
                PrGroup::AuthoredWaiting,
                (0..opened_count)
                    .map(|i| {
                        make_summary(&format!("o~o~{i}"), PrGroup::AuthoredWaiting, "me", None)
                    })
                    .collect(),
            );
        }
        if review_count > 0 {
            groups.insert(
                PrGroup::ReviewNeeded,
                (0..review_count)
                    .map(|i| {
                        make_summary(&format!("r~r~{i}"), PrGroup::ReviewNeeded, "other", None)
                    })
                    .collect(),
            );
        }
        PrListResponse {
            groups,
            updated_at: String::new(),
        }
    }

    #[test]
    fn build_inbox_rows_places_headers_before_each_non_empty_section() {
        let collapsed = HashMap::new();

        // review only: header at index 0, then all PR rows.
        let rows = build_inbox_rows(&make_prs_of_sizes(0, 5), &collapsed, "");
        assert_eq!(
            rows[0],
            InboxRow::Header {
                section: InboxSection::Group(PrGroup::ReviewNeeded),
                count: 5,
                collapsed: false,
            }
        );
        assert_eq!(rows.len(), 6);

        // opened only: header at index 0, then all PR rows.
        let rows = build_inbox_rows(&make_prs_of_sizes(5, 0), &collapsed, "");
        assert_eq!(
            rows[0],
            InboxRow::Header {
                section: InboxSection::Group(PrGroup::AuthoredWaiting),
                count: 5,
                collapsed: false,
            }
        );
        assert_eq!(rows.len(), 6);

        // both sections: authored header, its PRs, then review header, its PRs.
        let rows = build_inbox_rows(&make_prs_of_sizes(3, 4), &collapsed, "");
        assert_eq!(
            rows[0],
            InboxRow::Header {
                section: InboxSection::Group(PrGroup::AuthoredWaiting),
                count: 3,
                collapsed: false,
            }
        );
        assert_eq!(
            rows[4],
            InboxRow::Header {
                section: InboxSection::Group(PrGroup::ReviewNeeded),
                count: 4,
                collapsed: false,
            }
        );
        assert_eq!(rows.len(), 3 + 4 + 2);
    }

    #[test]
    fn build_inbox_rows_only_headers_non_empty_sections_with_matching_counts() {
        let collapsed = HashMap::new();

        // Empty section produces no header at all, not a header with count 0.
        let rows = build_inbox_rows(&make_prs_of_sizes(0, 3), &collapsed, "");
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, InboxRow::Header { .. }))
                .count(),
            1,
            "no header row for the empty authored section"
        );

        // Each Header's count matches the number of Pr rows that follow it
        // (up to the next Header or the end of the list).
        let rows = build_inbox_rows(&make_prs_of_sizes(3, 4), &collapsed, "");
        let mut i = 0;
        while i < rows.len() {
            if let InboxRow::Header { count, .. } = &rows[i] {
                let mut actual = 0;
                let mut j = i + 1;
                while j < rows.len() && matches!(rows[j], InboxRow::Pr { .. }) {
                    actual += 1;
                    j += 1;
                }
                assert_eq!(actual, *count, "header count must match following PR rows");
                i = j;
            } else {
                i += 1;
            }
        }
    }

    #[test]
    fn overview_focus_cycles() {
        let mut focus = OverviewFocus::Summary;
        focus = focus.next();
        assert_eq!(focus, OverviewFocus::Description);
        focus = focus.next();
        assert_eq!(focus, OverviewFocus::Checks);
        focus = focus.next();
        assert_eq!(focus, OverviewFocus::LastActivity);
        focus = focus.next();
        assert_eq!(focus, OverviewFocus::Summary);

        focus = focus.prev();
        assert_eq!(focus, OverviewFocus::LastActivity);
    }
}
