use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Child;

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tracing::info;

use crate::api::*;
use crate::config::Config;
use crate::tui::client::DaemonClient;
use crate::tui::event::{spawn_event_loop, TuiEvent};
use crate::tui::render::cache::RenderCache;
use crate::tui::render::chrome::InlineToast;
use crate::tui::render::component::{Component, RenderContext};
use crate::tui::render::layout::{Blade, RootLayout};
use crate::tui::render::theme::Theme;
use crate::tui::state::ViewStateManager;

/// Action returned by key handling for the render loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Refresh,
}

/// Central TUI application state. This struct holds domain and cache data only —
/// all transient view/scrolling state lives in `ViewStateManager`.
pub struct AppState {
    pub config: Config,
    pub client: DaemonClient,
    pub prs: PrListResponse,
    pub health: Option<HealthResponse>,

    /// Currently selected PR slug.
    pub selected_pr_id: Option<String>,
    /// Currently loaded PR detail.
    pub pr_detail: Option<PrDetailResponse>,
    /// Currently loaded diff response.
    pub pr_diff: Option<DiffResponse>,
    /// Parsed diff lines (kept for file-boundary navigation).
    pub diff_lines: Vec<crate::diff::render::ParsedDiffLine>,
    /// Mapping from diff line index to visible review comments.
    pub diff_comments: HashMap<usize, Vec<crate::api::ReviewCommentDto>>,
    /// Show line numbers in diff view.
    pub show_line_numbers: bool,
    /// Error message to display (toast).
    pub error_message: Option<String>,
    /// Daemon child process if we spawned it.
    pub daemon_child: Option<Child>,
    /// Loading state.
    pub loading: bool,
    /// Tracks whether selected PR changed and detail needs reload.
    pub detail_needs_reload: bool,
    /// Tracks whether the diff needs reload.
    pub diff_needs_reload: bool,
    /// Cached render artifacts (overview/activity/diff lines).
    pub render_cache: RenderCache,
}

impl AppState {
    pub fn new(config: Config, client: DaemonClient) -> Self {
        let show_line_numbers = config.tui.show_line_numbers;
        Self {
            config,
            client,
            prs: PrListResponse {
                groups: HashMap::new(),
                updated_at: String::new(),
            },
            health: None,
            selected_pr_id: None,
            pr_detail: None,
            pr_diff: None,
            diff_lines: Vec::new(),
            diff_comments: HashMap::new(),
            show_line_numbers,
            error_message: None,
            daemon_child: None,
            loading: false,
            detail_needs_reload: false,
            diff_needs_reload: false,
            render_cache: RenderCache::new(),
        }
    }

    /// Get the currently selected PR summary, if any.
    pub fn selected_pr(&self) -> Option<&PrSummary> {
        let id = self.selected_pr_id.as_ref()?;
        for prs in self.prs.groups.values() {
            if let Some(pr) = prs.iter().find(|p| &p.id == id) {
                return Some(pr);
            }
        }
        None
    }

    /// Clear parsed diff lines and mapped comments.
    pub fn clear_diff_cache(&mut self) {
        self.pr_diff = None;
        self.diff_lines = Vec::new();
        self.diff_comments = HashMap::new();
    }

    /// Move selection up/down within the Inbox blade.
    pub fn move_selection(&mut self, view: &mut ViewStateManager, delta: i32) {
        if view.view.flat_prs.is_empty() {
            return;
        }
        let new_idx = (view.view.selected_index as i32 + delta).max(0) as usize;
        view.view.selected_index = new_idx.min(view.view.flat_prs.len() - 1);
    }

    /// Move file selection up/down.
    pub fn move_file_selection(&mut self, view: &mut ViewStateManager, delta: i32) {
        let new_index = (view.view.selected_file_index as i32 + delta).max(0) as usize;
        self.set_selected_file_index(view, new_index);
    }

    pub fn set_selected_file_index(&mut self, view: &mut ViewStateManager, index: usize) {
        let last = self.selected_file_count().saturating_sub(1);
        let clamped = index.min(last);
        if clamped != view.view.selected_file_index {
            view.view.selected_file_index = clamped;
            self.clear_diff_cache();
            view.view.diff_scroll.scroll_to(0);
            self.diff_needs_reload = true;
        }
    }

    fn selected_file_count(&self) -> usize {
        self.pr_detail.as_ref().map_or(0, |d| d.files.len())
    }

    /// Scroll content for the active blade/focus.
    pub fn scroll_content(&mut self, view: &mut ViewStateManager, delta: i32) {
        view.view.active_scroll_mut().scroll_by(delta as isize);
    }

    /// Scroll the diff by full pages.
    pub fn page_diff(&mut self, view: &mut ViewStateManager, delta: i32) {
        let page = 20;
        if delta >= 0 {
            view.view.diff_scroll.scroll_by(page);
        } else {
            view.view.diff_scroll.scroll_by(-page);
        }
        self.sync_selected_file_to_diff_scroll(view);
    }

    pub fn scroll_diff_to_selected_file(&mut self, view: &mut ViewStateManager) {
        let boundaries = crate::diff::render::find_file_boundaries(&self.diff_lines);
        if let Some(&boundary) = boundaries.get(view.view.selected_file_index) {
            view.view.diff_scroll.scroll_to(boundary);
        }
    }

