use crossterm::event::{Event as CrosstermEvent, KeyEvent};
use futures::StreamExt;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::api::{
    ConfigPreviewCountsResponse, DiffResponse, HealthResponse, MembershipsResponse,
    PrDetailResponse, PrListResponse, SetupStatusResponse,
};
use crate::config::Config;

pub struct StartupLoad {
    pub daemon_child: Option<std::process::Child>,
    pub setup_status: SetupStatusResponse,
    pub config: Config,
    pub prs: PrListResponse,
    pub health: Option<HealthResponse>,
}

/// Bundled result of a config/health/setup(/prs) fetch spawned off the
/// render loop. `prs` is only populated by the call sites that need a PR
/// refresh alongside the config data (reloading config, applying a wizard
/// commit) — opening the config view inspector doesn't touch PR data.
pub struct ConfigRefreshBundle {
    pub config: Result<Config, String>,
    pub health: Result<HealthResponse, String>,
    pub setup: Result<SetupStatusResponse, String>,
    pub prs: Option<Result<PrListResponse, String>>,
}

/// Events that the TUI event loop processes.
pub enum TuiEvent {
    Key(KeyEvent),
    /// Terminal resize. ratatui re-queries the terminal size on every draw,
    /// so no payload is needed — this just wakes the render loop.
    Resize,
    /// Periodic tick for data refresh (every 5s)
    DataTick,
    /// Periodic tick for UI animations (every 150ms)
    UiTick,
    /// Result of asynchronous startup loading.
    StartupLoaded(Box<Result<StartupLoad, String>>),
    /// Result of a background PR list refresh (periodic `DataTick` or a
    /// manual `R` refresh). `trigger_error` carries a `POST /prs/refresh`
    /// failure for the manual case; the fetch still proceeds afterward
    /// either way, matching the previous inline-await behavior.
    PrsRefreshed {
        trigger_error: Option<String>,
        prs: Box<Result<PrListResponse, String>>,
        health: Option<HealthResponse>,
    },
    /// Result of fetching config/health/setup for the config inspector
    /// overlay (no PR refresh).
    ConfigViewOpened(ConfigRefreshBundle),
    /// Result of `POST /config/reload` plus the subsequent config/health/
    /// setup/prs refresh. The outer `Result` is the reload call itself.
    ConfigReloaded(Result<ConfigRefreshBundle, String>),
    /// Result of the config/health/setup/prs refresh that follows a
    /// successful wizard config write.
    WizardConfigApplied(ConfigRefreshBundle),
    /// Result of an asynchronous PR detail fetch.
    DetailLoaded(String, Box<Result<PrDetailResponse, String>>),
    /// Result of an asynchronous PR diff fetch.
    DiffLoaded(String, Result<DiffResponse, String>),
    /// Result of an on-demand rich LLM classification.
    LlmClassified(String, Result<(), String>),
    /// Result of polling `/setup/status` from within the setup wizard.
    WizardAuthStatusLoaded(Result<SetupStatusResponse, String>),
    /// Result of fetching the viewer's org/team memberships for the wizard.
    WizardMembershipsLoaded(Result<MembershipsResponse, String>),
    /// Result of a live preview-count fetch for the wizard's draft config.
    WizardPreviewLoaded(Result<ConfigPreviewCountsResponse, String>),
    /// Result of writing the wizard's draft config to disk and reloading it.
    WizardConfigWritten(Result<(), String>),
}

/// Spawn the event producer. Events are sent on the supplied channel.
pub fn spawn_event_loop(event_tx: mpsc::UnboundedSender<TuiEvent>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut event_stream = crossterm::event::EventStream::new();
        let mut data_interval = tokio::time::interval(Duration::from_secs(5));
        let mut ui_interval = tokio::time::interval(Duration::from_millis(150));
        // A stalled render loop (e.g. a slow daemon response) can leave one
        // or more ticks queued; catching up by firing them back-to-back once
        // the loop resumes buys nothing (data_tick coalesces anyway, and
        // ui_tick catch-up would just cause a visible animation jump), so
        // skip missed ticks instead of bursting them.
        data_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ui_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                // Keyboard/terminal events
                Some(Ok(event)) = event_stream.next() => {
                    let sent = match event {
                        CrosstermEvent::Key(key) => event_tx.send(TuiEvent::Key(key)).is_ok(),
                        CrosstermEvent::Resize(_, _) => event_tx.send(TuiEvent::Resize).is_ok(),
                        _ => true,
                    };
                    if !sent {
                        break;
                    }
                }

                _ = data_interval.tick() => {
                    if event_tx.send(TuiEvent::DataTick).is_err() {
                        break;
                    }
                }

                _ = ui_interval.tick() => {
                    if event_tx.send(TuiEvent::UiTick).is_err() {
                        break;
                    }
                }
            }
        }
    })
}
