pub mod poller;
pub mod server;
pub mod store;

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::config::{data_dir, Config};
use crate::daemon::poller::PollState;
use crate::daemon::server::AppState;
use crate::daemon::store::{PrStore, SharedStore};
use crate::github::auth::{resolve_host, resolve_token};
use crate::github::client::GitHubClient;
use crate::github::graphql::fetch_viewer_login;
use crate::llm::classifier::Classifier;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SERVICE_NAME: &str = env!("CARGO_PKG_NAME");

/// Run the daemon: poller + HTTP server.
pub async fn run_daemon(config: Config) -> Result<()> {
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

    // Resolve auth
    let token = resolve_token()?;
    let host = resolve_host();
    let gh_client = GitHubClient::new(token, Some(host))?;

    // Resolve current user
    let current_user = match fetch_viewer_login(&gh_client).await {
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
    };

    // Create shared store
    let store: SharedStore = Arc::new(tokio::sync::RwLock::new(PrStore::new(current_user)));

    // Create poll state (for refresh signal)
    let refresh_notify = Arc::new(Notify::new());
    let poll_state = Arc::new(PollState::new(refresh_notify.clone()));

    // Create LLM classifier if enabled
    let classifier = if config.llm.enabled {
        match Classifier::new(&config.llm) {
            Ok(mut c) => {
                if let Err(e) = c.resolve_model().await {
                    tracing::warn!("Failed to auto-detect LLM model: {}", e);
                }
                Some(Arc::new(c))
            }
            Err(e) => {
                tracing::warn!("Failed to create LLM classifier: {}", e);
                None
            }
        }
    } else {
        None
    };

    // Cancellation token for graceful shutdown
    let shutdown = CancellationToken::new();
    let shutdown_clone = shutdown.clone();

    // Spawn poller
    let poller_store = store.clone();
    let poller_config = config.clone();
    let poller_client = gh_client.clone();
    let poller_state = poll_state.clone();
    let poller_shutdown = shutdown.clone();
    let poller_classifier = classifier.clone();
    tokio::spawn(async move {
        poller::run_poll_loop(
            poller_client,
            poller_store,
            poller_config,
            poller_state,
            poller_classifier,
            poller_shutdown,
        )
        .await;
    });

    // Build app state
    let app_state = Arc::new(AppState {
        store: store.clone(),
        config: config.clone(),
        poll_state: poll_state.clone(),
        classifier: classifier.clone(),
    });

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
    let dir = data_dir()?;
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
    if let Ok(dir) = data_dir() {
        let pid_path = dir.join("daemon.pid");
        let _ = std::fs::remove_file(&pid_path);
    }
}
