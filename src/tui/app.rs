//! TUI entry point and event loop.

use anyhow::Result;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use crate::netbox::client::NetBoxClient;
use crate::netbox::search::SearchRequest;
use crate::tui::events::spawn_terminal_events;
use crate::tui::state::{App, AppCommand, AppEvent};
use crate::tui::ui;

/// Set up the terminal, run the loop, and restore on exit (panic-safe via
/// `ratatui::init`'s panic hook).
pub async fn run(app: App) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, app).await;
    ratatui::restore();
    result
}

async fn event_loop(terminal: &mut DefaultTerminal, mut app: App) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(64);
    spawn_terminal_events(tx.clone());

    while !app.should_quit {
        terminal.draw(|frame| ui::render(frame, &app))?;

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
                let result = client.search(SearchRequest { query, limit: 50 }).await;
                let _ = tx.send(AppEvent::SearchComplete(result)).await;
            });
        }
    }
}