    pub fn sync_selected_file_to_diff_scroll(&mut self, view: &mut ViewStateManager) {
        let boundaries = crate::diff::render::find_file_boundaries(&self.diff_lines);
        if boundaries.is_empty() {
            return;
        }
        let file_index = boundaries
            .iter()
            .enumerate()
            .take_while(|(_, boundary)| **boundary <= view.view.diff_scroll.offset)
            .map(|(index, _)| index)
            .last()
            .unwrap_or(0);
        view.view.selected_file_index = file_index.min(boundaries.len() - 1);
    }

    pub fn jump_diff_file(&mut self, view: &mut ViewStateManager, delta: i32) {
        let boundaries = crate::diff::render::find_file_boundaries(&self.diff_lines);
        if boundaries.is_empty() {
            return;
        }
        self.sync_selected_file_to_diff_scroll(view);
        let current = view.view.selected_file_index.min(boundaries.len() - 1);
        let target = (current as i32 + delta).clamp(0, boundaries.len() as i32 - 1) as usize;
        view.view.selected_file_index = target;
        view.view.diff_scroll.scroll_to(boundaries[target]);
    }

    fn set_active_blade(&mut self, view: &mut ViewStateManager, target: Blade) {
        if view.view.active_blade == Blade::Diff && target != Blade::Diff {
            self.clear_diff_cache();
        }
        view.view.active_blade = target;
        // Clear transient toasts when switching blades so stale messages don't linger.
        self.error_message = None;
        if target == Blade::Diff {
            self.diff_needs_reload = true;
            self.scroll_diff_to_selected_file(view);
        }
        if target == Blade::Files {
            let idx = view.view.selected_file_index;
            self.set_selected_file_index(view, idx);
        }
    }

    /// Navigate to the next blade to the right (deeper).
    pub fn next_blade(&mut self, view: &mut ViewStateManager) {
        let next = view.view.active_blade.index() + 1;
        if next < Blade::count() {
            self.set_active_blade(view, Blade::from_index(next));
        }
    }

    /// Navigate to the previous blade to the left.
    pub fn prev_blade(&mut self, view: &mut ViewStateManager) {
        let prev = view.view.active_blade.index().saturating_sub(1);
        self.set_active_blade(view, Blade::from_index(prev));
    }

    /// Jump directly to a blade by number.
    pub fn jump_to_blade(&mut self, view: &mut ViewStateManager, n: usize) {
        if n == 0 || n > Blade::count() {
            return;
        }
        self.set_active_blade(view, Blade::from_index(n - 1));
    }

    /// Open the selected file's diff.
    pub fn open_selected_file_diff(&mut self, view: &mut ViewStateManager) {
        if self.selected_file_count() > 0 {
            self.set_active_blade(view, Blade::Diff);
        }
    }

    /// Open the PR in the browser.
    pub fn open_pr_in_browser(&mut self) {
        if let Some(detail) = &self.pr_detail {
            let _ = open::that(&detail.url);
        }
    }

