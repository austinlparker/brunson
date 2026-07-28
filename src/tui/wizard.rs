//! In-TUI setup wizard state machine.
//!
//! Replaces the old CLI subprocess wizard (`brunson setup`'s interactive
//! prompts). `SetupWizardState::hydrate` builds wizard state from whatever
//! `Config` is currently loaded, so the same step machine serves both
//! first-run (hydrated from `Config::default()`) and re-editing an existing
//! config (hydrated from `AppState.config`) — there is no separate "create"
//! vs "edit" mode.

use crossterm::event::{KeyCode, KeyEvent};

use crate::api::{ConfigPreviewCountsResponse, MembershipsResponse, SetupStatusResponse};
use crate::config::{Config, DaemonConfig, GithubTarget, TuiConfig};
use crate::tui::app::Action;

/// Lifecycle of one wizard-driven async resource. Key handlers move
/// Idle/Ready/Failed → Requested; the render-loop pump moves Requested →
/// Loading and spawns the fetch; the completion event moves Loading →
/// Ready/Failed. Modeling this as one enum (rather than a `needs_load` /
/// `loading` bool pair plus an `Option` payload) makes states like
/// "needs_load and loading at once" or "loading with a stale error still
/// attached" unrepresentable.
#[derive(Debug, Clone, Default)]
pub enum AsyncResource<T> {
    #[default]
    Idle,
    Requested,
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> AsyncResource<T> {
    /// Ask for a (re)fetch. No-op while a fetch is already in flight or
    /// already queued, so mashing the "recheck"/"refresh" key can't spawn a
    /// second fetch on top of one that hasn't completed yet.
    pub fn request(&mut self) {
        if !matches!(self, AsyncResource::Loading | AsyncResource::Requested) {
            *self = AsyncResource::Requested;
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self, AsyncResource::Loading)
    }

    pub fn value(&self) -> Option<&T> {
        match self {
            AsyncResource::Ready(v) => Some(v),
            _ => None,
        }
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            AsyncResource::Failed(e) => Some(e.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    Welcome,
    AuthCheck,
    WatchMode,
    WatchListInput,
    TargetPicker,
    TargetDetail,
    LivePreview,
    LlmConfig,
    Confirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchModeChoice {
    Everything,
    BroadWatch,
    PreciseTargets,
}

impl WatchModeChoice {
    const ALL: [WatchModeChoice; 3] = [
        WatchModeChoice::Everything,
        WatchModeChoice::BroadWatch,
        WatchModeChoice::PreciseTargets,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|w| *w == self).unwrap_or(0)
    }

    fn from_index(idx: usize) -> Self {
        Self::ALL[idx.min(Self::ALL.len() - 1)]
    }

    pub fn label(self) -> &'static str {
        match self {
            WatchModeChoice::Everything => "Everything involving me (default, no config needed)",
            WatchModeChoice::BroadWatch => "Broad watch list (org/repo names)",
            WatchModeChoice::PreciseTargets => "Precise targets (pick teams from GitHub)",
        }
    }
}

pub struct SetupWizardState {
    pub step: WizardStep,
    step_history: Vec<WizardStep>,
    pub config_path: std::path::PathBuf,

    // Preserved verbatim from the hydrated config; the wizard doesn't edit these.
    daemon_config: DaemonConfig,
    tui_config: TuiConfig,

    // WatchMode / WatchListInput
    pub watch_mode: WatchModeChoice,
    pub watch_raw_input: String,

    // Targets
    pub selected_targets: Vec<GithubTarget>,
    pub target_cursor: usize,
    pub editing_target: Option<GithubTarget>,
    pub editing_target_cursor: usize,
    pub manual_entry_active: bool,
    pub manual_entry_buffer: String,
    pub target_error: Option<String>,

    // Auth
    pub auth: AsyncResource<SetupStatusResponse>,

    // Memberships
    pub memberships: AsyncResource<MembershipsResponse>,

    // Preview
    pub preview: AsyncResource<ConfigPreviewCountsResponse>,

    // LLM
    pub llm_enabled: bool,
    pub llm_provider_idx: usize,
    pub llm_endpoint: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub llm_cursor: usize,
    pub llm_editing_field: bool,

    // Confirm / commit
    pub confirm_scroll: usize,
    pub commit: AsyncResource<()>,
}

const LLM_PROVIDERS: [&str; 2] = ["lm_studio", "openai_compatible"];
/// Rows in the LLM step: enabled toggle, provider cycle, endpoint, api key, model.
const LLM_ROW_COUNT: usize = 5;

impl SetupWizardState {
    /// Build wizard state from `config`. Used both for first-run (pass
    /// `&Config::default()`) and for re-opening the wizard on an
    /// already-configured daemon (pass the currently loaded config).
    pub fn hydrate(config_path: std::path::PathBuf, config: &Config) -> Self {
        let watch_mode = if !config.github.targets.is_empty() {
            WatchModeChoice::PreciseTargets
        } else if !config.github.watch.is_empty() {
            WatchModeChoice::BroadWatch
        } else {
            WatchModeChoice::Everything
        };
        let llm_provider_idx = LLM_PROVIDERS
            .iter()
            .position(|p| *p == config.llm.provider)
            .unwrap_or(0);

        Self {
            step: WizardStep::Welcome,
            step_history: Vec::new(),
            config_path,
            daemon_config: config.daemon.clone(),
            tui_config: config.tui.clone(),
            watch_mode,
            watch_raw_input: config.github.watch.join(", "),
            selected_targets: config.github.targets.clone(),
            target_cursor: 0,
            editing_target: None,
            editing_target_cursor: 0,
            manual_entry_active: false,
            manual_entry_buffer: String::new(),
            target_error: None,
            auth: AsyncResource::Idle,
            memberships: AsyncResource::Idle,
            preview: AsyncResource::Idle,
            llm_enabled: config.llm.enabled,
            llm_provider_idx,
            llm_endpoint: config.llm.endpoint.clone(),
            llm_api_key: config.llm.api_key.clone(),
            llm_model: config.llm.model.clone(),
            llm_cursor: 0,
            llm_editing_field: false,
            confirm_scroll: 0,
            commit: AsyncResource::Idle,
        }
    }

    fn push_step(&mut self, next: WizardStep) {
        self.step_history.push(self.step);
        self.step = next;
    }

    /// Pop back to the previous step, if any. Returns `true` if it moved.
    fn pop_step(&mut self) -> bool {
        match self.step_history.pop() {
            Some(prev) => {
                self.step = prev;
                true
            }
            None => false,
        }
    }

    fn auth_resolved(&self) -> bool {
        self.auth
            .value()
            .is_some_and(|s| s.auth.resolved && s.auth.user.is_some())
    }

    /// The `Config` this wizard state currently represents. Used both as the
    /// live-preview request body and as the final commit payload — one
    /// source of truth for "what config are we building".
    pub fn draft(&self) -> Config {
        let (watch, targets) = match self.watch_mode {
            WatchModeChoice::Everything => (Vec::new(), Vec::new()),
            WatchModeChoice::BroadWatch => (
                self.watch_raw_input
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                Vec::new(),
            ),
            WatchModeChoice::PreciseTargets => (Vec::new(), self.selected_targets.clone()),
        };

        Config {
            github: crate::config::GithubConfig {
                watch,
                targets,
                poll_interval: 300,
            },
            daemon: self.daemon_config.clone(),
            llm: crate::config::LlmConfig {
                enabled: self.llm_enabled,
                provider: LLM_PROVIDERS[self.llm_provider_idx].to_string(),
                endpoint: self.llm_endpoint.clone(),
                api_key: self.llm_api_key.clone(),
                model: self.llm_model.clone(),
                classify_on_change: true,
                max_output_tokens: 4096,
            },
            tui: self.tui_config.clone(),
        }
    }

    /// Teams available for the org currently being edited in `TargetDetail`,
    /// from the fetched membership list (empty if unknown/manual entry).
    pub fn available_teams(&self) -> &[crate::api::TeamMembership] {
        let Some(target) = self.editing_target.as_ref() else {
            return &[];
        };
        let Some(org) = target.org.as_deref() else {
            return &[];
        };
        self.memberships
            .value()
            .and_then(|m| m.orgs.iter().find(|o| o.login == org))
            .map(|o| o.teams.as_slice())
            .unwrap_or(&[])
    }
}

/// Split a wizard-entered `myorg` or `myorg/repo` string into the `org`/`repo`
/// fields of a `GithubTarget`, matching `repo`-takes-priority parsing used by
/// `target_scope` in `github::search`.
pub fn target_org_and_repo(entry: &str) -> (Option<String>, Option<String>) {
    if entry.contains('/') {
        (None, Some(entry.to_string()))
    } else {
        (Some(entry.to_string()), None)
    }
}

pub fn step_keybar_hint(step: WizardStep) -> &'static str {
    match step {
        WizardStep::Welcome => "Enter continue · q close",
        WizardStep::AuthCheck => "r recheck · Enter continue once resolved · Esc back",
        WizardStep::WatchMode => "j/k select · Enter continue · Esc back",
        WizardStep::WatchListInput => "type to edit · Enter continue · Esc back",
        WizardStep::TargetPicker => {
            "j/k select · Enter edit/add · a manual entry · d remove · n next · Esc back"
        }
        WizardStep::TargetDetail => "j/k select · Space toggle · s save · Esc cancel",
        WizardStep::LivePreview => "r refresh · Enter continue · Esc back",
        WizardStep::LlmConfig => "j/k select · Enter edit/toggle · n continue · Esc back",
        WizardStep::Confirm => "j/k scroll · y confirm & write · Esc back · q cancel",
    }
}

// ── Welcome ──

pub fn handle_welcome_key(wizard: &mut SetupWizardState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => Action::CloseWizard,
        KeyCode::Enter => {
            if wizard.auth_resolved() {
                wizard.push_step(WizardStep::WatchMode);
            } else {
                wizard.auth.request();
                wizard.push_step(WizardStep::AuthCheck);
            }
            Action::None
        }
        _ => Action::None,
    }
}

