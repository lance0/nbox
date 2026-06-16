//! Terminal event source: forwards crossterm input onto the app channel.

use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use tokio::sync::mpsc::Sender;

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
