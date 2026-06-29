use std::collections::HashMap;

use crate::api::{group_key, PrListResponse, PrSummary};
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

/// All transient view state for the TUI.
#[derive(Debug, Clone)]
pub struct ViewState {
    pub active_blade: Blade,
    pub selected_index: usize,
    pub selected_file_index: usize,
    pub overview_focus: OverviewFocus,
    pub collapsed_groups: HashMap<String, bool>,

    pub inbox_scroll: ScrollState,
    pub overview_summary_scroll: ScrollState,
    pub overview_description_scroll: ScrollState,
    pub overview_checks_scroll: ScrollState,
    pub overview_last_activity_scroll: ScrollState,
    pub activity_scroll: ScrollState,
    pub files_scroll: ScrollState,
    pub diff_scroll: ScrollState,

    /// Flat list of PR ids in Inbox display order.
    pub flat_prs: Vec<String>,
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
            selected_index: 0,
            selected_file_index: 0,
            overview_focus: OverviewFocus::Summary,
            collapsed_groups: HashMap::new(),
            inbox_scroll: ScrollState::new(),
            overview_summary_scroll: ScrollState::new(),
            overview_description_scroll: ScrollState::new(),
            overview_checks_scroll: ScrollState::new(),
            overview_last_activity_scroll: ScrollState::new(),
            activity_scroll: ScrollState::new(),
            files_scroll: ScrollState::new(),
            diff_scroll: ScrollState::new(),
            flat_prs: Vec::new(),
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
        }
    }

    /// Recompute derived view state from `app`, clamp scroll offsets, and trigger
    /// detail/diff reloads when the selected PR changes.
    ///
    /// Contract:
    /// 1. `view.flat_prs` is rebuilt from `app.prs.groups` and `view.collapsed_groups` in the
    ///    same order the old `AppState::flat_sorted_pulls` produced: opened-by-me groups first,
    ///    then review-requested groups, with draft PRs routed by author, and each section sorted
    ///    by priority then most-recent `updated_at`.
    /// 2. `view.selected_index` and `app.selected_pr_id` are reconciled. If the PR at the current
    ///    index disappeared or the id cannot be found, `app.selected_pr_id` is updated, detail
    ///    and diff reload flags are set, cached detail/diff/scroll state is cleared, and
    ///    `view.selected_file_index` and all scroll offsets are reset.
    /// 3. Scroll offsets are clamped to the currently rendered content lengths held in
    ///    `app.render_cache` and the blade viewport geometries in `layout`.
    /// 4. The selected PR and selected file are kept visible within their scroll viewports.
    pub fn prepare(&mut self, app: &mut AppState, layout: &ViewLayout) {
        let current_user = app
            .health
            .as_ref()
            .map(|h| h.current_user.as_str())
            .unwrap_or("");

        // 1. Recompute flattened PR list.
        let (opened_len, review_len, flat_prs) =
            build_flat_prs(&app.prs, &self.view.collapsed_groups, current_user);
        self.view.flat_prs = flat_prs;

        let header_count = visible_header_count(opened_len, review_len);

        // 2. Reconcile selection.
        // `view.selected_index` is authoritative: if the PR at that index differs from
        // `app.selected_pr_id`, we switch to the new PR and request fresh detail/diff.
        if self.view.flat_prs.is_empty() {
            self.view.selected_index = 0;
            if app.selected_pr_id.is_some() {
                app.selected_pr_id = None;
                app.detail_needs_reload = true;
                app.diff_needs_reload = true;
                app.pr_detail = None;
                app.pr_diff = None;
                app.diff_lines = Vec::new();
                app.diff_comments = HashMap::new();
                self.view.selected_file_index = 0;
                self.view.reset_scroll_offsets();
                app.render_cache.clear();
            }
        } else {
            if self.view.selected_index >= self.view.flat_prs.len() {
                self.view.selected_index = self.view.flat_prs.len() - 1;
            }
            let expected_id = self
                .view
                .flat_prs
                .get(self.view.selected_index)
                .map(|s| s.as_str());
            if app.selected_pr_id.as_deref() != expected_id {
                app.selected_pr_id = expected_id.map(|s| s.to_string());
                app.detail_needs_reload = true;
                app.diff_needs_reload = true;
                app.pr_detail = None;
                app.pr_diff = None;
                app.diff_lines = Vec::new();
                app.diff_comments = HashMap::new();
                self.view.selected_file_index = 0;
                self.view.reset_scroll_offsets();
                app.render_cache.clear();
            }
        }

        // 3. Rebuild render caches so content lengths are current for this frame.
        if let Some(detail) = app.pr_detail.as_ref() {
            app.render_cache.rebuild_activity(
                Some(detail),
                layout.blade(Blade::Activity).content.width,
                16,
            );
            app.render_cache
                .rebuild_overview(Some(detail), layout.blade(Blade::Overview).content.width);
        }
        app.render_cache.rebuild_diff(
            app.pr_detail.as_ref(),
            app.pr_diff.as_ref().map(|d| d.diff.as_str()),
            layout.blade(Blade::Diff).content.width,
            app.show_line_numbers,
        );

        // 4. Clamp scroll offsets using the rebuilt cache lengths and current viewports.
        // The Inbox blade reserves one row for its non-scrolling column header,
        // so the scrollable viewport is one row shorter than the active content area.
        let inbox_viewport = layout.blade(Blade::Inbox).content.height.saturating_sub(1) as usize;
        self.view
            .inbox_scroll
            .set_content_viewport(self.view.flat_prs.len() + header_count, inbox_viewport);

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

        let overview_viewport = layout.blade(Blade::Overview).content.height as usize;
        self.view
            .overview_summary_scroll
            .set_content_viewport(app.render_cache.overview_summary.len(), overview_viewport);
        self.view.overview_description_scroll.set_content_viewport(
            app.render_cache.overview_description.len(),
            overview_viewport,
        );
        self.view
            .overview_checks_scroll
            .set_content_viewport(app.render_cache.overview_checks.len(), overview_viewport);
        self.view
            .overview_last_activity_scroll
            .set_content_viewport(1, overview_viewport);

        // 5. Keep selected items visible.
        // The Inbox scroll offset is measured in screen lines that include
        // section headers, so convert the flat PR index to its line index before
        // calling keep_visible.
        if !self.view.flat_prs.is_empty() {
            let line_idx =
                flat_index_to_line_index(self.view.selected_index, opened_len, review_len);
            self.view.inbox_scroll.keep_visible(line_idx);
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

/// Return `(opened_count, review_count, flat_ids)` for the Inbox display.
fn build_flat_prs(
    prs: &PrListResponse,
    collapsed: &HashMap<String, bool>,
    current_user: &str,
) -> (usize, usize, Vec<String>) {
    let mut opened: Vec<&PrSummary> = Vec::new();
    let mut review: Vec<&PrSummary> = Vec::new();

    for group in PrGroup::all_in_priority_order() {
        let key = group_key(group);
        if *collapsed.get(&key).unwrap_or(&false) {
            continue;
        }
        if let Some(prs) = prs.groups.get(&key) {
            for pr in prs {
                match group {
                    PrGroup::AuthoredActionNeeded
                    | PrGroup::AuthoredReadyToMerge
                    | PrGroup::AuthoredWaiting => opened.push(pr),
                    PrGroup::ReviewNeeded | PrGroup::ReviewUpdate | PrGroup::ReviewDone => {
                        review.push(pr)
                    }
                    PrGroup::Draft => {
                        if pr.author == current_user {
                            opened.push(pr);
                        } else {
                            review.push(pr);
                        }
                    }
                    PrGroup::Other => review.push(pr),
                }
            }
        }
    }

    let sort_fn = |a: &&PrSummary, b: &&PrSummary| {
        priority_rank(a.llm_priority.as_ref())
            .cmp(&priority_rank(b.llm_priority.as_ref()))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    };

    opened.sort_by(sort_fn);
    review.sort_by(sort_fn);

    let opened_len = opened.len();
    let review_len = review.len();
    let mut flat: Vec<String> = Vec::with_capacity(opened_len + review_len);
    flat.extend(opened.into_iter().map(|p| p.id.clone()));
    flat.extend(review.into_iter().map(|p| p.id.clone()));
    (opened_len, review_len, flat)
}

fn visible_header_count(opened_len: usize, review_len: usize) -> usize {
    let mut count = 0;
    if opened_len > 0 {
        count += 1;
    }
    if review_len > 0 {
        count += 1;
    }
    count
}

/// Map a flat PR index into the line index of the rendered Inbox list, which
/// contains a section header before each non-empty group.
fn flat_index_to_line_index(selected_index: usize, opened_len: usize, review_len: usize) -> usize {
    let mut line = selected_index;
    if opened_len > 0 {
        line += 1;
    }
    if selected_index >= opened_len && review_len > 0 {
        line += 1;
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{HealthResponse, PrSummary};
    use crate::config::Config;
    use crate::github::types::Priority;
    use crate::tui::client::DaemonClient;

    fn make_summary(id: &str, group: &str, author: &str, priority: Option<Priority>) -> PrSummary {
        PrSummary {
            id: id.to_string(),
            node_id: "node".to_string(),
            owner: "org".to_string(),
            repo: "repo".to_string(),
            number: id.split('~').next_back().unwrap().parse().unwrap_or(1),
            title: format!("Test {}", id),
            author: author.to_string(),
            group: group.to_string(),
            next_action: "Review now".to_string(),
            check_status: "none".to_string(),
            llm_priority: priority,
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            url: "https://example.com".to_string(),
            comments: 0,
        }
    }

    fn make_test_state(groups: HashMap<String, Vec<PrSummary>>) -> AppState {
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
        });
        state
    }

    fn make_layout(width: u16, height: u16, active: Blade) -> ViewLayout {
        let area = ratatui::layout::Rect::new(0, 0, width, height);
        crate::tui::render::layout::RootLayout::new(active).compute(area)
    }

    #[test]
    fn flat_prs_order_opened_by_me_first_then_review() {
        let mut groups = HashMap::new();
        groups.insert(
            "review_needed".to_string(),
            vec![make_summary(
                "a~b~1",
                "review_needed",
                "other",
                Some(Priority::High),
            )],
        );
        groups.insert(
            "authored_waiting".to_string(),
            vec![make_summary("a~b~2", "authored_waiting", "me", None)],
        );

        let app = make_test_state(groups);
        let mut view = ViewStateManager::new();
        let layout = make_layout(80, 24, Blade::Inbox);
        view.prepare(&mut { app }, &layout);

        // app moved into closure is not usable; use a fresh call.
        let mut app = make_test_state({
            let mut g = HashMap::new();
            g.insert(
                "review_needed".to_string(),
                vec![make_summary(
                    "a~b~1",
                    "review_needed",
                    "other",
                    Some(Priority::High),
                )],
            );
            g.insert(
                "authored_waiting".to_string(),
                vec![make_summary("a~b~2", "authored_waiting", "me", None)],
            );
            g
        });
        view.prepare(&mut app, &layout);
        assert_eq!(view.view.flat_prs, vec!["a~b~2", "a~b~1"]);
    }

    #[test]
    fn prepare_resets_selection_when_pr_disappears() {
        let mut groups = HashMap::new();
        groups.insert(
            "review_needed".to_string(),
            vec![make_summary("a~b~1", "review_needed", "other", None)],
        );
        let mut app = make_test_state(groups);
        app.selected_pr_id = Some("a~b~1".to_string());

        let mut view = ViewStateManager::new();
        view.view.selected_index = 0;
        let layout = make_layout(80, 24, Blade::Inbox);
        view.prepare(&mut app, &layout);
        assert_eq!(app.selected_pr_id, Some("a~b~1".to_string()));
        assert!(!app.detail_needs_reload);

        // Now remove the PR and run prepare again.
        app.prs.groups.clear();
        view.prepare(&mut app, &layout);

        assert!(app.selected_pr_id.is_none());
        assert!(app.detail_needs_reload);
        assert!(app.diff_needs_reload);
        assert!(app.pr_detail.is_none());
        assert!(app.pr_diff.is_none());
        assert!(app.diff_lines.is_empty());
        assert!(app.diff_comments.is_empty());
        assert_eq!(view.view.selected_index, 0);
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
                "review_needed",
                "other",
                None,
            ));
        }
        groups.insert("review_needed".to_string(), prs);
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

    #[test]
    fn flat_index_to_line_index_accounts_for_section_headers() {
        // review only
        assert_eq!(flat_index_to_line_index(0, 0, 5), 1);
        assert_eq!(flat_index_to_line_index(4, 0, 5), 5);
        // opened only
        assert_eq!(flat_index_to_line_index(0, 5, 0), 1);
        assert_eq!(flat_index_to_line_index(4, 5, 0), 5);
        // both groups
        assert_eq!(flat_index_to_line_index(0, 3, 4), 1);
        assert_eq!(flat_index_to_line_index(2, 3, 4), 3);
        assert_eq!(flat_index_to_line_index(3, 3, 4), 5);
        assert_eq!(flat_index_to_line_index(6, 3, 4), 8);
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
