//! Terminal event source: forwards crossterm input onto the app channel.

use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use tokio::sync::mpsc::Sender;
use tokio::time::MissedTickBehavior;

use crate::tui::state::AppEvent;

/// Spawn a task that reads terminal events and forwards them as [`AppEvent`]s.
/// Exits when the receiver is dropped.
pub fn spawn_terminal_events(tx: Sender<AppEvent>) {
    tokio::spawn(async move {
        let mut stream = EventStream::new();
        while let Some(Ok(event)) = stream.next().await {
            let app_event = match event {
                // Filter to key presses to avoid duplicate release events.
                Event::Key(key) if key.kind == KeyEventKind::Press => AppEvent::Key(key),
                Event::Resize(w, h) => AppEvent::Resize(w, h),
                _ => continue,
            };
            if tx.send(app_event).await.is_err() {
                break;
            }
        }
    });
}

/// Spawn a task that emits [`AppEvent::Tick`] every `secs` seconds. Exits when
/// the receiver is dropped.
pub fn spawn_ticks(tx: Sender<AppEvent>, secs: u64) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(secs));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker.tick().await; // the first tick fires immediately; skip it
        loop {
            ticker.tick().await;
            if tx.send(AppEvent::Tick).await.is_err() {
                break;
            }
        }
    });
}

/// The preview debounce interval: short enough to feel live, long enough that a
/// burst of cursor movement settles into a single fetch.
const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(180);

/// Spawn a fast, always-on ticker that emits [`AppEvent::PreviewTick`]. The pure
/// handler flushes a settled selection into a single preview load on each tick,
/// so scrolling the list never fires a network call per keystroke. Exits when
/// the receiver is dropped.
pub fn spawn_preview_ticks(tx: Sender<AppEvent>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(PREVIEW_DEBOUNCE);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker.tick().await; // skip the immediate first tick
        loop {
            ticker.tick().await;
            if tx.send(AppEvent::PreviewTick).await.is_err() {
                break;
            }
        }
    });
}