// ── AuthCheck ──

pub fn handle_auth_check_key(wizard: &mut SetupWizardState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => return Action::CloseWizard,
        KeyCode::Esc => {
            wizard.pop_step();
        }
        KeyCode::Char('r') | KeyCode::Char('R') => {
            wizard.auth.request();
        }
        KeyCode::Enter if wizard.auth_resolved() => {
            wizard.push_step(WizardStep::WatchMode);
        }
        _ => {}
    }
    Action::None
}

// ── WatchMode ──

pub fn handle_watch_mode_key(wizard: &mut SetupWizardState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => return Action::CloseWizard,
        KeyCode::Esc => {
            wizard.pop_step();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let idx = (wizard.watch_mode.index() + 1).min(WatchModeChoice::ALL.len() - 1);
            wizard.watch_mode = WatchModeChoice::from_index(idx);
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let idx = wizard.watch_mode.index().saturating_sub(1);
            wizard.watch_mode = WatchModeChoice::from_index(idx);
        }
        KeyCode::Enter => match wizard.watch_mode {
            WatchModeChoice::Everything => wizard.push_step(WizardStep::LivePreview),
            WatchModeChoice::BroadWatch => wizard.push_step(WizardStep::WatchListInput),
            WatchModeChoice::PreciseTargets => {
                if wizard.memberships.value().is_none() {
                    wizard.memberships.request();
                }
                wizard.push_step(WizardStep::TargetPicker);
            }
        },
        _ => {}
    }
    Action::None
}