    /// Handle a key event.
    pub fn handle_key(&mut self, view: &mut ViewStateManager, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.handle_ctrl_key(view, key);
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => return Action::Quit,
            KeyCode::Char('R') => {
                self.loading = true;
                self.error_message = None;
                return Action::Refresh;
            }
            KeyCode::Char('r') => {
                self.error_message = Some("Request changes stub".to_string());
            }
            KeyCode::Char('a') => {
                self.error_message = Some("Approve stub".to_string());
            }
            KeyCode::Char('m') => {
                self.error_message = Some("Merge stub".to_string());
            }
            KeyCode::Char('/') => {
                self.error_message = Some("Search stub".to_string());
            }
            KeyCode::Char('?') => {
                self.error_message = Some(
                    "Help: 1-5 jump blades, ←/→ or h/l/⏎ navigate, j/k ↑↓ scroll, Tab cycle overview sections, R refresh, q quit".to_string(),
                );
            }
            KeyCode::Char('n') if view.view.active_blade == Blade::Diff => {
                self.show_line_numbers = !self.show_line_numbers;
            }
            KeyCode::Char('o') | KeyCode::Char('O') => self.open_pr_in_browser(),
            KeyCode::Right | KeyCode::Char('l') => self.next_blade(view),
            KeyCode::Left | KeyCode::Char('h') => self.prev_blade(view),
            KeyCode::Esc => {
                self.error_message = None;
            }
            KeyCode::Enter => match view.view.active_blade {
                Blade::Inbox => self.set_active_blade(view, Blade::Overview),
                Blade::Overview => self.set_active_blade(view, Blade::Activity),
                Blade::Activity => self.set_active_blade(view, Blade::Files),
                Blade::Files => self.open_selected_file_diff(view),
                Blade::Diff => {}
            },
            KeyCode::Char('1') => self.jump_to_blade(view, 1),
            KeyCode::Char('2') => self.jump_to_blade(view, 2),
            KeyCode::Char('3') => self.jump_to_blade(view, 3),
            KeyCode::Char('4') => self.jump_to_blade(view, 4),
            KeyCode::Char('5') => self.jump_to_blade(view, 5),
            KeyCode::Down | KeyCode::Char('j') => self.handle_down(view),
            KeyCode::Up | KeyCode::Char('k') => self.handle_up(view),
            KeyCode::Char(' ') if view.view.active_blade == Blade::Inbox => {
                self.toggle_collapse_current(view)
            }
            KeyCode::Char('g') if view.view.active_blade == Blade::Diff => {
                view.view.diff_scroll.scroll_to(0);
            }
            KeyCode::Char('G') if view.view.active_blade == Blade::Diff => {
                let max = view.view.diff_scroll.max_scroll();
                view.view.diff_scroll.scroll_to(max);
                self.sync_selected_file_to_diff_scroll(view);
            }
            KeyCode::Tab if view.view.active_blade == Blade::Overview => {
                view.view.overview_focus = view.view.overview_focus.next();
            }
            KeyCode::BackTab if view.view.active_blade == Blade::Overview => {
                view.view.overview_focus = view.view.overview_focus.prev();
            }
            KeyCode::Tab if view.view.active_blade == Blade::Diff => self.jump_diff_file(view, 1),
            KeyCode::BackTab if view.view.active_blade == Blade::Diff => {
                self.jump_diff_file(view, -1)
            }
            _ => {}
        }
        Action::None
    }

    fn handle_ctrl_key(&mut self, view: &mut ViewStateManager, key: KeyEvent) -> Action {
        if view.view.active_blade == Blade::Diff {
            match key.code {
                KeyCode::Char('d') => self.page_diff(view, 1),
                KeyCode::Char('u') => self.page_diff(view, -1),
                _ => {}
            }
        }
        Action::None
    }

    fn handle_down(&mut self, view: &mut ViewStateManager) {
        match view.view.active_blade {
            Blade::Inbox => self.move_selection(view, 1),
            Blade::Files => self.move_file_selection(view, 1),
            Blade::Diff => {
                view.view.diff_scroll.scroll_by(1);
                self.sync_selected_file_to_diff_scroll(view);
            }
            _ => self.scroll_content(view, 1),
        }
    }

    fn handle_up(&mut self, view: &mut ViewStateManager) {
        match view.view.active_blade {
            Blade::Inbox => self.move_selection(view, -1),
            Blade::Files => self.move_file_selection(view, -1),
            Blade::Diff => {
                view.view.diff_scroll.scroll_by(-1);
                self.sync_selected_file_to_diff_scroll(view);
            }
            _ => self.scroll_content(view, -1),
        }
    }

    /// Toggle collapse on the group containing the current PR.
    pub fn toggle_collapse_current(&mut self, view: &mut ViewStateManager) {
        if let Some(id) = view.view.flat_prs.get(view.view.selected_index) {
            for group in crate::github::types::PrGroup::all_in_priority_order() {
                let key = crate::api::group_key(group);
                if let Some(prs) = self.prs.groups.get(&key) {
                    if prs.iter().any(|p| &p.id == id) {
                        let collapsed = view.view.collapsed_groups.entry(key).or_insert(false);
                        *collapsed = !*collapsed;
                        return;
                    }
                }
            }
        } else {
            for group in crate::github::types::PrGroup::all_in_priority_order() {
                let key = crate::api::group_key(group);
                if let Some(prs) = self.prs.groups.get(&key) {
                    if !prs.is_empty() && *view.view.collapsed_groups.get(&key).unwrap_or(&false) {
                        view.view.collapsed_groups.insert(key, false);
                        return;
                    }
                }
            }
        }
    }

    /// Fetch fresh PR data from daemon.
    pub async fn refresh_data(&mut self) {
        match self.client.get_prs().await {
            Ok(resp) => {
                self.prs = resp;
                self.loading = false;
                let all_ids: HashSet<&String> = self
                    .prs
                    .groups
                    .values()
                    .flat_map(|prs| prs.iter().map(|p| &p.id))
                    .collect();
                if let Some(ref id) = self.selected_pr_id {
                    if !all_ids.contains(id) {
                        self.selected_pr_id = None;
                        self.detail_needs_reload = true;
                        self.diff_needs_reload = true;
                    }
                }
            }
            Err(e) => {
                self.error_message = Some(format!("Failed to fetch PRs: {}", e));
                self.loading = false;
            }
        }

        if let Ok(h) = self.client.get_health().await {
            self.health = Some(h);
        }
    }

    /// Trigger manual refresh on daemon.
    pub async fn trigger_refresh(&mut self) {
        if let Err(e) = self.client.refresh().await {
            self.error_message = Some(format!("Refresh failed: {}", e));
        }
    }

    /// Load PR detail from daemon.
    pub async fn load_detail(&mut self) {
        if let Some(id) = &self.selected_pr_id {
            match self.client.get_pr_detail(id).await {
                Ok(detail) => self.pr_detail = Some(detail),
                Err(e) => self.error_message = Some(format!("Failed to load detail: {}", e)),
            }
        } else {
            self.pr_detail = None;
        }
        self.detail_needs_reload = false;
    }

    /// Load diff from daemon and map review comments inline.
    pub async fn load_diff(&mut self) {
        if let Some(id) = &self.selected_pr_id {
            match self.client.get_pr_diff(id).await {
                Ok(diff_resp) => {
                    self.diff_lines = crate::diff::render::parse_diff(&diff_resp.diff);
                    self.pr_diff = Some(diff_resp);

                    if let Some(detail) = &self.pr_detail {
                        self.diff_comments =
                            crate::diff::render::map_review_threads_to_diff_indices(
                                &detail.review_threads,
                                &self.diff_lines,
                            );
                    }

                    // Make sure the selected file boundary is visible. The actual scroll
                    // clamping and viewport sizing happens in ViewStateManager::prepare.
                }
                Err(e) => self.error_message = Some(format!("Failed to load diff: {}", e)),
            }
        }
        self.diff_needs_reload = false;
    }
}

