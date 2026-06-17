//! TUI entry point and event loop.

use anyhow::Result;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use crate::domain::detail::{load_detail, load_detail_by_ref};
use crate::netbox::client::NetBoxClient;
use crate::netbox::search::{SearchFilters, SearchRequest};
use crate::tui::events::{spawn_preview_ticks, spawn_terminal_events, spawn_ticks};
use crate::tui::state::{App, AppCommand, AppEvent};
use crate::tui::ui;

/// Set up the terminal, run the loop, and restore on exit (panic-safe via
/// `ratatui::init`'s panic hook).
pub async fn run(mut app: App, refresh_secs: Option<u64>) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, refresh_secs).await;
    ratatui::restore();

    // Persist the theme if it changed during the session.
    if app.theme.name() != app.initial_theme
        && let Some(path) = &app.config_path
        && let Err(e) = crate::config::save_ui_theme(path, app.theme.name())
    {
        tracing::warn!("failed to persist theme: {e:#}");
    }

    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    refresh_secs: Option<u64>,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(64);
    spawn_terminal_events(tx.clone());
    // The preview debounce is always on (independent of the optional auto-refresh).
    spawn_preview_ticks(tx.clone());
    if let Some(secs) = refresh_secs.filter(|s| *s > 0) {
        spawn_ticks(tx.clone(), secs);
    }

    while !app.should_quit {
        terminal.draw(|frame| ui::render(frame, app))?;

        let Some(event) = rx.recv().await else { break };
        // Never await network here — dispatch each command on its own task,
        // which posts results back as AppEvents.
        for command in app.handle_event(event) {
            dispatch(command, app.client.clone(), tx.clone());
        }
    }
    Ok(())
}

fn dispatch(command: AppCommand, client: NetBoxClient, tx: mpsc::Sender<AppEvent>) {
    match command {
        AppCommand::Search(query) => {
            tokio::spawn(async move {
                let result = client
                    .search(SearchRequest {
                        query,
                        limit: 50,
                        filters: SearchFilters::default(),
                    })
                    .await;
                let _ = tx.send(AppEvent::SearchComplete(result)).await;
            });
        }
        AppCommand::LoadDetail { kind, id } => {
            tokio::spawn(async move {
                let result = load_detail(&client, kind, id).await;
                let _ = tx.send(AppEvent::DetailLoaded(result)).await;
            });
        }
        AppCommand::LoadPreview { kind, id } => {
            tokio::spawn(async move {
                let result = load_detail(&client, kind, id).await;
                // Tag with (kind, id) so a stale response (cursor moved on) can
                // be dropped by the pure handler.
                let _ = tx.send(AppEvent::PreviewLoaded { kind, id, result }).await;
            });
        }
        AppCommand::LoadByRef { kind, value } => {
            tokio::spawn(async move {
                let result = load_detail_by_ref(&client, kind, &value).await;
                let _ = tx.send(AppEvent::DetailLoaded(result)).await;
            });
        }
        AppCommand::OpenBrowser(url) => {
            tokio::spawn(async move {
                let opened = tokio::task::spawn_blocking(move || open::that(&url)).await;
                let message = match opened {
                    Ok(Ok(())) => "opened in browser".to_string(),
                    Ok(Err(e)) => format!("open failed: {e}"),
                    Err(e) => format!("open failed: {e}"),
                };
                let _ = tx.send(AppEvent::Status(message)).await;
            });
        }
        AppCommand::Copy(text) => {
            tokio::spawn(async move {
                let message = match copy_to_clipboard(&text) {
                    Ok(()) => format!("copied: {text}"),
                    Err(e) => format!("copy failed: {e}"),
                };
                let _ = tx.send(AppEvent::Status(message)).await;
            });
        }
    }
}

#[cfg(feature = "clipboard")]
fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_text(text.to_string())?;
    Ok(())
}

#[cfg(not(feature = "clipboard"))]
fn copy_to_clipboard(_text: &str) -> Result<()> {
    anyhow::bail!("clipboard support was not built in (enable the `clipboard` feature)")
}