// ── WatchListInput ──

pub fn handle_watch_list_input_key(wizard: &mut SetupWizardState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Esc => {
            wizard.pop_step();
        }
        KeyCode::Char(c) => wizard.watch_raw_input.push(c),
        KeyCode::Backspace => {
            wizard.watch_raw_input.pop();
        }
        KeyCode::Enter => {
            wizard.preview.request();
            wizard.push_step(WizardStep::LivePreview);
        }
        _ => {}
    }
    Action::None
}

// ── TargetPicker ──

/// Flattened rows shown in the picker: one per known org, plus a trailing
/// "add manually" action.
fn target_picker_row_count(wizard: &SetupWizardState) -> usize {
    let orgs = wizard.memberships.value().map_or(0, |m| m.orgs.len());
    orgs + 1
}

pub fn handle_target_picker_key(wizard: &mut SetupWizardState, key: KeyEvent) -> Action {
    if wizard.manual_entry_active {
        match key.code {
            KeyCode::Esc => {
                wizard.manual_entry_active = false;
                wizard.manual_entry_buffer.clear();
            }
            KeyCode::Char(c) => wizard.manual_entry_buffer.push(c),
            KeyCode::Backspace => {
                wizard.manual_entry_buffer.pop();
            }
            KeyCode::Enter => {
                let entry = wizard.manual_entry_buffer.trim().to_string();
                wizard.manual_entry_active = false;
                wizard.manual_entry_buffer.clear();
                if !entry.is_empty() {
                    let (org, repo) = target_org_and_repo(&entry);
                    wizard.editing_target = Some(GithubTarget {
                        org,
                        repo,
                        direct_review_requests: true,
                        team_review_requests: Vec::new(),
                        include_authored: true,
                        include_involved: false,
                    });
                    wizard.editing_target_cursor = 0;
                    wizard.push_step(WizardStep::TargetDetail);
                }
            }
            _ => {}
        }
        return Action::None;
    }

    let row_count = target_picker_row_count(wizard);
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => return Action::CloseWizard,
        KeyCode::Esc => {
            wizard.pop_step();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if row_count > 0 {
                wizard.target_cursor = (wizard.target_cursor + 1).min(row_count - 1);
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            wizard.target_cursor = wizard.target_cursor.saturating_sub(1);
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            wizard.manual_entry_active = true;
            wizard.manual_entry_buffer.clear();
        }
        KeyCode::Char('d') | KeyCode::Char('D') => {
            if wizard.target_cursor < wizard.selected_targets.len() {
                wizard.selected_targets.remove(wizard.target_cursor);
                wizard.target_cursor = wizard
                    .target_cursor
                    .min(wizard.selected_targets.len().saturating_sub(1));
            }
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            if !wizard.selected_targets.is_empty() {
                wizard.preview.request();
                wizard.push_step(WizardStep::LivePreview);
            }
        }
        KeyCode::Enter => {
            let orgs_len = wizard.memberships.value().map_or(0, |m| m.orgs.len());
            if wizard.target_cursor >= orgs_len {
                // "Add manually" row.
                wizard.manual_entry_active = true;
                wizard.manual_entry_buffer.clear();
            } else if let Some(memberships) = wizard.memberships.value() {
                let org = &memberships.orgs[wizard.target_cursor];
                let existing = wizard
                    .selected_targets
                    .iter()
                    .find(|t| t.org.as_deref() == Some(org.login.as_str()))
                    .cloned();
                wizard.editing_target = Some(existing.unwrap_or(GithubTarget {
                    org: Some(org.login.clone()),
                    repo: None,
                    direct_review_requests: true,
                    team_review_requests: Vec::new(),
                    include_authored: true,
                    include_involved: false,
                }));
                wizard.editing_target_cursor = 0;
                wizard.push_step(WizardStep::TargetDetail);
            }
        }
        _ => {}
    }
    Action::None
}

