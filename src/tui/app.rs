use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::api::*;
use crate::config::Config;
use crate::tui::client::DaemonClient;
use crate::tui::event::{spawn_event_loop, ConfigRefreshBundle, StartupLoad, TuiEvent};
use crate::tui::render::cache::RenderCache;
use crate::tui::render::chrome::InlineToast;
use crate::tui::render::component::{Component, RenderContext};
use crate::tui::render::layout::{Blade, RootLayout};
use crate::tui::render::theme::Theme;
use crate::tui::state::{InboxCursor, InboxRow, InboxSection, ViewStateManager};
use crate::tui::wizard::{self, SetupWizardState, WizardStep};

/// Action returned by key handling for the render loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Refresh,
    ReloadConfig,
    ToggleConfig,
    OpenWizard,
    CloseWizard,
}

/// Whether the TUI is still bootstrapping (daemon start, initial fetches)
/// or has real data to show. Previously this cycled through several named
/// phases on a timer purely for visual variety; that was fake progress
/// (the label advanced regardless of what had actually completed), so it's
/// been collapsed to the two states that are actually true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupPhase {
    Starting,
    Ready,
}

fn copy_to_clipboard(text: &str) -> std::result::Result<(), String> {
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[("pbcopy", &[])];
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &[&str])] = &[("clip", &[])];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];

    let mut last_error = None;
    for (program, args) in candidates {
        let mut child = match Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                last_error = Some(format!("could not start {program}: {error}"));
                continue;
            }
        };

        let Some(mut stdin) = child.stdin.take() else {
            last_error = Some(format!("could not open {program} input"));
            continue;
        };
        if let Err(error) = stdin.write_all(text.as_bytes()) {
            last_error = Some(format!("could not write to {program}: {error}"));
            let _ = child.kill();
            let _ = child.wait();
            continue;
        }
        drop(stdin);

        match child.wait() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                last_error = Some(format!("{program} exited with {status}"));
            }
            Err(error) => {
                last_error = Some(format!("could not wait for {program}: {error}"));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| "no supported clipboard utility was found".to_string()))
}

pub struct AppState {
    pub config: Config,
    pub client: DaemonClient,
    /// Original config path passed on the CLI.
    pub config_path: Option<PathBuf>,
    pub prs: PrListResponse,
    pub health: Option<HealthResponse>,
    /// Last fetched daemon setup status.
    pub setup_status: Option<crate::api::SetupStatusResponse>,
    /// When `Some`, the setup wizard overlay replaces the dashboard. Auto-opens
    /// on first run (`!setup_status.ready`) and is reachable on demand via `w`.
    pub setup_wizard: Option<Box<SetupWizardState>>,
    /// When true, show the configuration inspector overlay.
    pub show_config_view: bool,
    /// Scroll offset for the configuration inspector overlay.
    pub config_scroll: usize,
    /// When true, show the inbox filter search overlay.
    pub show_search: bool,
    /// Active case-insensitive filter substring for the inbox.
    pub search_filter: String,
    /// The filter string captured when the search overlay was opened, restored
    /// verbatim when the overlay is cancelled with Esc.
    pub search_filter_saved: String,
    /// When true, show the keybinding help modal.
    pub show_help: bool,
    /// Scroll offset for the help modal (only meaningful when it overflows).
    pub help_scroll: usize,

    /// Currently selected PR slug.
    pub selected_pr_id: Option<String>,
    /// Currently loaded PR detail.
    pub pr_detail: Option<PrDetailResponse>,
    /// Currently loaded diff response. `render_cache` is the sole parser of
    /// its `diff` text (see `RenderCache::rebuild_diff`); nothing here keeps
    /// a second parsed copy.
    pub pr_diff: Option<DiffResponse>,
    /// Show line numbers in diff view.
    pub show_line_numbers: bool,
    /// Error message to display (toast).
    pub error_message: Option<String>,
    /// Daemon child process if we spawned it.
    pub daemon_child: Option<Child>,
    /// Loading state.
    pub loading: bool,
    /// Initial loading phase displayed while the TUI is bootstrapping.
    pub startup_phase: StartupPhase,
    /// True while a PR list refresh (periodic or manual) is in flight.
    /// Guards against DataTick/`R` piling up duplicate fetches against a
    /// slow or wedged daemon.
    pub refresh_inflight: bool,
    /// Monotonic UI animation tick.
    pub ui_tick: u64,
    /// Tracks whether selected PR changed and detail needs reload.
    pub detail_needs_reload: bool,
    /// Tracks whether the diff needs reload.
    pub diff_needs_reload: bool,
    /// Loading indicator for async detail fetch.
    pub detail_loading: bool,
    /// Loading indicator for on-demand rich LLM classification.
    pub llm_detail_loading: bool,
    /// Loading indicator for async diff fetch.
    pub diff_loading: bool,
    /// Cached render artifacts (overview/activity/diff lines).
    pub render_cache: RenderCache,
}

impl AppState {
    pub fn new(config: Config, client: DaemonClient) -> Self {
        let show_line_numbers = config.tui.show_line_numbers;
        Self {
            config,
            client,
            config_path: None,
            prs: PrListResponse {
                groups: HashMap::new(),
                updated_at: String::new(),
            },
            health: None,
            setup_status: None,
            setup_wizard: None,
            show_config_view: false,
            config_scroll: 0,
            show_search: false,
            search_filter: String::new(),
            search_filter_saved: String::new(),
            show_help: false,
            help_scroll: 0,
            selected_pr_id: None,
            pr_detail: None,
            pr_diff: None,
            show_line_numbers,
            error_message: None,
            daemon_child: None,
            loading: false,
            startup_phase: StartupPhase::Starting,
            refresh_inflight: false,
            ui_tick: 0,
            detail_needs_reload: false,
            diff_needs_reload: false,
            detail_loading: false,
            llm_detail_loading: false,
            diff_loading: false,
            render_cache: RenderCache::new(),
        }
    }

    /// Apply a fetched config, or toast the failure.
    fn accept_config(&mut self, result: Result<Config, String>) {
        match result {
            Ok(config) => self.config = config,
            Err(e) => self.error_message = Some(format!("Failed to fetch config: {}", e)),
        }
    }

    /// Apply fetched daemon health, or toast the failure.
    fn accept_health(&mut self, result: Result<HealthResponse, String>) {
        match result {
            Ok(health) => self.health = Some(health),
            Err(e) => self.error_message = Some(format!("Failed to fetch health: {}", e)),
        }
    }

    /// Apply a fetched setup status. Auto-opens the wizard when setup isn't
    /// ready (hydrating fresh only if one isn't already open, so this never
    /// clobbers in-progress wizard edits) but never auto-closes a wizard the
    /// user opened manually via `w`.
    fn accept_setup_status(&mut self, result: Result<SetupStatusResponse, String>) {
        match result {
            Ok(status) => {
                if !status.ready && self.setup_wizard.is_none() {
                    self.open_wizard();
                }
                self.setup_status = Some(status);
            }
            Err(e) => {
                self.error_message = Some(format!("Failed to fetch setup status: {}", e));
                if self.setup_wizard.is_none() {
                    self.open_wizard();
                }
            }
        }
    }

    /// Apply a fetched PR list (or its failure), clearing the in-flight
    /// guard either way.
    fn accept_prs(&mut self, result: Result<PrListResponse, String>) {
        self.refresh_inflight = false;
        match result {
            Ok(resp) => {
                self.prs = resp;
                self.loading = false;
                self.reconcile_selected_detail_freshness();
            }
            Err(e) => {
                self.error_message = Some(format!("Failed to fetch PRs: {}", e));
                self.loading = false;
            }
        }
    }

    /// Apply a config/health/setup(/prs) bundle fetched off the render
    /// loop — the shared acceptance path for opening the config view,
    /// reloading config, and applying a wizard commit.
    fn accept_config_bundle(&mut self, bundle: ConfigRefreshBundle) {
        self.accept_config(bundle.config);
        self.accept_health(bundle.health);
        self.accept_setup_status(bundle.setup);
        if let Some(prs) = bundle.prs {
            self.accept_prs(prs);
        }
    }

    /// Path the wizard should write to: the CLI-supplied `--config` path if
    /// any, else the default config location.
    fn wizard_config_path(&self) -> PathBuf {
        self.config_path
            .clone()
            .unwrap_or_else(|| crate::config::config_file_path().unwrap_or_default())
    }

    /// Open the setup wizard, hydrated from the currently loaded config —
    /// same step machine for first-run and for re-editing an existing config.
    pub fn open_wizard(&mut self) {
        let path = self.wizard_config_path();
        self.setup_wizard = Some(Box::new(SetupWizardState::hydrate(path, &self.config)));
        self.show_config_view = false;
    }

    /// Clear the loaded diff response.
    pub fn clear_diff_cache(&mut self) {
        self.pr_diff = None;
    }

    fn selected_summary(&self) -> Option<PrSummary> {
        let id = self.selected_pr_id.as_ref()?;
        self.prs
            .groups
            .values()
            .flat_map(|prs| prs.iter())
            .find(|pr| &pr.id == id)
            .cloned()
    }

    fn mark_selected_detail_stale(&mut self) {
        self.detail_needs_reload = true;
        self.diff_needs_reload = true;
        self.pr_detail = None;
        self.clear_diff_cache();
    }

