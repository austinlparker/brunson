use crossterm::event::{Event as CrosstermEvent, KeyEvent};
use futures::StreamExt;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::api::{DiffResponse, PrDetailResponse};

/// Events that the TUI event loop processes.
#[derive(Debug)]
#[allow(dead_code)]
pub enum TuiEvent {
    Key(KeyEvent),
    Resize(u16, u16),
    /// Periodic tick for data refresh (every 5s)
    DataTick,
    /// Periodic tick for UI updates (every 1s)
    UiTick,
    /// Result of an asynchronous PR detail fetch.
    DetailLoaded(String, Box<Result<PrDetailResponse, String>>),
    /// Result of an asynchronous PR diff fetch.
    DiffLoaded(String, Result<DiffResponse, String>),
}

/// Spawn the event producer. Events are sent on the supplied channel.
pub fn spawn_event_loop(event_tx: mpsc::UnboundedSender<TuiEvent>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut event_stream = crossterm::event::EventStream::new();
        let mut data_interval = tokio::time::interval(Duration::from_secs(5));
        let mut ui_interval = tokio::time::interval(Duration::from_secs(1));

        loop {
            tokio::select! {
                // Keyboard/terminal events
                Some(Ok(event)) = event_stream.next() => {
                    match event {
                        CrosstermEvent::Key(key) => {
                            if event_tx.send(TuiEvent::Key(key)).is_err() {
                                break;
                            }
                        }
                        CrosstermEvent::Resize(w, h)
                            if event_tx.send(TuiEvent::Resize(w, h)).is_err() => {
                                break;
                            }
                        _ => {}
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