// ── TargetDetail ──

fn target_detail_row_count(wizard: &SetupWizardState) -> usize {
    3 + wizard.available_teams().len()
}

pub fn handle_target_detail_key(wizard: &mut SetupWizardState, key: KeyEvent) -> Action {
    let row_count = target_detail_row_count(wizard);
    match key.code {
        KeyCode::Esc => {
            wizard.editing_target = None;
            wizard.target_error = None;
            wizard.pop_step();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if row_count > 0 {
                wizard.editing_target_cursor =
                    (wizard.editing_target_cursor + 1).min(row_count - 1);
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            wizard.editing_target_cursor = wizard.editing_target_cursor.saturating_sub(1);
        }
        KeyCode::Char(' ') | KeyCode::Enter => {
            let teams: Vec<String> = wizard
                .available_teams()
                .iter()
                .map(|t| format!("{}/{}", team_org(wizard), t.slug))
                .collect();
            let Some(target) = wizard.editing_target.as_mut() else {
                return Action::None;
            };
            match wizard.editing_target_cursor {
                0 => target.direct_review_requests = !target.direct_review_requests,
                1 => target.include_authored = !target.include_authored,
                2 => target.include_involved = !target.include_involved,
                idx => {
                    if let Some(team) = teams.get(idx - 3) {
                        if let Some(pos) =
                            target.team_review_requests.iter().position(|t| t == team)
                        {
                            target.team_review_requests.remove(pos);
                        } else {
                            target.team_review_requests.push(team.clone());
                        }
                    }
                }
            }
        }
        KeyCode::Char('s') | KeyCode::Char('S') => {
            let Some(target) = wizard.editing_target.clone() else {
                return Action::None;
            };
            if !target.direct_review_requests
                && target.team_review_requests.is_empty()
                && !target.include_authored
                && !target.include_involved
            {
                wizard.target_error =
                    Some("Enable at least one of direct/team/authored/involved".to_string());
                return Action::None;
            }
            wizard.target_error = None;
            let existing_idx = wizard
                .selected_targets
                .iter()
                .position(|t| t.org == target.org && t.repo == target.repo);
            match existing_idx {
                Some(idx) => wizard.selected_targets[idx] = target,
                None => wizard.selected_targets.push(target),
            }
            wizard.editing_target = None;
            wizard.pop_step();
        }
        _ => {}
    }
    Action::None
}

fn team_org(wizard: &SetupWizardState) -> String {
    wizard
        .editing_target
        .as_ref()
        .and_then(|t| t.org.clone())
        .unwrap_or_default()
}

// ── LivePreview ──

pub fn handle_live_preview_key(wizard: &mut SetupWizardState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => return Action::CloseWizard,
        KeyCode::Esc => {
            wizard.pop_step();
        }
        KeyCode::Char('r') | KeyCode::Char('R') => {
            wizard.preview.request();
        }
        KeyCode::Enter => {
            wizard.push_step(WizardStep::LlmConfig);
        }
        _ => {}
    }
    Action::None
}