    fn reconcile_selected_detail_freshness(&mut self) {
        let Some(id) = self.selected_pr_id.clone() else {
            return;
        };
        let Some(summary) = self.selected_summary() else {
            self.selected_pr_id = None;
            self.mark_selected_detail_stale();
            return;
        };

        let detail_is_stale = self.pr_detail.as_ref().is_none_or(|detail| {
            detail.id != id
                || detail.id != summary.id
                || detail.updated_at != summary.updated_at
                || detail.llm_priority != summary.llm_priority
        });
        if detail_is_stale {
            self.mark_selected_detail_stale();
        }
    }

    fn accept_detail(&mut self, detail: PrDetailResponse) {
        self.pr_detail = Some(detail);
    }

    fn accept_diff(&mut self, diff_resp: DiffResponse) {
        self.pr_diff = Some(diff_resp);
    }

    /// Move the Inbox cursor up/down. Navigation is in row space, so it walks
    /// section headers as well as PR rows.
    pub fn move_selection(&mut self, view: &mut ViewStateManager, delta: i32) {
        let len = view.view.inbox_rows.len();
        if len == 0 {
            return;
        }
        let new_row = (view.view.selected_row as i32 + delta).clamp(0, len as i32 - 1) as usize;
        self.set_selected_row(view, new_row);
    }

    /// Move the Inbox cursor to a specific row, updating the authoritative
    /// `inbox_cursor` anchor. `ViewStateManager::prepare` re-derives
    /// `selected_row` and `selected_pr_id` from that anchor, so a later refresh
    /// that reorders the list can't silently swap the selection out from under
    /// an unmoved index.
    pub fn set_selected_row(&mut self, view: &mut ViewStateManager, row: usize) {
        view.view.selected_row = row;
        match view.view.inbox_rows.get(row) {
            Some(InboxRow::Header { section, .. }) => {
                view.view.inbox_cursor = InboxCursor::Section(*section);
            }
            Some(InboxRow::Pr { id }) => {
                view.view.inbox_cursor = InboxCursor::Pr(id.clone());
            }
            None => {}
        }
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
        let boundaries = &self.render_cache.diff_file_boundaries;
        if let Some(&boundary) = boundaries.get(view.view.selected_file_index) {
            view.view.diff_scroll.scroll_to(boundary);
        }
    }

    pub fn sync_selected_file_to_diff_scroll(&mut self, view: &mut ViewStateManager) {
        let boundaries = &self.render_cache.diff_file_boundaries;
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
        if self.render_cache.diff_file_boundaries.is_empty() {
            return;
        }
        self.sync_selected_file_to_diff_scroll(view);
        let boundaries = &self.render_cache.diff_file_boundaries;
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
        if target == Blade::Overview {
            // If the user moves into the Overview blade and the current PR lacks a
            // rich catch-up summary, kick off a detail reload so the classify path fires.
            if let Some(detail) = self.pr_detail.as_ref() {
                if detail.llm_rich_summary.is_none() && !self.llm_detail_loading {
                    self.detail_needs_reload = true;
                }
            }
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

    /// Copy the selected PR's source branch to the system clipboard.
    fn copy_selected_branch(&mut self) {
        let Some(branch) = self
            .pr_detail
            .as_ref()
            .map(|detail| detail.head_ref.trim().to_string())
            .filter(|branch| !branch.is_empty())
        else {
            self.error_message = Some(
                "Copy branch failed: PR details are still loading or no branch is available"
                    .to_string(),
            );
            return;
        };

        match copy_to_clipboard(&branch) {
            Ok(()) => {
                self.error_message = Some(format!("Copied branch: {branch}"));
            }
            Err(error) => {
                self.error_message = Some(format!("Copy branch failed: {error}"));
            }
        }
    }

    /// Handle a key event.
    pub fn handle_key(&mut self, view: &mut ViewStateManager, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.handle_ctrl_key(view, key);
        }

        if self.setup_wizard.is_some() {
            return self.handle_setup_key(key);
        }

        if self.show_config_view {
            return self.handle_config_key(key);
        }

        // The help modal is a top-level overlay: while open it swallows every
        // key (closing on `?`/Esc/q), and `?` opens it from any blade. Guarded
        // against the search overlay so `?` can still be typed into a filter.
        if self.show_help {
            return self.handle_help_key(key);
        }
        if key.code == KeyCode::Char('?') && !self.show_search {
            self.show_help = true;
            self.help_scroll = 0;
            return Action::None;
        }

        // `/` toggles the inbox filter overlay. It takes precedence over normal
        // bindings and over search-mode character input.
        if key.code == KeyCode::Char('/') {
            if self.show_search && !self.search_filter.is_empty() {
                self.search_filter.clear();
                self.show_search = false;
            } else {
                // Snapshot the current filter so Esc can restore it on cancel.
                self.search_filter_saved = self.search_filter.clone();
                self.show_search = true;
            }
            return Action::None;
        }

        // Modal search input overrides normal bindings while the overlay is open.
        if self.show_search {
            return self.handle_search_key(key);
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => return Action::Quit,
            KeyCode::Char('c') | KeyCode::Char('C') => return Action::ToggleConfig,
            KeyCode::Char('w') | KeyCode::Char('W') => return Action::OpenWizard,
            KeyCode::Char('R') | KeyCode::Char('r') => {
                // A refresh already in flight (periodic or manual) makes a
                // second refresh key a no-op rather than piling up another
                // fetch against a possibly-slow daemon. `r` is an alias for `R`.
                if !self.refresh_inflight {
                    self.loading = true;
                    self.error_message = None;
                    return Action::Refresh;
                }
            }
            KeyCode::Char('n') if view.view.active_blade == Blade::Diff => {
                self.show_line_numbers = !self.show_line_numbers;
            }
            KeyCode::Char('o') | KeyCode::Char('O') => self.open_pr_in_browser(),
            KeyCode::Right | KeyCode::Char('l') => self.next_blade(view),
            KeyCode::Left | KeyCode::Char('h') => self.prev_blade(view),
            KeyCode::Esc => {
                // Universal step-out: from any deeper blade, back to the Inbox;
                // on the Inbox, clear an active filter, else dismiss any toast.
                if view.view.active_blade != Blade::Inbox {
                    self.set_active_blade(view, Blade::Inbox);
                } else if !self.search_filter.is_empty() {
                    self.search_filter.clear();
                } else {
                    self.error_message = None;
                }
            }
            KeyCode::Enter => match view.view.active_blade {
                // On a section header, Enter toggles its fold; on a PR row it
                // drills into the Overview blade.
                Blade::Inbox => {
                    if self.cursor_on_header(view) {
                        self.toggle_collapse_current(view);
                    } else {
                        self.set_active_blade(view, Blade::Overview);
                    }
                }
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
            KeyCode::Char('d') if view.view.active_blade == Blade::Overview => {
                view.view.overview_description_expanded = !view.view.overview_description_expanded;
            }
            KeyCode::Char('g') => self.jump_to_top(view),
            KeyCode::Char('G') => self.jump_to_bottom(view),
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

    /// Dispatch a key to the current wizard step's handler.
    fn handle_setup_key(&mut self, key: KeyEvent) -> Action {
        let Some(wizard) = self.setup_wizard.as_mut() else {
            return Action::None;
        };
        match wizard.step {
            WizardStep::Welcome => wizard::handle_welcome_key(wizard, key),
            WizardStep::AuthCheck => wizard::handle_auth_check_key(wizard, key),
            WizardStep::WatchMode => wizard::handle_watch_mode_key(wizard, key),
            WizardStep::WatchListInput => wizard::handle_watch_list_input_key(wizard, key),
            WizardStep::TargetPicker => wizard::handle_target_picker_key(wizard, key),
            WizardStep::TargetDetail => wizard::handle_target_detail_key(wizard, key),
            WizardStep::LivePreview => wizard::handle_live_preview_key(wizard, key),
            WizardStep::LlmConfig => wizard::handle_llm_config_key(wizard, key),
            WizardStep::Confirm => wizard::handle_confirm_key(wizard, key),
        }
    }

    fn handle_config_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => Action::Quit,
            KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Esc => Action::ToggleConfig,
            KeyCode::Char('w') | KeyCode::Char('W') => Action::OpenWizard,
            KeyCode::Char('R') => Action::ReloadConfig,
            KeyCode::Down | KeyCode::Char('j') => {
                self.config_scroll = self.config_scroll.saturating_add(1);
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.config_scroll = self.config_scroll.saturating_sub(1);
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Handle keys while the inbox search/filter overlay is active.
    ///
    /// Printable characters append to the filter and Backspace deletes the last
    /// one (a no-op, keeping the overlay open, when the filter is already
    /// empty). Enter accepts the filter; Esc cancels, restoring the filter to
    /// what it was when the overlay opened. Both close the overlay.
    fn handle_search_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char(c) => {
                self.search_filter.push(c);
            }
            KeyCode::Backspace => {
                // A no-op on an empty filter: the overlay stays open rather than
                // closing, so a following Enter can't fall through to a blade.
                self.search_filter.pop();
            }
            KeyCode::Enter => {
                self.show_search = false;
            }
            KeyCode::Esc => {
                self.search_filter = self.search_filter_saved.clone();
                self.show_search = false;
            }
            _ => {}
        }
        Action::None
    }

    fn handle_ctrl_key(&mut self, view: &mut ViewStateManager, key: KeyEvent) -> Action {
        // Ctrl+C must quit unconditionally — regardless of active blade, wizard,
        // or overlay state — since it's the terminal-standard interrupt and the
        // only other quit binding (`q`) is unavailable while text-input overlays
        // (search, wizard fields) have focus.
        if key.code == KeyCode::Char('c') {
            return Action::Quit;
        }

        match key.code {
            KeyCode::Char('d') => self.half_page(view, 1),
            KeyCode::Char('u') => self.half_page(view, -1),
            KeyCode::Char('y') => self.copy_selected_branch(),
            _ => {}
        }
        Action::None
    }

    /// Ctrl+D/Ctrl+U half-page movement for the active blade. Diff keeps its
    /// existing fixed-page scroll (with file-boundary sync); the other blades
    /// move by half their current viewport — the cursor in the row/file lists,
    /// the scroll offset in the content panes.
    fn half_page(&mut self, view: &mut ViewStateManager, dir: i32) {
        match view.view.active_blade {
            Blade::Diff => self.page_diff(view, dir),
            Blade::Inbox => {
                let half = (view.view.inbox_scroll.viewport_height / 2).max(1) as i32;
                self.move_selection(view, dir * half);
            }
            Blade::Files => {
                let half = (view.view.files_scroll.viewport_height / 2).max(1) as i32;
                self.move_file_selection(view, dir * half);
            }
            _ => {
                let half = (view.view.active_scroll().viewport_height / 2).max(1) as isize;
                view.view.active_scroll_mut().scroll_by(dir as isize * half);
            }
        }
    }

    /// `g`: jump to the top of the active blade — first row (Inbox), first file
    /// (Files), or the top of the content (Overview/Activity/Diff).
    fn jump_to_top(&mut self, view: &mut ViewStateManager) {
        match view.view.active_blade {
            Blade::Inbox => self.set_selected_row(view, 0),
            Blade::Files => self.set_selected_file_index(view, 0),
            Blade::Diff => view.view.diff_scroll.scroll_to(0),
            _ => view.view.active_scroll_mut().scroll_to(0),
        }
    }

    /// `G`: jump to the bottom of the active blade — last row (Inbox), last file
    /// (Files), or the end of the content (Overview/Activity/Diff). Diff also
    /// resyncs the selected file to the new scroll position, as before.
    fn jump_to_bottom(&mut self, view: &mut ViewStateManager) {
        match view.view.active_blade {
            Blade::Inbox => {
                let last = view.view.inbox_rows.len().saturating_sub(1);
                self.set_selected_row(view, last);
            }
            Blade::Files => {
                let last = self.selected_file_count().saturating_sub(1);
                self.set_selected_file_index(view, last);
            }
            Blade::Diff => {
                let max = view.view.diff_scroll.max_scroll();
                view.view.diff_scroll.scroll_to(max);
                self.sync_selected_file_to_diff_scroll(view);
            }
            _ => {
                let max = view.view.active_scroll().max_scroll();
                view.view.active_scroll_mut().scroll_to(max);
            }
        }
    }

    /// Handle keys while the help modal is open. It swallows every key: `?`,
    /// Esc, and `q` close it; j/k scroll it when the content overflows.
    fn handle_help_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.show_help = false;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.help_scroll = self.help_scroll.saturating_add(1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.help_scroll = self.help_scroll.saturating_sub(1);
            }
            _ => {}
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

    /// Whether the Inbox cursor currently rests on a section header row.
    fn cursor_on_header(&self, view: &ViewStateManager) -> bool {
        matches!(
            view.view.inbox_rows.get(view.view.selected_row),
            Some(InboxRow::Header { .. })
        )
    }

    /// Fold or unfold the section the cursor is in (the header itself, or the
    /// section enclosing the selected PR row). Folding moves the cursor onto the
    /// section's header so the user keeps their place and can unfold from there.
    pub fn toggle_collapse_current(&mut self, view: &mut ViewStateManager) {
        let section = match view.view.inbox_rows.get(view.view.selected_row) {
            Some(InboxRow::Header { section, .. }) => *section,
            Some(InboxRow::Pr { .. }) => {
                match self.enclosing_section(view, view.view.selected_row) {
                    Some(s) => s,
                    None => return,
                }
            }
            None => return,
        };
        let now_collapsed = !view
            .view
            .collapsed_sections
            .get(&section)
            .copied()
            .unwrap_or_else(|| section.default_collapsed());
        view.view.collapsed_sections.insert(section, now_collapsed);
        // Anchor the cursor to the header so folding never strands it on a row
        // that just disappeared; `prepare` re-resolves it to the header's index.
        view.view.inbox_cursor = InboxCursor::Section(section);
    }

    /// The section enclosing the row at `row`: the nearest preceding header.
    fn enclosing_section(&self, view: &ViewStateManager, row: usize) -> Option<InboxSection> {
        view.view.inbox_rows[..row]
            .iter()
            .rev()
            .find_map(|r| match r {
                InboxRow::Header { section, .. } => Some(*section),
                InboxRow::Pr { .. } => None,
            })
    }
}

/// Run the full TUI application.
pub async fn run_tui(config: Config) -> Result<()> {
    run_tui_with_config_path(config, None).await
}

/// RAII guard for terminal raw mode + alternate screen.
///
/// Entering the terminal is multiple fallible steps (raw mode, alternate
/// screen, cursor hide); without a guard, a `?` failing partway through
/// leaves the terminal in a broken state, and the same is true of any `?`
/// in the render loop itself. The panic hook installed in
/// `run_tui_with_config_path` covers panics/aborts, where `Drop` doesn't
/// run — the two mechanisms are complementary, not redundant.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut out = std::io::stdout();
        out.execute(EnterAlternateScreen).inspect_err(|_| {
            let _ = disable_raw_mode();
        })?;
        out.execute(Hide).inspect_err(|_| {
            let _ = std::io::stdout().execute(LeaveAlternateScreen);
            let _ = disable_raw_mode();
        })?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        let _ = out.execute(Show);
        let _ = out.execute(LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// Run TUI with an optional config path (for daemon spawn forwarding).
pub async fn run_tui_with_config_path(config: Config, config_path: Option<PathBuf>) -> Result<()> {
    let port = config.daemon.port;

    let client = DaemonClient::new(port)?;

    let mut state = AppState::new(config, client);
    state.config_path = config_path.clone();

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = std::io::stdout().execute(Show);
        let _ = std::io::stdout().execute(LeaveAlternateScreen);
        let _ = disable_raw_mode();
        original_hook(info);
    }));

    let guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let result = run_render_loop(&mut terminal, &mut state).await;
    drop(guard);

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
            if daemon_binary_is_current(&health) {
                info!("Daemon already running");
                return Ok(None);
            }
            warn!(
                "Running daemon is a stale build (binary changed on disk since it started); \
                 restarting it"
            );
            if let Err(e) = client.request_shutdown().await {
                warn!("Failed to request graceful daemon shutdown: {}", e);
            }
            wait_for_daemon_to_exit(client).await;
        }
    }

    spawn_daemon(client, config_path).await
}