/// Run the full TUI application.
pub async fn run_tui(config: Config) -> Result<()> {
    run_tui_with_config_path(config, None).await
}

/// Run TUI with an optional config path (for daemon spawn forwarding).
pub async fn run_tui_with_config_path(config: Config, config_path: Option<PathBuf>) -> Result<()> {
    let port = config.daemon.port;

    let client = DaemonClient::new(port)?;
    let daemon_child = ensure_daemon(&client, port, config_path.as_deref()).await?;

    let mut state = AppState::new(config, client);
    state.daemon_child = daemon_child;

    state.refresh_data().await;

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = std::io::stdout().execute(Show);
        let _ = std::io::stdout().execute(LeaveAlternateScreen);
        let _ = disable_raw_mode();
        original_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(Hide)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = run_render_loop(&mut terminal, &mut state).await;

    let mut stdout = std::io::stdout();
    stdout.execute(Show)?;
    stdout.execute(LeaveAlternateScreen)?;
    disable_raw_mode()?;

    if state.config.daemon.kill_on_tui_exit {
        if let Some(mut child) = state.daemon_child.take() {
            let _ = child.kill();
            let _ = child.wait();
            info!("Killed spawned daemon");
        }
    }

    result
}

async fn ensure_daemon(
    client: &DaemonClient,
    _port: u16,
    config_path: Option<&std::path::Path>,
) -> Result<Option<Child>> {
    if let Some(health) = client.check_health().await {
        if health.service == crate::daemon::SERVICE_NAME {
            info!("Daemon already running");
            return Ok(None);
        }
    }

    info!("Spawning daemon...");
    let data_dir = crate::config::data_dir()?;
    std::fs::create_dir_all(&data_dir)?;
    let log_path = data_dir.join("daemon.log");
    let log_file = std::fs::File::create(&log_path)?;

    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("Cannot find current executable: {}", e))?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("daemon")
        .stdout(std::process::Stdio::from(log_file.try_clone()?))
        .stderr(std::process::Stdio::from(log_file));

    if let Some(path) = config_path {
        cmd.arg("--config").arg(path);
    }

    let mut child = cmd.spawn().map_err(|e| {
        anyhow::anyhow!(
            "Failed to spawn daemon: {}. Check log: {}",
            e,
            log_path.display()
        )
    })?;

    let max_wait = std::time::Duration::from_secs(10);
    let start = std::time::Instant::now();
    while start.elapsed() < max_wait {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(anyhow::anyhow!(
                    "Daemon exited early with status {}. Check log: {}",
                    status,
                    log_path.display()
                ));
            }
            Ok(None) => {}
            Err(e) => return Err(anyhow::anyhow!("Failed to check daemon status: {}", e)),
        }

        if let Some(health) = client.check_health().await {
            if health.service == crate::daemon::SERVICE_NAME {
                info!("Daemon is ready");
                return Ok(Some(child));
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    let _ = child.kill();
    let _ = child.wait();
    Err(anyhow::anyhow!(
        "Daemon did not become ready within 10s. Check log: {}",
        log_path.display()
    ))
}

/// Render one frame of the TUI.
pub fn render_frame(f: &mut ratatui::Frame, state: &mut AppState, view: &mut ViewStateManager) {
    let area = f.area();

    // RootLayout::render fills the whole terminal with BASE, so every cell is
    // painted explicitly — no manual clear_area / skip-flag reset is needed.
    let layout = RootLayout::new(view.view.active_blade).render(f, area);

    // Reconcile view state with domain/cached data and clamp scroll offsets.
    view.prepare(state, &layout);

    let theme = Theme::new(state.config.tui.osc8_links);
    let ctx = RenderContext::new(state, &view.view, &theme);

    // Chrome.
    crate::tui::views::chrome::render_command_line(f, layout.command_line, &ctx);
    crate::tui::views::chrome::render_keybar(f, layout.keybar, &ctx);

    // Active blade content.
    let active_area = layout.active_content();
    match view.view.active_blade {
        Blade::Inbox => crate::tui::views::inbox::render_inbox(f, active_area, &ctx),
        Blade::Overview => crate::tui::views::overview::render_overview(f, active_area, &ctx),
        Blade::Activity => crate::tui::views::activity::render_activity(f, active_area, &ctx),
        Blade::Files => crate::tui::views::files::render_files(f, active_area, &ctx),
        Blade::Diff => crate::tui::views::diff::render_diff(f, active_area, &ctx),
    }

    // Error/action-stub feedback renders as a centered overlay over the body.
    // The keybar remains visible with its bindings at all times.
    InlineToast.render(f, layout.body, &ctx);
}

async fn run_render_loop(
    terminal: &mut ratatui::DefaultTerminal,
    state: &mut AppState,
) -> Result<()> {
    let (mut event_rx, event_handle) = spawn_event_loop();
    let mut view = ViewStateManager::new();

    loop {
        if state.detail_needs_reload {
            state.load_detail().await;
        }
        if state.diff_needs_reload {
            state.load_diff().await;
            state.scroll_diff_to_selected_file(&mut view);
        }

        terminal.draw(|f| render_frame(f, state, &mut view))?;

        let event = match event_rx.recv().await {
            Some(e) => e,
            None => break,
        };

        match event {
            TuiEvent::Key(key) => {
                let action = state.handle_key(&mut view, key);
                match action {
                    Action::Quit => break,
                    Action::Refresh => {
                        state.trigger_refresh().await;
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        state.refresh_data().await;
                        state.loading = false;
                    }
                    Action::None => {
                        if state.loading {
                            state.trigger_refresh().await;
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            state.refresh_data().await;
                            state.loading = false;
                        }
                    }
                }
            }
            TuiEvent::Resize(_, _) => {}
            TuiEvent::DataTick => {
                state.refresh_data().await;
            }
            TuiEvent::UiTick => {}
        }
    }

    event_handle.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::types::Priority;

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
        });
        state
    }

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

    #[test]
    fn test_move_selection_updates_detail_flag() {
        let mut groups = HashMap::new();
        groups.insert(
            "review_needed".to_string(),
            vec![
                make_summary("a~b~1", "review_needed", "other", None),
                make_summary("a~b~2", "review_needed", "other", None),
            ],
        );

        let mut state = make_test_state(groups);
        let mut view = ViewStateManager::new();
        let layout =
            RootLayout::new(Blade::Inbox).compute(ratatui::layout::Rect::new(0, 0, 80, 24));
        view.prepare(&mut state, &layout);

        state.move_selection(&mut view, 1);
        view.prepare(&mut state, &layout);
        assert_eq!(view.view.selected_index, 1);
        assert_eq!(state.selected_pr_id, Some("a~b~2".to_string()));
        assert!(state.detail_needs_reload);
        assert!(state.diff_needs_reload);
    }

    #[test]
    fn test_blade_navigation_clamps() {
        let mut groups = HashMap::new();
        groups.insert(
            "review_needed".to_string(),
            vec![make_summary("a~b~1", "review_needed", "other", None)],
        );

        let mut state = make_test_state(groups);
        let mut view = ViewStateManager::new();
        assert_eq!(view.view.active_blade, Blade::Inbox);

        state.prev_blade(&mut view); // cannot go left from Inbox
        assert_eq!(view.view.active_blade, Blade::Inbox);

        for _ in 0..6 {
            state.next_blade(&mut view);
        }
        assert_eq!(view.view.active_blade, Blade::Diff);
    }

    #[test]
    fn test_jump_to_blade_number() {
        let mut groups = HashMap::new();
        groups.insert(
            "review_needed".to_string(),
            vec![make_summary("a~b~1", "review_needed", "other", None)],
        );

        let mut state = make_test_state(groups);
        let mut view = ViewStateManager::new();
        state.jump_to_blade(&mut view, 3);
        assert_eq!(view.view.active_blade, Blade::Activity);
        state.jump_to_blade(&mut view, 1);
        assert_eq!(view.view.active_blade, Blade::Inbox);
    }

    #[test]
    fn test_quit_key() {
        let groups = HashMap::new();
        let mut state = make_test_state(groups);
        let mut view = ViewStateManager::new();

        let action = state.handle_key(
            &mut view,
            KeyEvent::new(
                crossterm::event::KeyCode::Char('q'),
                crossterm::event::KeyModifiers::NONE,
            ),
        );
        assert_eq!(action, Action::Quit);
    }

    #[test]
    fn test_right_left_navigate_blades() {
        let mut groups = HashMap::new();
        groups.insert(
            "review_needed".to_string(),
            vec![make_summary("a~b~1", "review_needed", "other", None)],
        );

        let mut state = make_test_state(groups);
        let mut view = ViewStateManager::new();
        state.selected_pr_id = Some("a~b~1".to_string());
        assert_eq!(view.view.active_blade, Blade::Inbox);

        state.handle_key(
            &mut view,
            KeyEvent::new(
                crossterm::event::KeyCode::Right,
                crossterm::event::KeyModifiers::NONE,
            ),
        );
        assert_eq!(view.view.active_blade, Blade::Overview);

        state.handle_key(
            &mut view,
            KeyEvent::new(
                crossterm::event::KeyCode::Left,
                crossterm::event::KeyModifiers::NONE,
            ),
        );
        assert_eq!(view.view.active_blade, Blade::Inbox);
    }

    #[test]
    fn overview_frame_does_not_leak_inbox_content_after_blade_switch() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut groups = HashMap::new();
        groups.insert(
            "review_needed".to_string(),
            vec![
                make_summary("org~repo~1", "review_needed", "other", Some(Priority::High)),
                make_summary(
                    "org~repo~2",
                    "review_needed",
                    "other2",
                    Some(Priority::Medium),
                ),
            ],
        );
        groups.insert(
            "authored_waiting".to_string(),
            vec![make_summary("org~repo~3", "authored_waiting", "me", None)],
        );

        let mut state = make_test_state(groups);
        state.config.tui.osc8_links = true;
        state.selected_pr_id = Some("org~repo~1".to_string());
        state.pr_detail = Some(crate::api::PrDetailResponse {
            id: "org~repo~1".to_string(),
            node_id: "n1".to_string(),
            owner: "org".to_string(),
            repo: "repo".to_string(),
            number: 35847,
            title: "feat(slack-service): allow channel connections to change investigation"
                .to_string(),
            body: "## Summary\n\n- add an in-place Slack channel connection rebind operation."
                .to_string(),
            url: "https://github.com/org/repo/pull/35847".to_string(),
            author: "austin".to_string(),
            is_draft: false,
            updated_at: "2024-05-20T12:00:00Z".to_string(),
            head_ref: "1718-slack-channel-rebind".to_string(),
            base_ref: "main".to_string(),
            mergeable: "MERGEABLE".to_string(),
            review_decision: None,
            review_requests: vec![],
            viewer_latest_review: None,
            latest_reviews: vec![],
            check_status: "pending".to_string(),
            checks: vec![crate::api::CheckEntryDto {
                name: "ci".to_string(),
                status: "IN_PROGRESS".to_string(),
                conclusion: None,
                url: "https://github.com/org/repo/pull/35847/checks".to_string(),
            }],
            review_threads: vec![],
            files: vec![
                crate::api::FileDto {
                    path: "src/main.rs".to_string(),
                    additions: 1290,
                    deletions: 34,
                    status: 'M',
                },
                crate::api::FileDto {
                    path: "terraform/refinery-as-a-service/us1/_griztest-poc.tf".to_string(),
                    additions: 18,
                    deletions: 0,
                    status: 'A',
                },
            ],
            timeline: vec![],
            llm_priority: Some(Priority::Medium),
            llm_summary: Some(
                "Implements Slack channel connection rebinding and new Slack controls/commands."
                    .to_string(),
            ),
        });

        let backend = TestBackend::new(124, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut view = ViewStateManager::new();
        // Ensure the selected index matches the selected PR id so detail is not cleared by prepare.
        view.view.selected_index = 1;
        view.view.active_blade = Blade::Inbox;
        terminal
            .draw(|f| crate::tui::app::render_frame(f, &mut state, &mut view))
            .unwrap();

        view.view.active_blade = Blade::Overview;
        terminal
            .draw(|f| crate::tui::app::render_frame(f, &mut state, &mut view))
            .unwrap();

        let buf = terminal.backend().buffer();
        let active_rect = crate::tui::render::layout::RootLayout::new(Blade::Overview)
            .compute(ratatui::layout::Rect::new(0, 0, 124, 32))
            .blade(Blade::Overview)
            .content;

        let mut content = String::new();
        for y in active_rect.top()..active_rect.bottom() {
            for x in active_rect.left()..active_rect.right() {
                content.push_str(buf.cell((x, y)).unwrap().symbol());
            }
            content.push('\n');
        }
        assert!(
            !content.contains("OPENED BY ME"),
            "active Overview blade leaked Inbox section header:\n{}",
            content
        );
        assert!(
            !content.contains("NEEDS MY REVIEW"),
            "active Overview blade leaked Inbox section header:\n{}",
            content
        );
        assert!(content.contains("SUMMARY"));
    }

    #[test]
    fn overview_111x30_no_inbox_leak_after_blade_switch() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut groups = HashMap::new();
        groups.insert(
            "review_needed".to_string(),
            vec![
                make_summary("org~repo~1", "review_needed", "other", Some(Priority::High)),
                make_summary(
                    "org~repo~2",
                    "review_needed",
                    "other2",
                    Some(Priority::Medium),
                ),
            ],
        );
        groups.insert(
            "authored_waiting".to_string(),
            vec![make_summary("org~repo~3", "authored_waiting", "me", None)],
        );
        groups.insert(
            "authored_ready_to_merge".to_string(),
            vec![make_summary(
                "org~repo~4",
                "authored_ready_to_merge",
                "me",
                None,
            )],
        );
        groups.insert(
            "authored_action_needed".to_string(),
            vec![make_summary(
                "org~repo~5",
                "authored_action_needed",
                "me",
                None,
            )],
        );

        let mut state = make_test_state(groups);
        state.config.tui.osc8_links = true;
        state.selected_pr_id = Some("org~repo~1".to_string());

        let backend = TestBackend::new(111, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut view = ViewStateManager::new();

        view.view.active_blade = Blade::Inbox;
        terminal
            .draw(|f| crate::tui::app::render_frame(f, &mut state, &mut view))
            .unwrap();

        view.view.active_blade = Blade::Overview;
        terminal
            .draw(|f| crate::tui::app::render_frame(f, &mut state, &mut view))
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut content = String::new();
        for x in 0..buf.area().width {
            content.push_str(buf.cell((x, 2)).unwrap().symbol());
        }
        assert!(
            !content.contains("OPENED BY ME"),
            "row 2 leaked Inbox header at 111x30: {:?}",
            content
        );
    }

    #[test]
    fn keybar_renders_bindings_not_only_border() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let groups = HashMap::new();
        let mut state = make_test_state(groups);
        let mut view = ViewStateManager::new();
        view.view.active_blade = Blade::Files;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| crate::tui::app::render_frame(f, &mut state, &mut view))
            .unwrap();

        let buf = terminal.backend().buffer();
        let row = buf.area().height - 1;
        let line: String = (0..buf.area().width)
            .map(|x| buf.cell((x, row)).unwrap().symbol().to_string())
            .collect();
        assert!(
            line.contains("blade") && line.contains("jump") && line.contains("quit"),
            "keybar should show bindings, got: {:?}",
            line
        );
    }

    #[test]
    #[ignore]
    #[allow(deprecated)]
    fn dump_111x30_overview_after_inbox() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut groups = HashMap::new();
        groups.insert(
            "review_needed".to_string(),
            vec![
                make_summary("org~repo~1", "review_needed", "other", Some(Priority::High)),
                make_summary(
                    "org~repo~2",
                    "review_needed",
                    "other2",
                    Some(Priority::Medium),
                ),
            ],
        );
        groups.insert(
            "authored_waiting".to_string(),
            vec![make_summary("org~repo~3", "authored_waiting", "me", None)],
        );

        let mut state = make_test_state(groups);
        state.config.tui.osc8_links = true;
        state.selected_pr_id = Some("org~repo~1".to_string());
        state.pr_detail = Some(crate::api::PrDetailResponse {
            id: "org~repo~1".to_string(),
            node_id: "n1".to_string(),
            owner: "org".to_string(),
            repo: "repo".to_string(),
            number: 35847,
            title: "feat(slack-service): allow channel connections to change investigation"
                .to_string(),
            body: "## Summary\n\n- add an in-place Slack channel connection rebind operation."
                .to_string(),
            url: "https://github.com/org/repo/pull/35847".to_string(),
            author: "austin".to_string(),
            is_draft: false,
            updated_at: "2024-05-20T12:00:00Z".to_string(),
            head_ref: "1718-slack-channel-rebind".to_string(),
            base_ref: "main".to_string(),
            mergeable: "MERGEABLE".to_string(),
            review_decision: None,
            review_requests: vec![],
            viewer_latest_review: None,
            latest_reviews: vec![],
            check_status: "success".to_string(),
            checks: vec![crate::api::CheckEntryDto {
                name: "ci".to_string(),
                status: "COMPLETED".to_string(),
                conclusion: Some("SUCCESS".to_string()),
                url: "https://github.com/org/repo/pull/35847/checks".to_string(),
            }],
            review_threads: vec![],
            files: vec![
                crate::api::FileDto {
                    path: "src/main.rs".to_string(),
                    additions: 1290,
                    deletions: 34,
                    status: 'M',
                },
                crate::api::FileDto {
                    path: "terraform/refinery-as-a-service/us1/_griztest-poc.tf".to_string(),
                    additions: 18,
                    deletions: 0,
                    status: 'A',
                },
            ],
            timeline: vec![],
            llm_priority: Some(Priority::Medium),
            llm_summary: Some(
                "Implements Slack channel connection rebinding and new Slack controls/commands."
                    .to_string(),
            ),
        });

        let backend = TestBackend::new(111, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut view = ViewStateManager::new();
        view.view.active_blade = Blade::Inbox;
        terminal
            .draw(|f| render_frame(f, &mut state, &mut view))
            .unwrap();
        view.view.active_blade = Blade::Overview;
        terminal.clear().unwrap();
        terminal
            .draw(|f| render_frame(f, &mut state, &mut view))
            .unwrap();

        let buf = terminal.backend().buffer();
        eprintln!("\n--- row 2 cells (y=2) ---");
        for x in 0..buf.area().width {
            let cell = buf.cell((x, 2)).unwrap();
            eprintln!("x={:3} sym={:?} skip={}", x, cell.symbol(), cell.skip);
        }
        eprintln!("--- row 1 cells (y=1) ---");
        for x in 0..buf.area().width {
            let cell = buf.cell((x, 1)).unwrap();
            eprintln!("x={:3} sym={:?} skip={}", x, cell.symbol(), cell.skip);
        }
        let mut out = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                out.push_str(buf.cell((x, y)).unwrap().symbol());
            }
            out.push('\n');
        }
        std::fs::write("render_dump_111x30_overview.txt", out).unwrap();
    }

    #[test]
    fn inbox_selected_row_is_highlighted_full_width() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut groups = HashMap::new();
        groups.insert(
            "review_needed".to_string(),
            vec![
                make_summary("org~repo~1", "review_needed", "other", Some(Priority::High)),
                make_summary(
                    "org~repo~2",
                    "review_needed",
                    "other2",
                    Some(Priority::Medium),
                ),
                make_summary("org~repo~3", "review_needed", "other3", None),
            ],
        );

        // The selected row must carry the selection background across the full
        // blade width. PR/file titles are deliberately not hyperlinked (those
        // overlays corrupted cell widths and broke the highlight), so the OSC 8
        // setting must not matter.
        for osc8 in [false, true] {
            let mut state = make_test_state(groups.clone());
            state.config.tui.osc8_links = osc8;
            let backend = TestBackend::new(120, 30);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut view = ViewStateManager::new();
            view.view.active_blade = Blade::Inbox;
            terminal
                .draw(|f| render_frame(f, &mut state, &mut view))
                .unwrap();

            let buf = terminal.backend().buffer();
            let layout = crate::tui::render::layout::RootLayout::new(Blade::Inbox)
                .compute(ratatui::layout::Rect::new(0, 0, 120, 28));
            let content = layout.active_content();
            let selected_y = find_selected_row(buf, content);

            let mut bad = 0u32;
            let mut report = String::new();
            for x in content.left()..content.right() {
                let bg = buf.cell((x, selected_y)).unwrap().style().bg;
                if !matches!(bg, Some(crate::tui::render::theme::SURFACE0)) {
                    bad += 1;
                    report.push_str(&format!("x={x} bg={bg:?}\n"));
                }
            }
            assert_eq!(
                bad, 0,
                "selected row not fully highlighted (osc8={osc8}):\n{report}"
            );
        }
    }

    fn find_selected_row(buf: &ratatui::buffer::Buffer, content: ratatui::layout::Rect) -> u16 {
        for y in content.top()..content.bottom() {
            if buf.cell((content.x, y)).unwrap().symbol() == "▌" {
                return y;
            }
        }
        panic!("no selected row (▌ bar) found in inbox content");
    }

    #[test]
    fn files_scroll_follows_selection_when_list_overflows_viewport() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut groups = HashMap::new();
        groups.insert(
            "review_needed".to_string(),
            vec![make_summary("org~repo~42", "review_needed", "other", None)],
        );
        let mut state = make_test_state(groups);
        state.selected_pr_id = Some("org~repo~42".to_string());
        state.pr_detail = Some(crate::api::PrDetailResponse {
            id: "org~repo~42".to_string(),
            node_id: "n1".to_string(),
            owner: "org".to_string(),
            repo: "repo".to_string(),
            number: 42,
            title: "Test PR".to_string(),
            body: String::new(),
            url: "https://example.com".to_string(),
            author: "me".to_string(),
            is_draft: false,
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            head_ref: "feature".to_string(),
            base_ref: "main".to_string(),
            mergeable: "MERGEABLE".to_string(),
            review_decision: None,
            review_requests: vec![],
            viewer_latest_review: None,
            latest_reviews: vec![],
            check_status: "none".to_string(),
            checks: vec![],
            review_threads: vec![],
            files: (0..30)
                .map(|i| crate::api::FileDto {
                    path: format!("src/file_{:02}.rs", i),
                    additions: i as u64,
                    deletions: 1,
                    status: 'M',
                })
                .collect(),
            timeline: vec![],
            llm_priority: None,
            llm_summary: None,
        });

        let mut view = ViewStateManager::new();
        view.view.active_blade = Blade::Files;
        view.view.selected_file_index = 29;
        let layout =
            RootLayout::new(Blade::Files).compute(ratatui::layout::Rect::new(0, 0, 111, 30));
        view.prepare(&mut state, &layout);

        // Active blade content height is 26; the Files blade reserves one row
        // for its header, so the scrollable viewport is 25 rows. Selecting the
        // last file should scroll so that it is the last visible row (offset 5).
        assert_eq!(
            view.view.files_scroll.offset, 5,
            "files_scroll should follow selection to the last page"
        );

        let backend = TestBackend::new(111, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_frame(f, &mut state, &mut view))
            .unwrap();

        // The scrolled viewport should start at file_03, so file_00 must not
        // be visible in the Files body area.
        let buf = terminal.backend().buffer();
        let layout =
            RootLayout::new(Blade::Files).compute(ratatui::layout::Rect::new(0, 0, 111, 30));
        let content = layout.active_content();
        let mut found_file_00 = false;
        for y in content.top()..content.bottom() {
            let line: String = (content.left()..content.right())
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect();
            if line.contains("file_00") {
                found_file_00 = true;
                break;
            }
        }
        assert!(
            !found_file_00,
            "file_00 should be scrolled off the top of the viewport"
        );
    }

    #[test]
    fn inbox_scroll_follows_selection_when_list_overflows_viewport() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut groups = HashMap::new();
        groups.insert(
            "review_needed".to_string(),
            (0..30)
                .map(|i| crate::api::PrSummary {
                    id: format!("org~repo~{}", i),
                    node_id: "n1".to_string(),
                    owner: "org".to_string(),
                    repo: "repo".to_string(),
                    number: i as u64,
                    title: format!("PR {}", i),
                    author: "other".to_string(),
                    group: "review_needed".to_string(),
                    next_action: "Review now".to_string(),
                    check_status: "none".to_string(),
                    llm_priority: None,
                    updated_at: "2024-01-01T00:00:00Z".to_string(),
                    url: "https://example.com".to_string(),
                    comments: 0,
                })
                .collect(),
        );
        let mut state = make_test_state(groups);
        state.selected_pr_id = Some("org~repo~29".to_string());

        let mut view = ViewStateManager::new();
        view.view.active_blade = Blade::Inbox;
        view.view.selected_index = 29;
        let layout =
            RootLayout::new(Blade::Inbox).compute(ratatui::layout::Rect::new(0, 0, 111, 30));
        view.prepare(&mut state, &layout);

        // Active blade content height is 26; the Inbox blade reserves one row
        // for its column header, so the scrollable viewport is 25 rows. With 30
        // PRs plus one section header there are 31 content lines; selecting the
        // last PR (line index 30) should scroll so it is the last visible row.
        assert_eq!(
            view.view.inbox_scroll.offset, 6,
            "inbox_scroll should follow selection to the last page"
        );

        let backend = TestBackend::new(111, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_frame(f, &mut state, &mut view))
            .unwrap();

        let buf = terminal.backend().buffer();
        let layout =
            RootLayout::new(Blade::Inbox).compute(ratatui::layout::Rect::new(0, 0, 111, 30));
        let content = layout.active_content();
        let body = {
            let chunks = ratatui::layout::Layout::default()
                .direction(ratatui::layout::Direction::Vertical)
                .constraints([
                    ratatui::layout::Constraint::Length(1),
                    ratatui::layout::Constraint::Min(1),
                ])
                .split(content);
            chunks[1]
        };

        let mut found_pr_00 = false;
        for y in body.top()..body.bottom() {
            let line: String = (body.left()..body.right())
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect();
            if line.contains("PR 0") {
                found_pr_00 = true;
                break;
            }
        }
        assert!(
            !found_pr_00,
            "PR 0 should be scrolled off the top of the viewport"
        );
    }
}