// ── LlmConfig ──

pub fn handle_llm_config_key(wizard: &mut SetupWizardState, key: KeyEvent) -> Action {
    if wizard.llm_editing_field {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => wizard.llm_editing_field = false,
            KeyCode::Char(c) => {
                field_buffer_mut(wizard).push(c);
            }
            KeyCode::Backspace => {
                field_buffer_mut(wizard).pop();
            }
            _ => {}
        }
        return Action::None;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => return Action::CloseWizard,
        KeyCode::Esc => {
            wizard.pop_step();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            wizard.llm_cursor = (wizard.llm_cursor + 1).min(LLM_ROW_COUNT - 1);
        }
        KeyCode::Up | KeyCode::Char('k') => {
            wizard.llm_cursor = wizard.llm_cursor.saturating_sub(1);
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            wizard.push_step(WizardStep::Confirm);
        }
        KeyCode::Enter | KeyCode::Char(' ') => match wizard.llm_cursor {
            0 => wizard.llm_enabled = !wizard.llm_enabled,
            1 => wizard.llm_provider_idx = (wizard.llm_provider_idx + 1) % LLM_PROVIDERS.len(),
            _ => wizard.llm_editing_field = true,
        },
        _ => {}
    }
    Action::None
}

fn field_buffer_mut(wizard: &mut SetupWizardState) -> &mut String {
    match wizard.llm_cursor {
        3 => &mut wizard.llm_api_key,
        4 => &mut wizard.llm_model,
        _ => &mut wizard.llm_endpoint,
    }
}

// ── Confirm ──

