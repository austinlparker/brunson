pub mod poller;
pub mod server;
pub mod setup_status;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::config::{Config, LlmConfig};
use crate::daemon::poller::PollState;
use crate::daemon::server::AppState;
use crate::daemon::setup_status::{evaluate_setup, SetupAuthImpl, SetupCache};
use crate::daemon::store::{PrStore, SharedStore};
use crate::github::auth::{resolve_host, resolve_token};
use crate::github::client::GitHubClient;
use crate::github::graphql::fetch_viewer_login;
use crate::llm::classifier::Classifier;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SERVICE_NAME: &str = env!("CARGO_PKG_NAME");

/// Build a classifier from the latest LLM configuration.
pub async fn build_classifier(config: &LlmConfig) -> Option<Arc<Classifier>> {
    if !config.enabled {
        return None;
    }
    match Classifier::new(config) {
        Ok(mut c) => {
            if let Err(e) = c.resolve_model().await {
                warn!("Failed to auto-detect LLM model: {}", e);
                // We still keep the classifier; a configured model name may work.
            }
            Some(Arc::new(c))
        }
        Err(e) => {
            warn!("Failed to create LLM classifier: {}", e);
            None
        }
    }
}

/// Run the daemon: poller + HTTP server.
pub async fn run_daemon(config: Config, config_path: Option<PathBuf>) -> Result<()> {
    let port = config.daemon.port;
    let addr = format!("127.0.0.1:{}", port);

    // Try to bind the TCP listener first (lifecycle check)
    let listener = TcpListener::bind(&addr).await.map_err(|e| {
        anyhow::anyhow!(
            "Failed to bind to {}. \
                 If another {} daemon is running, it will respond on this port. \
                 Error: {}",
            addr,
            SERVICE_NAME,
            e
        )
    })?;

    info!("Daemon listening on {}", addr);

    // Write PID file
    write_pid_file()?;

    // Resolve auth. Do not fail startup if auth is missing; `/setup/status` will
    // report the problem and the daemon can recover via `POST /config/reload`.
    let gh_client: Option<GitHubClient> = match resolve_token() {
        Ok(token) => match GitHubClient::new(token, Some(resolve_host())) {
            Ok(client) => Some(client),
            Err(e) => {
                warn!("Failed to build GitHub client: {}", e);
                None
            }
        },
        Err(e) => {
            warn!("GitHub auth not resolved at startup: {}", e);
            None
        }
    };

    // Resolve current user
    let current_user = if let Some(ref client) = gh_client {
        match fetch_viewer_login(client).await {
            Ok(login) => {
                info!("Authenticated as: {}", login);
                login
            }
            Err(e) => {
                error!(
                    "Failed to fetch viewer login: {}. Starting with empty user.",
                    e
                );
                "unknown".to_string()
            }
        }
    } else {
        "unknown".to_string()
    };

    // Create shared store
    let store: SharedStore = Arc::new(tokio::sync::RwLock::new(PrStore::new(current_user)));

    // Create poll state (for refresh signal)
    let refresh_notify = Arc::new(Notify::new());
    let poll_state = Arc::new(PollState::new(refresh_notify.clone()));

    // Create the config watch channel so the poller and handlers always see
    // the latest configuration without requiring a daemon restart.
    let (config_tx, config_rx) = watch::channel(config.clone());

    // Build app state
    let setup_cache = Arc::new(tokio::sync::RwLock::new(SetupCache::default()));
    let classifier = Arc::new(tokio::sync::RwLock::new(
        build_classifier(&config.llm).await,
    ));
    let gh_client = Arc::new(tokio::sync::RwLock::new(gh_client));
    let app_state = Arc::new(AppState {
        store: store.clone(),
        config_rx,
        config_tx,
        config_path,
        poll_state: poll_state.clone(),
        classifier: classifier.clone(),
        gh_client: gh_client.clone(),
        setup_cache: setup_cache.clone(),
        auth: Box::new(SetupAuthImpl),
    });

    // Cancellation token for graceful shutdown
    let shutdown = CancellationToken::new();
    let shutdown_clone = shutdown.clone();

    // Spawn poller
    let poller_store = store.clone();
    let poller_state = poll_state.clone();
    let poller_shutdown = shutdown.clone();
    let poller_config_rx = app_state.config_rx.clone();
    tokio::spawn(async move {
        poller::run_poll_loop(
            gh_client,
            poller_store,
            poller_config_rx,
            poller_state,
            classifier,
            poller_shutdown,
        )
        .await;
    });

    // Run an eager setup diagnostic so the first /health and /setup/status
    // requests return real data instead of placeholder defaults.
    let initial_setup = evaluate_setup(
        &app_state.latest_config(),
        app_state.config_path.as_deref(),
        app_state.auth.as_ref(),
    )
    .await;
    {
        let mut cache = setup_cache.write().await;
        cache.cached_at = Some(std::time::Instant::now());
        cache.result = initial_setup;
    }

    // Build router
    let app = server::build_router(app_state);

    // Start server with graceful shutdown
    info!("Starting HTTP server on {}", addr);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = shutdown_clone.cancelled() => {
                    info!("Shutdown signal received");
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("Ctrl+C received, shutting down...");
                    shutdown_clone.cancel();
                }
            }
        })
        .await
        .context("HTTP server error")?;

    // Cleanup
    remove_pid_file();
    info!("Daemon stopped");

    Ok(())
}

fn write_pid_file() -> Result<()> {
    let dir = crate::config::data_dir()?;
    std::fs::create_dir_all(&dir)?;
    let pid_path = dir.join("daemon.pid");
    let pid = std::process::id();

    // Write atomically: write to temp then rename
    let tmp_path = dir.join("daemon.pid.tmp");
    std::fs::write(&tmp_path, pid.to_string())?;
    std::fs::rename(&tmp_path, &pid_path)?;

    info!("PID {} written to {}", pid, pid_path.display());
    Ok(())
}

fn remove_pid_file() {
    if let Ok(dir) = crate::config::data_dir() {
        let pid_path = dir.join("daemon.pid");
        let _ = std::fs::remove_file(&pid_path);
    }
}