/// Whether a running daemon reports the same binary mtime we're running
/// from. If either side couldn't determine its mtime, assume it matches
/// rather than forcing an unnecessary restart.
fn daemon_binary_is_current(health: &HealthResponse) -> bool {
    match (health.binary_mtime, crate::daemon::binary_mtime()) {
        (Some(daemon_mtime), Some(our_mtime)) => daemon_mtime == our_mtime,
        _ => true,
    }
}

/// Poll until a stale daemon's HTTP server stops responding (or we give up
/// and let the caller spawn a replacement anyway — a leftover process is a
/// pre-existing problem for the user to notice via a bind failure, not a new
/// one this restart logic introduces).
async fn wait_for_daemon_to_exit(client: &DaemonClient) {
    let max_wait = std::time::Duration::from_secs(10);
    let start = std::time::Instant::now();
    while start.elapsed() < max_wait {
        if client.check_health().await.is_none() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    warn!("Timed out waiting for stale daemon to exit; spawning a new one anyway");
}

async fn spawn_daemon(
    client: &DaemonClient,
    config_path: Option<&std::path::Path>,
) -> Result<Option<Child>> {
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

    if state.setup_wizard.is_some() {
        crate::tui::views::setup::render_setup_wizard(f, area, state);
        return;
    }

    // RootLayout::render fills the whole terminal with BASE, so every cell is
    // painted explicitly — no manual clear_area / skip-flag reset is needed.
    let layout = RootLayout::new(view.view.active_blade).render(f, area);

    // Reconcile view state with domain/cached data and clamp scroll offsets.
    view.prepare(state, &layout);

    let theme = Theme;
    let ctx = RenderContext::new(state, &view.view, &theme);

    // Chrome.
    crate::tui::views::chrome::render_tab_line(f, layout.tab_line, &ctx);
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

    if state.show_config_view {
        crate::tui::views::config::render_config_view(f, layout.body, &ctx);
    }

    if state.show_search {
        crate::tui::views::search::render_search_overlay(f, layout.body, &ctx);
    }

    if state.show_help {
        crate::tui::views::help::render_help(f, layout.body, &ctx);
    }

    if state.startup_phase != StartupPhase::Ready {
        crate::tui::views::loading::render_loading(f, layout.body, &ctx);
    }

    // Error/diagnostic feedback renders as a centered overlay over the body.
    // The keybar remains visible with its bindings at all times.
    InlineToast.render(f, layout.body, &ctx);
}

/// Whether `run_render_loop` should keep running after processing an event.
enum LoopControl {
    Continue,
    Quit,
}

/// Split a batch of events drained from the channel into "was at least one
/// DataTick queued", "was at least one UiTick queued", and the remaining
/// events in their original order. A stalled loop can queue several ticks
/// before it gets a chance to drain them; replaying each one is pointless
/// (DataTick only needs to trigger one refresh, UiTick only needs `ui_tick`
/// to have advanced), so ticks collapse while everything else passes
/// through untouched.
fn coalesce_ticks(events: Vec<TuiEvent>) -> (bool, bool, Vec<TuiEvent>) {
    let mut data_tick = false;
    let mut ui_tick = false;
    let mut rest = Vec::with_capacity(events.len());
    for e in events {
        match e {
            TuiEvent::DataTick => data_tick = true,
            TuiEvent::UiTick => ui_tick = true,
            other => rest.push(other),
        }
    }
    (data_tick, ui_tick, rest)
}

/// Spawn a background PR list refresh. `trigger` requests
/// `POST /prs/refresh` (and the settle delay the daemon needs to act on it
/// before the poller reflects it) ahead of the fetch; a periodic `DataTick`
/// refresh skips that and just re-fetches.
fn spawn_prs_refresh(
    event_tx: &mpsc::UnboundedSender<TuiEvent>,
    client: &DaemonClient,
    trigger: bool,
) {
    let tx = event_tx.clone();
    let client = client.clone();
    tokio::spawn(async move {
        let trigger_error = if trigger {
            let res = client.refresh().await;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            res.err().map(|e| format!("Refresh failed: {}", e))
        } else {
            None
        };
        let prs = client.get_prs().await.map_err(|e| e.to_string());
        let health = client.get_health().await.ok();
        let _ = tx.send(TuiEvent::PrsRefreshed {
            trigger_error,
            prs: Box::new(prs),
            health,
        });
    });
}

/// Fetch the config/health/setup(/prs) bundle shared by the config-view,
/// config-reload, and wizard-commit flows.
async fn fetch_config_bundle(client: &DaemonClient, include_prs: bool) -> ConfigRefreshBundle {
    let config = client.get_config().await.map_err(|e| e.to_string());
    let health = client.get_health().await.map_err(|e| e.to_string());
    let setup = client.get_setup_status().await.map_err(|e| e.to_string());
    let prs = if include_prs {
        Some(client.get_prs().await.map_err(|e| e.to_string()))
    } else {
        None
    };
    ConfigRefreshBundle {
        config,
        health,
        setup,
        prs,
    }
}

fn spawn_config_view_fetch(event_tx: &mpsc::UnboundedSender<TuiEvent>, client: &DaemonClient) {
    let tx = event_tx.clone();
    let client = client.clone();
    tokio::spawn(async move {
        let bundle = fetch_config_bundle(&client, false).await;
        let _ = tx.send(TuiEvent::ConfigViewOpened(bundle));
    });
}

fn spawn_config_reload(event_tx: &mpsc::UnboundedSender<TuiEvent>, client: &DaemonClient) {
    let tx = event_tx.clone();
    let client = client.clone();
    tokio::spawn(async move {
        let result = match client.reload_config().await {
            Ok(()) => Ok(fetch_config_bundle(&client, true).await),
            Err(e) => Err(e.to_string()),
        };
        let _ = tx.send(TuiEvent::ConfigReloaded(result));
    });
}

fn spawn_wizard_config_applied(event_tx: &mpsc::UnboundedSender<TuiEvent>, client: &DaemonClient) {
    let tx = event_tx.clone();
    let client = client.clone();
    tokio::spawn(async move {
        let bundle = fetch_config_bundle(&client, true).await;
        let _ = tx.send(TuiEvent::WizardConfigApplied(bundle));
    });
}

/// Handle one non-tick event. `DataTick`/`UiTick` are coalesced by the
/// caller in `run_render_loop` before reaching here, since collapsing
/// repeated ticks (rather than replaying every queued one) is a property of
/// the batch, not of a single event.
fn handle_event(
    state: &mut AppState,
    view: &mut ViewStateManager,
    event_tx: &mpsc::UnboundedSender<TuiEvent>,
    event: TuiEvent,
) -> LoopControl {
    match event {
        TuiEvent::Key(key) => {
            let action = state.handle_key(view, key);
            match action {
                Action::Quit => return LoopControl::Quit,
                Action::Refresh => {
                    state.refresh_inflight = true;
                    spawn_prs_refresh(event_tx, &state.client, true);
                }
                Action::ReloadConfig => spawn_config_reload(event_tx, &state.client),
                Action::OpenWizard => state.open_wizard(),
                Action::CloseWizard => state.setup_wizard = None,
                Action::ToggleConfig => {
                    if state.show_config_view {
                        state.show_config_view = false;
                    } else {
                        // Show the overlay immediately; it renders from
                        // `state.config`/`state.health`/`state.setup_status`,
                        // which are always present (if stale), and re-renders
                        // once the fetch below lands.
                        state.show_config_view = true;
                        state.config_scroll = 0;
                        spawn_config_view_fetch(event_tx, &state.client);
                    }
                }
                Action::None => {}
            }
        }
        TuiEvent::Resize => {}
        TuiEvent::DataTick | TuiEvent::UiTick => {
            unreachable!("ticks are coalesced by run_render_loop before dispatch")
        }
        TuiEvent::PrsRefreshed {
            trigger_error,
            prs,
            health,
        } => {
            if let Some(e) = trigger_error {
                state.error_message = Some(e);
            }
            state.accept_prs(*prs);
            if let Some(h) = health {
                state.health = Some(h);
            }
        }
        TuiEvent::ConfigViewOpened(bundle) => state.accept_config_bundle(bundle),
        TuiEvent::ConfigReloaded(result) => match result {
            Ok(bundle) => state.accept_config_bundle(bundle),
            Err(e) => state.error_message = Some(format!("Failed to reload config: {}", e)),
        },
        TuiEvent::WizardConfigApplied(bundle) => state.accept_config_bundle(bundle),
        TuiEvent::StartupLoaded(result) => match *result {
            Ok(load) => {
                state.daemon_child = load.daemon_child;
                if !load.setup_status.ready {
                    let path = state.wizard_config_path();
                    state.setup_wizard =
                        Some(Box::new(SetupWizardState::hydrate(path, &load.config)));
                }
                state.setup_status = Some(load.setup_status);
                state.config = load.config;
                state.prs = load.prs;
                state.health = load.health;
                state.loading = false;
                state.reconcile_selected_detail_freshness();
                state.startup_phase = StartupPhase::Ready;
            }
            Err(e) => {
                state.error_message = Some(e);
                state.startup_phase = StartupPhase::Ready;
            }
        },
        TuiEvent::DetailLoaded(id, result) => {
            state.detail_loading = false;
            if state.selected_pr_id.as_ref() == Some(&id) {
                match *result {
                    Ok(detail) => {
                        let in_overview =
                            view.view.active_blade == crate::tui::render::layout::Blade::Overview;
                        let needs_rich = in_overview && detail.llm_rich_summary.is_none();
                        let has_rich = detail.llm_rich_summary.is_some();
                        state.accept_detail(detail);

                        if needs_rich && state.config.llm.enabled && !state.llm_detail_loading {
                            state.llm_detail_loading = true;
                            let tx = event_tx.clone();
                            let client = state.client.clone();
                            tokio::spawn(async move {
                                let result = match client.classify(&id).await {
                                    Ok(_) => {
                                        let _ = client.mark_seen(&id).await;
                                        match client.get_pr_detail(&id).await {
                                            Ok(detail) => {
                                                let _ = tx.send(TuiEvent::DetailLoaded(
                                                    id.clone(),
                                                    Box::new(Ok(detail)),
                                                ));
                                                Ok(id.clone())
                                            }
                                            Err(e) => Err(e.to_string()),
                                        }
                                    }
                                    Err(e) => Err(e.to_string()),
                                };
                                match result {
                                    Ok(success_id) => {
                                        let _ =
                                            tx.send(TuiEvent::LlmClassified(success_id, Ok(())));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(TuiEvent::LlmClassified(id, Err(e)));
                                    }
                                }
                            });
                        } else if in_overview && has_rich {
                            // Record that the user has seen this PR while it has a fresh
                            // rich summary, so the next catch-up is scoped to this viewing.
                            let client = state.client.clone();
                            tokio::spawn(async move {
                                let _ = client.mark_seen(&id).await;
                            });
                        }
                    }
                    Err(e) => state.error_message = Some(format!("Failed to load detail: {}", e)),
                }
            }
        }
        TuiEvent::LlmClassified(id, result) => {
            state.llm_detail_loading = false;
            if state.selected_pr_id.as_ref() == Some(&id) {
                if let Err(e) = result {
                    state.error_message = Some(format!("LLM summary failed: {}", e));
                }
            }
        }
        TuiEvent::DiffLoaded(id, result) => {
            state.diff_loading = false;
            if state.selected_pr_id.as_ref() == Some(&id) {
                match result {
                    Ok(diff_resp) => {
                        state.accept_diff(diff_resp);
                        state.scroll_diff_to_selected_file(view);
                    }
                    Err(e) => state.error_message = Some(format!("Failed to load diff: {}", e)),
                }
            }
        }
        TuiEvent::WizardAuthStatusLoaded(result) => {
            if let Some(wizard) = state.setup_wizard.as_mut() {
                match result {
                    Ok(status) => wizard.auth = wizard::AsyncResource::Ready(status),
                    Err(e) => {
                        // Auth-poll errors go to the global toast, not a
                        // wizard-rendered field (matches pre-existing UX);
                        // the resource still records Failed so a later 'r'
                        // recheck is a normal Failed -> Requested transition.
                        wizard.auth = wizard::AsyncResource::Failed(e.clone());
                        state.error_message = Some(format!("Failed to check auth: {}", e));
                    }
                }
            }
        }
        TuiEvent::WizardMembershipsLoaded(result) => {
            if let Some(wizard) = state.setup_wizard.as_mut() {
                wizard.memberships = match result {
                    Ok(resp) => wizard::AsyncResource::Ready(resp),
                    Err(e) => wizard::AsyncResource::Failed(e),
                };
            }
        }
        TuiEvent::WizardPreviewLoaded(result) => {
            if let Some(wizard) = state.setup_wizard.as_mut() {
                wizard.preview = match result {
                    Ok(resp) => wizard::AsyncResource::Ready(resp),
                    Err(e) => wizard::AsyncResource::Failed(e),
                };
            }
        }
        TuiEvent::WizardConfigWritten(result) => match result {
            Ok(()) => {
                // Written and reloaded successfully; close the wizard and
                // spawn the config/health/setup/prs refresh that reflects it.
                // (`AsyncResource::Ready(())` is never observed here — the
                // wizard is gone before anything would render it.)
                state.setup_wizard = None;
                spawn_wizard_config_applied(event_tx, &state.client);
            }
            Err(e) => {
                if let Some(wizard) = state.setup_wizard.as_mut() {
                    wizard.commit = wizard::AsyncResource::Failed(e);
                }
            }
        },
    }
    LoopControl::Continue
}

async fn run_render_loop(
    terminal: &mut ratatui::DefaultTerminal,
    state: &mut AppState,
) -> Result<()> {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_handle = spawn_event_loop(event_tx.clone());
    let mut view = ViewStateManager::new();
    let mut startup_spawned = false;

    loop {
        if !startup_spawned {
            let tx = event_tx.clone();
            let client = state.client.clone();
            let port = state.config.daemon.port;
            let config_path = state.config_path.clone();
            tokio::spawn(async move {
                let result = async {
                    let daemon_child = ensure_daemon(&client, port, config_path.as_deref())
                        .await
                        .map_err(|e| format!("Failed to start daemon: {}", e))?;
                    let setup_status = client
                        .get_setup_status()
                        .await
                        .map_err(|e| format!("Failed to fetch setup status: {}", e))?;
                    let config = client
                        .get_config()
                        .await
                        .map_err(|e| format!("Failed to fetch config: {}", e))?;
                    let prs = if setup_status.ready {
                        client
                            .get_prs()
                            .await
                            .map_err(|e| format!("Failed to fetch PRs: {}", e))?
                    } else {
                        PrListResponse {
                            groups: HashMap::new(),
                            updated_at: String::new(),
                        }
                    };
                    let health = client.get_health().await.ok();
                    Ok(StartupLoad {
                        daemon_child,
                        setup_status,
                        config,
                        prs,
                        health,
                    })
                }
                .await;
                let _ = tx.send(TuiEvent::StartupLoaded(Box::new(result)));
            });
            startup_spawned = true;
        }

        // Start async detail/diff fetches instead of blocking the render loop.
        if state.detail_needs_reload {
            if let Some(id) = state.selected_pr_id.clone() {
                let tx = event_tx.clone();
                let client = state.client.clone();
                tokio::spawn(async move {
                    let res = client.get_pr_detail(&id).await.map_err(|e| e.to_string());
                    let _ = tx.send(TuiEvent::DetailLoaded(id, Box::new(res)));
                });
                state.detail_loading = true;
            }
            state.detail_needs_reload = false;
        }
        if state.diff_needs_reload {
            if let Some(id) = state.selected_pr_id.clone() {
                let tx = event_tx.clone();
                let client = state.client.clone();
                tokio::spawn(async move {
                    let res = client.get_pr_diff(&id).await.map_err(|e| e.to_string());
                    let _ = tx.send(TuiEvent::DiffLoaded(id, res));
                });
                state.diff_loading = true;
            }
            state.diff_needs_reload = false;
        }

        // Wizard async fetches follow the same non-blocking pattern as
        // detail/diff above, modeled as an `AsyncResource` state machine
        // (see wizard.rs): a key handler moves a resource Idle/Ready/Failed
        // -> Requested; here we move Requested -> Loading and spawn the
        // fetch; the result comes back as a TuiEvent that moves Loading ->
        // Ready/Failed. Requested -> Loading only (never Loading itself)
        // means a resource can't be spawned twice concurrently.
        if let Some(wizard) = state.setup_wizard.as_mut() {
            if matches!(wizard.auth, wizard::AsyncResource::Requested) {
                wizard.auth = wizard::AsyncResource::Loading;
                let tx = event_tx.clone();
                let client = state.client.clone();
                tokio::spawn(async move {
                    let res = client.get_setup_status().await.map_err(|e| e.to_string());
                    let _ = tx.send(TuiEvent::WizardAuthStatusLoaded(res));
                });
            }
            if matches!(wizard.memberships, wizard::AsyncResource::Requested) {
                wizard.memberships = wizard::AsyncResource::Loading;
                let tx = event_tx.clone();
                let client = state.client.clone();
                tokio::spawn(async move {
                    let res = client
                        .get_org_memberships()
                        .await
                        .map_err(|e| e.to_string());
                    let _ = tx.send(TuiEvent::WizardMembershipsLoaded(res));
                });
            }
            if matches!(wizard.preview, wizard::AsyncResource::Requested) {
                wizard.preview = wizard::AsyncResource::Loading;
                let draft = wizard.draft();
                let tx = event_tx.clone();
                let client = state.client.clone();
                tokio::spawn(async move {
                    let res = client
                        .preview_config_counts(&draft)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = tx.send(TuiEvent::WizardPreviewLoaded(res));
                });
            }
            if matches!(wizard.commit, wizard::AsyncResource::Requested) {
                wizard.commit = wizard::AsyncResource::Loading;
                let draft = wizard.draft();
                let path = wizard.config_path.clone();
                let tx = event_tx.clone();
                let client = state.client.clone();
                tokio::spawn(async move {
                    let res: Result<(), String> = async {
                        draft.validate().map_err(|e| e.to_string())?;
                        draft.write_atomic(&path).map_err(|e| e.to_string())?;
                        client.reload_config().await.map_err(|e| e.to_string())?;
                        // `/config/reload` only swaps the config for the poller's
                        // *next* scheduled cycle (up to poll_interval away), which
                        // can otherwise leave stale out-of-scope PRs visible for
                        // minutes. Force an immediate re-poll so the new scope
                        // takes effect right away. Best-effort: the config write
                        // and reload already succeeded, so a refresh-trigger
                        // failure here isn't fatal to the commit.
                        let _ = client.refresh().await;
                        Ok(())
                    }
                    .await;
                    let _ = tx.send(TuiEvent::WizardConfigWritten(res));
                });
            }
        }

        terminal.draw(|f| render_frame(f, state, &mut view))?;

        let event = match event_rx.recv().await {
            Some(e) => e,
            None => break,
        };

        // Drain whatever else piled up on the channel while we were
        // rendering/awaiting and coalesce ticks: a stall (e.g. a slow
        // daemon response) can queue several DataTicks/UiTicks, and
        // replaying each one back-to-back buys nothing — DataTick only
        // needs to trigger one refresh, and UiTick only needs `ui_tick` to
        // have advanced. Every non-tick event still runs in order.
        let mut pending = vec![event];
        while let Ok(e) = event_rx.try_recv() {
            pending.push(e);
        }
        let (data_tick, ui_tick, rest) = coalesce_ticks(pending);
        let mut quit = false;
        for other in rest {
            if let LoopControl::Quit = handle_event(state, &mut view, &event_tx, other) {
                quit = true;
                break;
            }
        }
        if quit {
            break;
        }

        if ui_tick {
            state.ui_tick = state.ui_tick.wrapping_add(1);
        }
        if data_tick
            && state.setup_wizard.is_none()
            && state.startup_phase == StartupPhase::Ready
            && !state.refresh_inflight
        {
            state.refresh_inflight = true;
            spawn_prs_refresh(&event_tx, &state.client, false);
        }
    }

    event_handle.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::types::{CheckStatus, MergeableState, PrGroup, Priority};

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

    #[test]
    fn daemon_binary_is_current_matches_reported_mtime() {
        let our_mtime = crate::daemon::binary_mtime().expect("test binary should have an mtime");

        let make_health = |binary_mtime: Option<u64>| HealthResponse {
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
            binary_mtime,
        };

        assert!(daemon_binary_is_current(&make_health(Some(our_mtime))));
        assert!(!daemon_binary_is_current(&make_health(Some(
            our_mtime.wrapping_add(1)
        ))));
        // Fail open when either side couldn't determine an mtime, rather
        // than forcing an unnecessary restart.
        assert!(daemon_binary_is_current(&make_health(None)));
    }

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

    fn make_detail(id: &str, updated_at: &str, priority: Option<Priority>) -> PrDetailResponse {
        let parts: Vec<&str> = id.split('~').collect();
        PrDetailResponse {
            id: id.to_string(),
            node_id: "node".to_string(),
            owner: parts.first().unwrap_or(&"org").to_string(),
            repo: parts.get(1).unwrap_or(&"repo").to_string(),
            number: parts.get(2).and_then(|n| n.parse().ok()).unwrap_or(1),
            title: "Test PR".to_string(),
            body: String::new(),
            url: "https://example.com".to_string(),
            author: "other".to_string(),
            is_draft: false,
            updated_at: updated_at.to_string(),
            head_ref: "feature".to_string(),
            base_ref: "main".to_string(),
            mergeable: MergeableState::Mergeable,
            review_decision: None,
            review_requests: vec![],
            team_review_requests: vec![],
            viewer_latest_review: None,
            latest_reviews: vec![],
            check_status: CheckStatus::None,
            checks: vec![],
            review_threads: vec![],
            files: vec![],
            timeline: vec![],
            llm_priority: priority,
            llm_summary: None,
            llm_rich_summary: None,
            last_seen_at: None,
        }
    }

    #[test]
    fn selected_detail_is_marked_stale_when_summary_updated_at_changes() {
        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![make_summary(
                "org~repo~1",
                PrGroup::ReviewNeeded,
                "other",
                None,
            )],
        );
        let mut state = make_test_state(groups);
        state.selected_pr_id = Some("org~repo~1".to_string());
        state.pr_detail = Some(make_detail("org~repo~1", "2023-01-01T00:00:00Z", None));
        state.pr_diff = Some(DiffResponse {
            diff: "diff --git a/a b/a\n".to_string(),
            cached: true,
        });
        state.reconcile_selected_detail_freshness();

        assert!(state.detail_needs_reload);
        assert!(state.diff_needs_reload);
        assert!(state.pr_detail.is_none());
        assert!(state.pr_diff.is_none());
    }

    #[test]
    fn preparing_after_accepting_a_new_detail_and_diff_rebuilds_render_cache() {
        // `accept_detail`/`accept_diff` no longer clear the render cache
        // themselves (see `RenderCache`'s identity-keyed rebuilds); the
        // rebuild happens the next time `ViewStateManager::prepare` runs and
        // notices the detail/diff identity changed.
        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![make_summary(
                "org~repo~1",
                PrGroup::ReviewNeeded,
                "other",
                None,
            )],
        );
        let mut state = make_test_state(groups);
        let mut view = ViewStateManager::new();
        let layout =
            RootLayout::new(Blade::Overview).compute(ratatui::layout::Rect::new(0, 0, 80, 24));

        state.selected_pr_id = Some("org~repo~1".to_string());
        view.prepare(&mut state, &layout);
        state.detail_needs_reload = false;
        state.diff_needs_reload = false;
        assert!(
            state.render_cache.overview_summary.is_empty(),
            "no detail loaded yet, so overview_summary should be empty"
        );
        assert!(state.render_cache.diff_lines.is_empty());

        let mut detail = make_detail("org~repo~1", "2024-01-01T00:00:00Z", None);
        detail.llm_summary = Some("New summary".to_string());
        state.accept_detail(detail);
        view.prepare(&mut state, &layout);
        assert!(
            !state.render_cache.overview_summary.is_empty(),
            "a newly accepted detail with a summary must rebuild the overview cache"
        );

        state.accept_diff(DiffResponse {
            diff: "diff --git a/a.txt b/a.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n".to_string(),
            cached: false,
        });
        view.prepare(&mut state, &layout);
        assert!(
            !state.render_cache.diff_lines.is_empty(),
            "a newly accepted diff must rebuild the diff cache"
        );
        assert!(state.pr_diff.is_some());
    }

    #[test]
    fn test_move_selection_updates_detail_flag() {
        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![
                make_summary("a~b~1", PrGroup::ReviewNeeded, "other", None),
                make_summary("a~b~2", PrGroup::ReviewNeeded, "other", None),
            ],
        );

        let mut state = make_test_state(groups);
        let mut view = ViewStateManager::new();
        let layout =
            RootLayout::new(Blade::Inbox).compute(ratatui::layout::Rect::new(0, 0, 80, 24));
        view.prepare(&mut state, &layout);

        state.move_selection(&mut view, 1);
        view.prepare(&mut state, &layout);
        // Rows: [header, a~b~1, a~b~2]; the cursor starts on a~b~1 and moves to a~b~2.
        assert_eq!(view.view.selected_row, 2);
        assert_eq!(state.selected_pr_id, Some("a~b~2".to_string()));
        assert!(state.detail_needs_reload);
        assert!(state.diff_needs_reload);
    }

    #[test]
    fn test_blade_navigation_clamps() {
        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![make_summary("a~b~1", PrGroup::ReviewNeeded, "other", None)],
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
            PrGroup::ReviewNeeded,
            vec![make_summary("a~b~1", PrGroup::ReviewNeeded, "other", None)],
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
            PrGroup::ReviewNeeded,
            vec![make_summary("a~b~1", PrGroup::ReviewNeeded, "other", None)],
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
            PrGroup::ReviewNeeded,
            vec![
                make_summary(
                    "org~repo~1",
                    PrGroup::ReviewNeeded,
                    "other",
                    Some(Priority::High),
                ),
                make_summary(
                    "org~repo~2",
                    PrGroup::ReviewNeeded,
                    "other2",
                    Some(Priority::Medium),
                ),
            ],
        );
        groups.insert(
            PrGroup::AuthoredWaiting,
            vec![make_summary(
                "org~repo~3",
                PrGroup::AuthoredWaiting,
                "me",
                None,
            )],
        );

        let mut state = make_test_state(groups);
        state.selected_pr_id = Some("org~repo~1".to_string());

        let backend = TestBackend::new(124, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut view = ViewStateManager::new();
        view.view.active_blade = Blade::Inbox;

        // Settle the initial selection first: the first `prepare()` call
        // after a fresh `ViewStateManager` always resets cached detail,
        // matching the real event flow where the first selection triggers
        // an async detail fetch. Setting `pr_detail` only after this keeps
        // it from being cleared by that settle.
        terminal
            .draw(|f| crate::tui::app::render_frame(f, &mut state, &mut view))
            .unwrap();
        state.detail_needs_reload = false;
        state.diff_needs_reload = false;

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
            mergeable: MergeableState::Mergeable,
            review_decision: None,
            review_requests: vec![],
            team_review_requests: vec![],
            viewer_latest_review: None,
            latest_reviews: vec![],
            check_status: CheckStatus::Pending,
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
            llm_rich_summary: None,
            last_seen_at: None,
        });

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
        assert!(content.contains("Brunson Says"));
    }

    #[test]
    fn overview_111x30_no_inbox_leak_after_blade_switch() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![
                make_summary(
                    "org~repo~1",
                    PrGroup::ReviewNeeded,
                    "other",
                    Some(Priority::High),
                ),
                make_summary(
                    "org~repo~2",
                    PrGroup::ReviewNeeded,
                    "other2",
                    Some(Priority::Medium),
                ),
            ],
        );
        groups.insert(
            PrGroup::AuthoredWaiting,
            vec![make_summary(
                "org~repo~3",
                PrGroup::AuthoredWaiting,
                "me",
                None,
            )],
        );
        groups.insert(
            PrGroup::AuthoredReadyToMerge,
            vec![make_summary(
                "org~repo~4",
                PrGroup::AuthoredReadyToMerge,
                "me",
                None,
            )],
        );
        groups.insert(
            PrGroup::AuthoredActionNeeded,
            vec![make_summary(
                "org~repo~5",
                PrGroup::AuthoredActionNeeded,
                "me",
                None,
            )],
        );

        let mut state = make_test_state(groups);
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
            line.contains("diff") && line.contains("blade") && line.contains("quit"),
            "keybar should show the Files bindings, got: {:?}",
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
            PrGroup::ReviewNeeded,
            vec![
                make_summary(
                    "org~repo~1",
                    PrGroup::ReviewNeeded,
                    "other",
                    Some(Priority::High),
                ),
                make_summary(
                    "org~repo~2",
                    PrGroup::ReviewNeeded,
                    "other2",
                    Some(Priority::Medium),
                ),
            ],
        );
        groups.insert(
            PrGroup::AuthoredWaiting,
            vec![make_summary(
                "org~repo~3",
                PrGroup::AuthoredWaiting,
                "me",
                None,
            )],
        );

        let mut state = make_test_state(groups);
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
            mergeable: MergeableState::Mergeable,
            review_decision: None,
            review_requests: vec![],
            team_review_requests: vec![],
            viewer_latest_review: None,
            latest_reviews: vec![],
            check_status: CheckStatus::Success,
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
            llm_rich_summary: None,
            last_seen_at: None,
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
            PrGroup::ReviewNeeded,
            vec![
                make_summary(
                    "org~repo~1",
                    PrGroup::ReviewNeeded,
                    "other",
                    Some(Priority::High),
                ),
                make_summary(
                    "org~repo~2",
                    PrGroup::ReviewNeeded,
                    "other2",
                    Some(Priority::Medium),
                ),
                make_summary("org~repo~3", PrGroup::ReviewNeeded, "other3", None),
            ],
        );

        // The selected row must carry the selection background across the full
        // blade width. PR/file titles are deliberately not hyperlinked because
        // those overlays corrupted cell widths and broke the highlight.
        let mut state = make_test_state(groups.clone());
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
        assert_eq!(bad, 0, "selected row not fully highlighted:\n{report}");
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
            PrGroup::ReviewNeeded,
            vec![make_summary(
                "org~repo~42",
                PrGroup::ReviewNeeded,
                "other",
                None,
            )],
        );
        let mut state = make_test_state(groups);
        state.selected_pr_id = Some("org~repo~42".to_string());

        let mut view = ViewStateManager::new();
        view.view.active_blade = Blade::Files;
        let layout =
            RootLayout::new(Blade::Files).compute(ratatui::layout::Rect::new(0, 0, 111, 30));

        // Settle the initial selection first: the first `prepare()` call
        // after a fresh `ViewStateManager` always resets cached detail and
        // `selected_file_index`, matching the real event flow where the
        // first selection triggers an async detail fetch. Setting
        // `pr_detail`/`selected_file_index` only after this keeps them from
        // being cleared by that settle.
        view.prepare(&mut state, &layout);
        state.detail_needs_reload = false;
        state.diff_needs_reload = false;

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
            mergeable: MergeableState::Mergeable,
            review_decision: None,
            review_requests: vec![],
            team_review_requests: vec![],
            viewer_latest_review: None,
            latest_reviews: vec![],
            check_status: CheckStatus::None,
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
            llm_rich_summary: None,
            last_seen_at: None,
        });

        view.view.selected_file_index = 29;
        view.prepare(&mut state, &layout);

        // Active blade content height is 25 (full-bleed body under a 5-row
        // chrome); the Files blade reserves one row for its header, so the
        // scrollable viewport is 24 rows. Selecting the last file should scroll
        // so that it is the last visible row (offset 6).
        assert_eq!(
            view.view.files_scroll.offset, 6,
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
            PrGroup::ReviewNeeded,
            (0..30)
                .map(|i| crate::api::PrSummary {
                    id: format!("org~repo~{}", i),
                    node_id: "n1".to_string(),
                    owner: "org".to_string(),
                    repo: "repo".to_string(),
                    number: i as u64,
                    title: format!("PR {}", i),
                    author: "other".to_string(),
                    author_is_bot: false,
                    group: PrGroup::ReviewNeeded,
                    next_action: "Review now".to_string(),
                    check_status: CheckStatus::None,
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
        // Anchor the cursor on the last PR; `prepare` resolves it to its row.
        view.view.inbox_cursor = InboxCursor::Pr("org~repo~29".to_string());
        let layout =
            RootLayout::new(Blade::Inbox).compute(ratatui::layout::Rect::new(0, 0, 111, 30));
        view.prepare(&mut state, &layout);

        // Active blade content height is 25 (full-bleed body under a 5-row
        // chrome); the Inbox blade reserves one row for its column header, so
        // the scrollable viewport is 24 rows. With 30 PRs plus one section
        // header there are 31 content lines; selecting the last PR (line index
        // 30) should scroll so it is the last visible row.
        assert_eq!(
            view.view.inbox_scroll.offset, 7,
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    #[test]
    fn ctrl_y_without_detail_reports_a_copy_error() {
        let mut state = make_test_state(HashMap::new());
        let mut view = ViewStateManager::new();

        let action = state.handle_key(
            &mut view,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
        );

        assert_eq!(action, Action::None);
        assert!(state
            .error_message
            .as_deref()
            .is_some_and(|message| message.starts_with("Copy branch failed:")));
    }

    #[test]
    fn slash_opens_search_overlay() {
        let mut state = make_test_state(HashMap::new());
        let mut view = ViewStateManager::new();
        let action = state.handle_key(&mut view, key(KeyCode::Char('/')));
        assert_eq!(action, Action::None);
        assert!(state.show_search);
        assert!(state.search_filter.is_empty());
    }

    #[test]
    fn slash_clears_active_filter_and_closes_overlay() {
        let mut state = make_test_state(HashMap::new());
        state.show_search = true;
        state.search_filter = "foo".to_string();
        let mut view = ViewStateManager::new();
        let action = state.handle_key(&mut view, key(KeyCode::Char('/')));
        assert_eq!(action, Action::None);
        assert!(!state.show_search);
        assert!(state.search_filter.is_empty());
    }

    #[test]
    fn typing_appends_to_search_filter() {
        let mut state = make_test_state(HashMap::new());
        state.show_search = true;
        let mut view = ViewStateManager::new();
        state.handle_key(&mut view, key(KeyCode::Char('f')));
        state.handle_key(&mut view, key(KeyCode::Char('o')));
        state.handle_key(&mut view, key(KeyCode::Char('o')));
        assert_eq!(state.search_filter, "foo");
        assert!(state.show_search);
    }

    #[test]
    fn backspace_deletes_last_char_and_keeps_overlay_open() {
        let mut state = make_test_state(HashMap::new());
        state.show_search = true;
        state.search_filter = "x".to_string();
        let mut view = ViewStateManager::new();
        let action = state.handle_key(&mut view, key(KeyCode::Backspace));
        assert_eq!(action, Action::None);
        assert!(
            state.show_search,
            "backspace must not close the overlay even when it empties the filter"
        );
        assert!(state.search_filter.is_empty());
    }

    #[test]
    fn backspace_on_empty_filter_is_a_noop() {
        let mut state = make_test_state(HashMap::new());
        state.show_search = true;
        state.search_filter = String::new();
        let mut view = ViewStateManager::new();
        let action = state.handle_key(&mut view, key(KeyCode::Backspace));
        assert_eq!(action, Action::None);
        assert!(state.show_search, "backspace on empty stays open");
        assert!(state.search_filter.is_empty());
    }

    #[test]
    fn esc_cancels_filter_and_restores_prior_value() {
        let mut state = make_test_state(HashMap::new());
        // Open the overlay with an existing filter: `/` snapshots it.
        state.search_filter = "keep".to_string();
        let mut view = ViewStateManager::new();
        state.handle_key(&mut view, key(KeyCode::Char('/')));
        assert!(state.show_search);
        // Edit the live filter, then cancel with Esc.
        state.handle_key(&mut view, key(KeyCode::Char('x')));
        assert_eq!(state.search_filter, "keepx");
        let action = state.handle_key(&mut view, key(KeyCode::Esc));
        assert_eq!(action, Action::None);
        assert!(!state.show_search, "Esc closes the overlay");
        assert_eq!(
            state.search_filter, "keep",
            "Esc restores the filter captured when the overlay opened"
        );
    }

    #[test]
    fn enter_accepts_filter_and_closes_overlay() {
        let mut state = make_test_state(HashMap::new());
        let mut view = ViewStateManager::new();
        state.handle_key(&mut view, key(KeyCode::Char('/')));
        state.handle_key(&mut view, key(KeyCode::Char('a')));
        let action = state.handle_key(&mut view, key(KeyCode::Enter));
        assert_eq!(action, Action::None);
        assert!(!state.show_search);
        assert_eq!(state.search_filter, "a", "Enter keeps the typed filter");
    }

    #[test]
    fn esc_steps_out_to_inbox_from_deeper_blade() {
        let mut state = make_test_state(HashMap::new());
        let mut view = ViewStateManager::new();
        view.view.active_blade = Blade::Files;
        state.handle_key(&mut view, key(KeyCode::Esc));
        assert_eq!(view.view.active_blade, Blade::Inbox);
    }

    #[test]
    fn esc_on_inbox_clears_filter_before_dismissing_toast() {
        let mut state = make_test_state(HashMap::new());
        state.search_filter = "foo".to_string();
        state.error_message = Some("stale".to_string());
        let mut view = ViewStateManager::new();
        assert_eq!(view.view.active_blade, Blade::Inbox);

        state.handle_key(&mut view, key(KeyCode::Esc));
        assert!(
            state.search_filter.is_empty(),
            "first Esc clears the filter"
        );
        assert!(
            state.error_message.is_some(),
            "the toast survives while a filter is still active"
        );

        state.handle_key(&mut view, key(KeyCode::Esc));
        assert!(
            state.error_message.is_none(),
            "with no filter left, Esc dismisses the toast"
        );
    }

    #[test]
    fn g_and_capital_g_jump_inbox_rows() {
        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![
                make_summary("o~r~1", PrGroup::ReviewNeeded, "other", None),
                make_summary("o~r~2", PrGroup::ReviewNeeded, "other", None),
                make_summary("o~r~3", PrGroup::ReviewNeeded, "other", None),
            ],
        );
        let mut state = make_test_state(groups);
        let mut view = ViewStateManager::new();
        let layout =
            RootLayout::new(Blade::Inbox).compute(ratatui::layout::Rect::new(0, 0, 80, 24));
        view.prepare(&mut state, &layout);
        // Rows: [header, o~r~1, o~r~2, o~r~3]; the cursor starts on the first PR.
        state.handle_key(&mut view, key(KeyCode::Char('G')));
        assert_eq!(view.view.selected_row, 3, "G jumps to the last row");
        state.handle_key(&mut view, key(KeyCode::Char('g')));
        assert_eq!(
            view.view.selected_row, 0,
            "g jumps to the first row (header)"
        );
    }

    #[test]
    fn d_toggles_overview_description_expansion() {
        let mut state = make_test_state(HashMap::new());
        let mut view = ViewStateManager::new();
        view.view.active_blade = Blade::Overview;
        assert!(!view.view.overview_description_expanded);
        state.handle_key(&mut view, key(KeyCode::Char('d')));
        assert!(view.view.overview_description_expanded);
        state.handle_key(&mut view, key(KeyCode::Char('d')));
        assert!(!view.view.overview_description_expanded);
    }

    #[test]
    fn help_modal_opens_closes_and_swallows_keys() {
        let mut state = make_test_state(HashMap::new());
        let mut view = ViewStateManager::new();

        // `?` opens it.
        let action = state.handle_key(&mut view, key(KeyCode::Char('?')));
        assert_eq!(action, Action::None);
        assert!(state.show_help);

        // While open, an unrelated key is swallowed — no blade change, no quit.
        let action = state.handle_key(&mut view, key(KeyCode::Char('l')));
        assert_eq!(action, Action::None);
        assert!(state.show_help);
        assert_eq!(view.view.active_blade, Blade::Inbox);

        // `q` closes the modal rather than quitting the app.
        let action = state.handle_key(&mut view, key(KeyCode::Char('q')));
        assert_eq!(action, Action::None);
        assert!(!state.show_help);

        // Esc closes it too.
        state.handle_key(&mut view, key(KeyCode::Char('?')));
        assert!(state.show_help);
        state.handle_key(&mut view, key(KeyCode::Esc));
        assert!(!state.show_help);
    }

    #[test]
    fn question_mark_types_into_filter_when_search_is_open() {
        let mut state = make_test_state(HashMap::new());
        state.show_search = true;
        let mut view = ViewStateManager::new();
        state.handle_key(&mut view, key(KeyCode::Char('?')));
        assert!(!state.show_help, "? must not open help while filtering");
        assert_eq!(state.search_filter, "?");
    }

    #[test]
    fn search_overlay_renders_filter_prompt() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![make_summary("o~r~1", PrGroup::ReviewNeeded, "other", None)],
        );
        let mut state = make_test_state(groups);
        state.show_search = true;
        state.search_filter = "query".to_string();
        state.startup_phase = StartupPhase::Ready;
        let mut view = ViewStateManager::new();

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_frame(f, &mut state, &mut view))
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut found_title = false;
        let mut found_input = false;
        let mut found_hint = false;
        for y in 0..buf.area().height {
            let line: String = (0..buf.area().width)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect();
            if line.contains("Filter") {
                found_title = true;
            }
            if line.contains("Filter: query") {
                found_input = true;
            }
            if line.contains("Backspace delete") {
                found_hint = true;
            }
        }
        assert!(
            found_title,
            "search overlay should render a Filter title/prompt"
        );
        assert!(found_input, "search overlay should render the query prompt");
        assert!(found_hint, "search overlay should render the hint line");
    }

    #[test]
    fn inbox_filters_rows_when_search_filter_is_active() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![
                make_summary("o~r~1", PrGroup::ReviewNeeded, "other", None),
                make_summary("o~r~2", PrGroup::ReviewNeeded, "other", None),
            ],
        );
        let mut state = make_test_state(groups);
        state.startup_phase = StartupPhase::Ready;
        state.search_filter = "r~2".to_string();
        let mut view = ViewStateManager::new();

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_frame(f, &mut state, &mut view))
            .unwrap();

        let buf = terminal.backend().buffer();
        let active = RootLayout::new(Blade::Inbox)
            .compute(ratatui::layout::Rect::new(0, 0, 120, 30))
            .active_content();
        let content = {
            let chunks = ratatui::layout::Layout::default()
                .direction(ratatui::layout::Direction::Vertical)
                .constraints([
                    ratatui::layout::Constraint::Length(1),
                    ratatui::layout::Constraint::Min(1),
                ])
                .split(active);
            chunks[1]
        };

        let mut found_first = false;
        let mut found_second = false;
        for y in content.top()..content.bottom() {
            let line: String = (content.left()..content.right())
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect();
            if line.contains("Test o~r~1") {
                found_first = true;
            }
            if line.contains("Test o~r~2") {
                found_second = true;
            }
        }
        assert!(
            !found_first,
            "filtered Inbox should not render the non-matching PR"
        );
        assert!(found_second, "filtered Inbox should render the matching PR");
    }

    #[test]
    fn slash_with_empty_filter_keeps_overlay_open() {
        let mut state = make_test_state(HashMap::new());
        state.show_search = true;
        state.search_filter = String::new();
        let mut view = ViewStateManager::new();
        let action = state.handle_key(&mut view, key(KeyCode::Char('/')));
        assert_eq!(action, Action::None);
        assert!(state.show_search, "empty-query / should keep overlay open");
    }

    #[test]
    fn review_stub_keys_are_noops() {
        // `r` is no longer a stub — it aliases `R` (refresh) — so it's excluded.
        for c in ['a', 'm'] {
            let mut state = make_test_state(HashMap::new());
            let mut view = ViewStateManager::new();
            let action = state.handle_key(&mut view, key(KeyCode::Char(c)));
            assert_eq!(action, Action::None, "{} should not produce an action", c);
            assert!(
                state.error_message.is_none(),
                "{} should not show a stub toast",
                c
            );
        }
    }

    #[test]
    fn docs_keybindings_document_filter_not_stubs() {
        let readme = include_str!("../../README.md");
        let agents = include_str!("../../AGENTS.md");
        assert!(readme.contains("`/`"));
        assert!(readme.contains("Filter inbox"));
        assert!(!readme.contains("Approve stub"));
        assert!(!readme.contains("Request changes stub"));
        assert!(!readme.contains("Merge stub"));
        assert!(!readme.contains("Search stub"));
        assert!(agents.contains("`/` filter inbox"));
    }

    #[test]
    fn keybar_renders_filter_not_act() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let groups = HashMap::new();
        let mut state = make_test_state(groups);
        state.startup_phase = StartupPhase::Ready;
        let mut view = ViewStateManager::new();
        view.view.active_blade = Blade::Inbox;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_frame(f, &mut state, &mut view))
            .unwrap();

        let buf = terminal.backend().buffer();
        let row = buf.area().height - 1;
        let line: String = (0..buf.area().width)
            .map(|x| buf.cell((x, row)).unwrap().symbol().to_string())
            .collect();
        assert!(
            line.contains("filter") && line.contains("quit"),
            "inbox keybar should show bindings, got: {:?}",
            line
        );
        assert!(
            !line.contains("act"),
            "keybar should not contain the old a/r/m act label, got: {:?}",
            line
        );
    }

    #[test]
    fn tab_line_shows_animated_refresh_while_loading() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let groups = HashMap::new();
        let mut state = make_test_state(groups);
        state.startup_phase = StartupPhase::Ready;
        state.health.as_mut().unwrap().refresh_in_progress = true;
        state.ui_tick = 3;
        let mut view = ViewStateManager::new();
        view.view.active_blade = Blade::Files;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_frame(f, &mut state, &mut view))
            .unwrap();

        // The refresh spinner lives on the tab line (top row) alongside the
        // data-age indicator; ui_tick 3 selects a deterministic braille frame.
        let buf = terminal.backend().buffer();
        let line: String = (0..buf.area().width)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect();
        assert!(
            line.contains('⠸'),
            "tab line should show the refresh spinner while loading, got: {:?}",
            line
        );
    }

    #[test]
    fn coalesce_ticks_collapses_repeated_ticks_and_preserves_other_order() {
        let events = vec![
            TuiEvent::DataTick,
            TuiEvent::UiTick,
            TuiEvent::Resize,
            TuiEvent::DataTick,
            TuiEvent::UiTick,
            TuiEvent::Resize,
        ];
        let (data_tick, ui_tick, rest) = coalesce_ticks(events);
        assert!(data_tick);
        assert!(ui_tick);
        assert_eq!(rest.len(), 2, "only the two Resize events should remain");
        assert!(rest.iter().all(|e| matches!(e, TuiEvent::Resize)));
    }

    #[test]
    fn coalesce_ticks_reports_false_when_no_ticks_queued() {
        let (data_tick, ui_tick, rest) = coalesce_ticks(vec![TuiEvent::Resize]);
        assert!(!data_tick);
        assert!(!ui_tick);
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn accept_prs_clears_inflight_and_applies_success() {
        let mut state = make_test_state(HashMap::new());
        state.refresh_inflight = true;
        state.loading = true;
        let mut groups = HashMap::new();
        groups.insert(
            PrGroup::ReviewNeeded,
            vec![make_summary("a~b~1", PrGroup::ReviewNeeded, "other", None)],
        );
        state.accept_prs(Ok(PrListResponse {
            groups,
            updated_at: "2024-01-01T00:00:00Z".to_string(),
        }));
        assert!(!state.refresh_inflight);
        assert!(!state.loading);
        assert_eq!(state.prs.groups.len(), 1);
    }

    #[test]
    fn accept_prs_clears_inflight_and_toasts_on_failure() {
        let mut state = make_test_state(HashMap::new());
        state.refresh_inflight = true;
        state.accept_prs(Err("boom".to_string()));
        assert!(!state.refresh_inflight);
        assert!(!state.loading);
        assert!(state.error_message.unwrap().contains("boom"));
    }

    #[test]
    fn accept_setup_status_auto_opens_wizard_when_not_ready() {
        let mut state = make_test_state(HashMap::new());
        assert!(state.setup_wizard.is_none());
        state.accept_setup_status(Ok(SetupStatusResponse {
            ready: false,
            ..Default::default()
        }));
        assert!(
            state.setup_wizard.is_some(),
            "an unready setup status should auto-open the wizard"
        );
    }

    #[test]
    fn accept_setup_status_leaves_wizard_closed_when_ready() {
        let mut state = make_test_state(HashMap::new());
        state.accept_setup_status(Ok(SetupStatusResponse {
            ready: true,
            ..Default::default()
        }));
        assert!(state.setup_wizard.is_none());
    }
}