pub fn handle_confirm_key(wizard: &mut SetupWizardState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => return Action::CloseWizard,
        KeyCode::Esc => {
            wizard.pop_step();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            wizard.confirm_scroll = wizard.confirm_scroll.saturating_add(1);
        }
        KeyCode::Up | KeyCode::Char('k') => {
            wizard.confirm_scroll = wizard.confirm_scroll.saturating_sub(1);
        }
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            wizard.commit.request();
        }
        _ => {}
    }
    Action::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn hydrate_default() -> SetupWizardState {
        SetupWizardState::hydrate(
            std::path::PathBuf::from("/tmp/config.toml"),
            &Config::default(),
        )
    }

    #[test]
    fn target_org_and_repo_splits_repo_form() {
        assert_eq!(
            target_org_and_repo("myorg/myrepo"),
            (None, Some("myorg/myrepo".to_string()))
        );
    }

    #[test]
    fn target_org_and_repo_treats_bare_name_as_org() {
        assert_eq!(
            target_org_and_repo("myorg"),
            (Some("myorg".to_string()), None)
        );
    }

    #[test]
    fn hydrate_infers_watch_mode_from_config_shape() {
        let mut cfg = Config::default();
        assert_eq!(
            SetupWizardState::hydrate(default_test_path(), &cfg).watch_mode,
            WatchModeChoice::Everything
        );

        cfg.github.watch = vec!["myorg".to_string()];
        assert_eq!(
            SetupWizardState::hydrate(default_test_path(), &cfg).watch_mode,
            WatchModeChoice::BroadWatch
        );

        cfg.github.targets.push(GithubTarget {
            org: Some("myorg".to_string()),
            repo: None,
            direct_review_requests: true,
            team_review_requests: vec![],
            include_authored: true,
            include_involved: false,
        });
        assert_eq!(
            SetupWizardState::hydrate(default_test_path(), &cfg).watch_mode,
            WatchModeChoice::PreciseTargets
        );
    }

    fn default_test_path() -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp/config.toml")
    }

    #[test]
    fn draft_round_trips_broad_watch() {
        let mut cfg = Config::default();
        cfg.github.watch = vec!["myorg".to_string(), "myorg/repo".to_string()];
        let wizard = SetupWizardState::hydrate(default_test_path(), &cfg);
        let draft = wizard.draft();
        assert_eq!(draft.github.watch, cfg.github.watch);
        assert!(draft.github.targets.is_empty());
    }

    #[test]
    fn draft_round_trips_precise_targets() {
        let mut cfg = Config::default();
        cfg.github.targets.push(GithubTarget {
            org: Some("myorg".to_string()),
            repo: None,
            direct_review_requests: true,
            team_review_requests: vec!["myorg/team-a".to_string()],
            include_authored: false,
            include_involved: false,
        });
        let wizard = SetupWizardState::hydrate(default_test_path(), &cfg);
        let draft = wizard.draft();
        assert!(draft.github.watch.is_empty());
        assert_eq!(draft.github.targets.len(), 1);
        assert_eq!(draft.github.targets[0].org.as_deref(), Some("myorg"));
        assert_eq!(
            draft.github.targets[0].team_review_requests,
            vec!["myorg/team-a".to_string()]
        );
    }

    #[test]
    fn draft_preserves_daemon_and_tui_config_untouched() {
        let mut cfg = Config::default();
        cfg.daemon.port = 12345;
        cfg.tui.show_line_numbers = false;
        let wizard = SetupWizardState::hydrate(default_test_path(), &cfg);
        let draft = wizard.draft();
        assert_eq!(draft.daemon.port, 12345);
        assert!(!draft.tui.show_line_numbers);
    }

    #[test]
    fn welcome_enter_skips_auth_check_when_already_resolved() {
        let mut wizard = hydrate_default();
        wizard.auth = AsyncResource::Ready(SetupStatusResponse {
            ready: true,
            status: "ready".to_string(),
            auth: crate::api::AuthStatus {
                resolved: true,
                source: None,
                user: Some("me".to_string()),
            },
            llm: crate::api::LlmSetupStatus::default(),
            next_steps: vec![],
        });
        handle_welcome_key(&mut wizard, key(KeyCode::Enter));
        assert_eq!(wizard.step, WizardStep::WatchMode);
    }

    #[test]
    fn welcome_enter_goes_to_auth_check_when_unresolved() {
        let mut wizard = hydrate_default();
        handle_welcome_key(&mut wizard, key(KeyCode::Enter));
        assert_eq!(wizard.step, WizardStep::AuthCheck);
        assert!(matches!(wizard.auth, AsyncResource::Requested));
    }

    #[test]
    fn watch_mode_navigation_and_selection() {
        let mut wizard = hydrate_default();
        wizard.step = WizardStep::WatchMode;
        handle_watch_mode_key(&mut wizard, key(KeyCode::Down));
        assert_eq!(wizard.watch_mode, WatchModeChoice::BroadWatch);
        handle_watch_mode_key(&mut wizard, key(KeyCode::Enter));
        assert_eq!(wizard.step, WizardStep::WatchListInput);
    }

    #[test]
    fn watch_mode_precise_targets_triggers_membership_load() {
        let mut wizard = hydrate_default();
        wizard.step = WizardStep::WatchMode;
        wizard.watch_mode = WatchModeChoice::PreciseTargets;
        handle_watch_mode_key(&mut wizard, key(KeyCode::Enter));
        assert_eq!(wizard.step, WizardStep::TargetPicker);
        assert!(matches!(wizard.memberships, AsyncResource::Requested));
    }

    #[test]
    fn esc_pops_step_history() {
        let mut wizard = hydrate_default();
        wizard.push_step(WizardStep::WatchMode);
        wizard.push_step(WizardStep::WatchListInput);
        handle_watch_list_input_key(&mut wizard, key(KeyCode::Esc));
        assert_eq!(wizard.step, WizardStep::WatchMode);
    }

    #[test]
    fn watch_list_input_appends_and_backspaces() {
        let mut wizard = hydrate_default();
        wizard.step = WizardStep::WatchListInput;
        handle_watch_list_input_key(&mut wizard, key(KeyCode::Char('a')));
        handle_watch_list_input_key(&mut wizard, key(KeyCode::Char('b')));
        assert_eq!(wizard.watch_raw_input, "ab");
        handle_watch_list_input_key(&mut wizard, key(KeyCode::Backspace));
        assert_eq!(wizard.watch_raw_input, "a");
    }

    #[test]
    fn target_picker_manual_entry_creates_editing_target() {
        let mut wizard = hydrate_default();
        wizard.step = WizardStep::TargetPicker;
        handle_target_picker_key(&mut wizard, key(KeyCode::Char('a')));
        assert!(wizard.manual_entry_active);
        for c in "myorg/myrepo".chars() {
            handle_target_picker_key(&mut wizard, key(KeyCode::Char(c)));
        }
        handle_target_picker_key(&mut wizard, key(KeyCode::Enter));
        assert_eq!(wizard.step, WizardStep::TargetDetail);
        let target = wizard.editing_target.as_ref().unwrap();
        assert_eq!(target.repo.as_deref(), Some("myorg/myrepo"));
    }

    #[test]
    fn target_detail_save_rejects_target_with_no_relationship_enabled() {
        let mut wizard = hydrate_default();
        wizard.editing_target = Some(GithubTarget {
            org: Some("myorg".to_string()),
            repo: None,
            direct_review_requests: false,
            team_review_requests: vec![],
            include_authored: false,
            include_involved: false,
        });
        wizard.step = WizardStep::TargetDetail;
        handle_target_detail_key(&mut wizard, key(KeyCode::Char('s')));
        assert!(wizard.target_error.is_some());
        assert_eq!(wizard.step, WizardStep::TargetDetail);
        assert!(wizard.editing_target.is_some());
    }

    #[test]
    fn target_detail_save_adds_target_and_returns_to_picker() {
        let mut wizard = hydrate_default();
        // Mirror the real transition (handle_target_picker_key's Enter arm):
        // currently on TargetPicker, push into TargetDetail.
        wizard.step = WizardStep::TargetPicker;
        wizard.push_step(WizardStep::TargetDetail);
        wizard.editing_target = Some(GithubTarget {
            org: Some("myorg".to_string()),
            repo: None,
            direct_review_requests: true,
            team_review_requests: vec![],
            include_authored: true,
            include_involved: false,
        });
        handle_target_detail_key(&mut wizard, key(KeyCode::Char('s')));
        assert_eq!(wizard.step, WizardStep::TargetPicker);
        assert_eq!(wizard.selected_targets.len(), 1);
        assert!(wizard.editing_target.is_none());
    }

    #[test]
    fn target_detail_toggle_direct_review_requests() {
        let mut wizard = hydrate_default();
        wizard.editing_target = Some(GithubTarget {
            org: Some("myorg".to_string()),
            repo: None,
            direct_review_requests: true,
            team_review_requests: vec![],
            include_authored: true,
            include_involved: false,
        });
        wizard.editing_target_cursor = 0;
        handle_target_detail_key(&mut wizard, key(KeyCode::Char(' ')));
        assert!(!wizard.editing_target.unwrap().direct_review_requests);
    }

    #[test]
    fn llm_config_enter_toggles_enabled_on_row_zero() {
        let mut wizard = hydrate_default();
        wizard.step = WizardStep::LlmConfig;
        assert!(!wizard.llm_enabled);
        handle_llm_config_key(&mut wizard, key(KeyCode::Enter));
        assert!(wizard.llm_enabled);
    }

    #[test]
    fn llm_config_text_field_editing() {
        let mut wizard = hydrate_default();
        wizard.step = WizardStep::LlmConfig;
        wizard.llm_cursor = 2; // endpoint field
        handle_llm_config_key(&mut wizard, key(KeyCode::Enter));
        assert!(wizard.llm_editing_field);
        handle_llm_config_key(&mut wizard, key(KeyCode::Char('x')));
        assert_eq!(wizard.llm_endpoint, "x");
        handle_llm_config_key(&mut wizard, key(KeyCode::Enter));
        assert!(!wizard.llm_editing_field);
    }

    #[test]
    fn confirm_y_requests_commit() {
        let mut wizard = hydrate_default();
        wizard.step = WizardStep::Confirm;
        handle_confirm_key(&mut wizard, key(KeyCode::Char('y')));
        assert!(matches!(wizard.commit, AsyncResource::Requested));
    }

    #[test]
    fn confirm_y_does_not_double_spawn_commit_while_in_flight() {
        // Regression test: the old `commit_needs_write` bool had no guard
        // against a second 'y' press while a write was already in flight,
        // which could fire a second config write + reload on top of the
        // first (and left a stale `commit_error` displayed alongside the
        // "writing..." spinner if the first attempt had previously failed).
        let mut wizard = hydrate_default();
        wizard.step = WizardStep::Confirm;
        handle_confirm_key(&mut wizard, key(KeyCode::Char('y')));
        assert!(matches!(wizard.commit, AsyncResource::Requested));
        // Simulate the render-loop pump: Requested -> Loading, spawn.
        wizard.commit = AsyncResource::Loading;
        handle_confirm_key(&mut wizard, key(KeyCode::Char('y')));
        assert!(matches!(wizard.commit, AsyncResource::Loading));
    }

    #[test]
    fn async_resource_request_noop_while_requested_or_loading() {
        let mut r: AsyncResource<i32> = AsyncResource::Requested;
        r.request();
        assert!(matches!(r, AsyncResource::Requested));

        let mut r: AsyncResource<i32> = AsyncResource::Loading;
        r.request();
        assert!(matches!(r, AsyncResource::Loading));
    }

    #[test]
    fn async_resource_request_from_ready_or_failed_reenters_requested() {
        let mut r = AsyncResource::Ready(5);
        r.request();
        assert!(matches!(r, AsyncResource::Requested));

        let mut r: AsyncResource<i32> = AsyncResource::Failed("boom".to_string());
        r.request();
        assert!(matches!(r, AsyncResource::Requested));
    }

    #[test]
    fn auth_check_recheck_allowed_after_ready() {
        // Some key handlers legitimately re-request after a resource is
        // already Ready (e.g. 'r' to recheck auth, refresh a preview after
        // the draft changed) — Ready -> Requested must stay allowed.
        let mut wizard = hydrate_default();
        wizard.step = WizardStep::AuthCheck;
        wizard.auth = AsyncResource::Ready(SetupStatusResponse {
            ready: false,
            status: "pending".to_string(),
            auth: crate::api::AuthStatus {
                resolved: false,
                source: None,
                user: None,
            },
            llm: crate::api::LlmSetupStatus::default(),
            next_steps: vec![],
        });
        handle_auth_check_key(&mut wizard, key(KeyCode::Char('r')));
        assert!(matches!(wizard.auth, AsyncResource::Requested));
    }

    #[test]
    fn auth_check_full_lifecycle_via_key_pump_and_completion() {
        // Drives the whole Idle -> Requested -> Loading -> Ready path a key
        // handler + the render-loop pump + a completion event would produce,
        // without needing the daemon client or tokio runtime.
        let mut wizard = hydrate_default();
        wizard.step = WizardStep::AuthCheck;

        // Key handler: 'r' requests a recheck.
        handle_auth_check_key(&mut wizard, key(KeyCode::Char('r')));
        assert!(matches!(wizard.auth, AsyncResource::Requested));

        // Render-loop pump: Requested -> Loading, spawn (mirrors app.rs).
        if matches!(wizard.auth, AsyncResource::Requested) {
            wizard.auth = AsyncResource::Loading;
        }
        assert!(wizard.auth.is_loading());

        // Completion event: mirrors the `TuiEvent::WizardAuthStatusLoaded`
        // success arm.
        wizard.auth = AsyncResource::Ready(SetupStatusResponse {
            ready: true,
            status: "ready".to_string(),
            auth: crate::api::AuthStatus {
                resolved: true,
                source: None,
                user: Some("me".to_string()),
            },
            llm: crate::api::LlmSetupStatus::default(),
            next_steps: vec![],
        });
        assert!(!wizard.auth.is_loading());
        assert!(wizard.auth_resolved());
    }

    #[test]
    fn step_keybar_hint_is_nonempty_for_every_step() {
        let steps = [
            WizardStep::Welcome,
            WizardStep::AuthCheck,
            WizardStep::WatchMode,
            WizardStep::WatchListInput,
            WizardStep::TargetPicker,
            WizardStep::TargetDetail,
            WizardStep::LivePreview,
            WizardStep::LlmConfig,
            WizardStep::Confirm,
        ];
        for step in steps {
            assert!(!step_keybar_hint(step).is_empty());
        }
    }
}
