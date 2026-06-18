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
    // The auto-refresh ticker, kept as a handle so the Settings section can re-arm
    // it (abort + respawn) at a new interval without a restart. `None` ⇒ off.
    let mut refresh_ticker = arm_refresh(&tx, refresh_secs);

    while !app.should_quit {
        terminal.draw(|frame| ui::render(frame, app))?;

        let Some(event) = rx.recv().await else { break };
        // Never await network here — dispatch each command on its own task,
        // which posts results back as AppEvents.
        for command in app.handle_event(event) {
            // `ArmRefresh` re-arms the ticker in place (it owns no client/network):
            // abort the old task and spawn one at the new interval. Everything else
            // is side-effecting work spawned off the render thread.
            if let AppCommand::ArmRefresh(secs) = command {
                if let Some(handle) = refresh_ticker.take() {
                    handle.abort();
                }
                refresh_ticker = arm_refresh(&tx, secs);
            } else {
                dispatch(command, app.client.clone(), tx.clone());
            }
        }
    }
    Ok(())
}

/// Spawn the auto-refresh ticker at `secs` (skipping `None`/`0` = off), returning
/// its [`JoinHandle`](tokio::task::JoinHandle) so it can be aborted on a re-arm.
fn arm_refresh(
    tx: &mpsc::Sender<AppEvent>,
    secs: Option<u64>,
) -> Option<tokio::task::JoinHandle<()>> {
    secs.filter(|s| *s > 0)
        .map(|secs| spawn_ticks(tx.clone(), secs))
}

fn dispatch(command: AppCommand, client: NetBoxClient, tx: mpsc::Sender<AppEvent>) {
    match command {
        AppCommand::Search { query, req } => {
            tokio::spawn(async move {
                let result = client
                    .search(SearchRequest {
                        query,
                        limit: 50,
                        filters: SearchFilters::default(),
                    })
                    .await;
                // Echo the request id back so a stale (superseded) search result
                // is dropped by the pure handler.
                let _ = tx.send(AppEvent::SearchComplete { req, result }).await;
            });
        }
        AppCommand::LoadDetail { kind, id, req } => {
            tokio::spawn(async move {
                let result = load_detail(&client, kind, id).await;
                let _ = tx.send(AppEvent::DetailLoaded { req, result }).await;
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
        AppCommand::LoadByRef { kind, value, req } => {
            tokio::spawn(async move {
                let result = load_detail_by_ref(&client, kind, &value).await;
                let _ = tx.send(AppEvent::DetailLoaded { req, result }).await;
            });
        }
        AppCommand::OpenBrowser { url, command } => {
            tokio::spawn(async move {
                // Honor the live `open_browser_command` (carried on the command):
                // a custom opener runs with the URL appended as a literal final
                // arg; a blank command falls back to the OS default. Never
                // shell-interpolates the URL (see `config::open_url`).
                let opened =
                    tokio::task::spawn_blocking(move || crate::config::open_url(&command, &url))
                        .await;
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
        AppCommand::SwitchProfile {
            id,
            name,
            config,
            config_path,
        } => {
            tokio::spawn(async move {
                // Reconnect the TUI way: rebuild the client from the target
                // profile and re-probe `/api/status/` — the same connect/probe
                // code paths launch uses — off the render thread. Token resolution
                // reads the env + keyring here (not in the pure handler). The
                // switch `id` is echoed back so a superseded switch is dropped on
                // arrival.
                let result = reconnect(&config, config_path.as_deref(), &name).await;
                let _ = tx
                    .send(AppEvent::ProfileSwitched { id, name, result })
                    .await;
            });
        }
        AppCommand::TestConnect { id, req } => {
            tokio::spawn(async move {
                // Build a temporary client from the candidate form fields and probe
                // `/api/status/` via the same `verify_compatible` path a real
                // connect/switch uses — off the render thread. The carried token is
                // a secret; it's consumed here and never logged. The test `id` is
                // echoed back so a superseded test is dropped on arrival.
                let result = test_connect(&req).await;
                let _ = tx.send(AppEvent::ConnectTested { id, result }).await;
            });
        }
        // Re-arming the auto-refresh ticker is handled in the event loop (it owns
        // the ticker handle); it never reaches `dispatch`.
        AppCommand::ArmRefresh(_) => {}
    }
}

/// Build a temporary client for the candidate `req` and probe the instance,
/// returning its NetBox version on success. Reuses [`NetBoxClient::new`] +
/// [`NetBoxClient::verify_compatible`] — the same pair launch/switch use — so a
/// test enforces the same reachability + version floor and surfaces the same
/// errors. The token is moved straight into the client; it is never logged.
async fn test_connect(req: &crate::tui::state::ConnectRequest) -> Result<String> {
    let profile = req.to_profile();
    let client = NetBoxClient::new(&profile, req.token.clone())?;
    let status = client.verify_compatible().await?;
    Ok(status.netbox_version)
}

/// Build a fresh client for `profile` and re-probe the instance, returning the
/// client paired with its NetBox version on success. Reuses
/// [`NetBoxClient::new`] + [`NetBoxClient::verify_compatible`] — the exact pair
/// `run_tui` calls at launch — so a switch enforces the same version floor and
/// surfaces the same errors (unreachable / unsupported) as a fresh start.
async fn reconnect(
    profile: &crate::config::ProfileConfig,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(NetBoxClient, String)> {
    // Token resolution needs the config path + profile name to key the keyring.
    // With no backing file (config_path None), fall back to env-only resolution
    // by keying off an empty path — the keyring lookup just misses.
    let path = config_path.unwrap_or_else(|| std::path::Path::new(""));
    let token = crate::config::resolve_token(profile, path, profile_name);
    let client = NetBoxClient::new(profile, token)?;
    let status = client.verify_compatible().await?;
    Ok((client, status.netbox_version))
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
