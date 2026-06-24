//! TUI entry point and event loop.

use anyhow::Result;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use crate::cache::{Cache, CacheKey};
use crate::domain::detail::{load_detail, load_detail_by_ref};
use crate::netbox::client::NetBoxClient;
use crate::netbox::search::SearchRequest;
use crate::tui::events::{AbortOnDrop, spawn_preview_ticks, spawn_terminal_events, spawn_ticks};
use crate::tui::state::{App, AppCommand, AppEvent};
use crate::tui::ui;

/// Set up the terminal, run the loop, and restore on exit (panic-safe via
/// `ratatui::init`'s panic hook).
pub async fn run(mut app: App, refresh_secs: Option<u64>) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = run_on(&mut terminal, &mut app, refresh_secs).await;
    ratatui::restore();
    result
}

/// Run the event loop on an already-initialized `terminal`, persisting the theme
/// on exit. Split from [`run`] so the first-run onboarding wizard can share one
/// terminal with the app loop (no flicker from a re-init between them): the caller
/// owns `ratatui::init`/`restore`, runs the wizard, then hands the terminal here.
pub async fn run_on(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    refresh_secs: Option<u64>,
) -> Result<()> {
    let result = event_loop(terminal, app, refresh_secs).await;

    // Persist only the `[ui]` fields that changed this session, in ONE
    // format-preserving write so a failure can't leave the file half-updated.
    let mut fields = Vec::new();
    // Theme, if it changed.
    if app.theme.name() != app.initial_theme {
        fields.push(crate::config::UiField::Theme(app.theme.name().to_string()));
    }
    // Last-browsed Nav kind, if it moved (so the next launch lands where the
    // user left off).
    let last_browsed = app.last_browsed.map(|k| k.as_str().to_string());
    if last_browsed != app.initial_last_browsed {
        fields.push(crate::config::UiField::LastBrowsed(last_browsed));
    }
    if !fields.is_empty()
        && let Some(path) = &app.config_path
        && let Err(e) = crate::config::save_ui_fields(path, &fields)
    {
        tracing::warn!("failed to persist ui fields: {e:#}");
    }

    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    refresh_secs: Option<u64>,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(64);
    let _terminal_events = AbortOnDrop::new(spawn_terminal_events(tx.clone()));
    // The preview debounce is always on (independent of the optional auto-refresh).
    spawn_preview_ticks(tx.clone());
    // The auto-refresh ticker, kept as a handle so the Settings section can re-arm
    // it (abort + respawn) at a new interval without a restart. `None` ⇒ off.
    let mut refresh_ticker = arm_refresh(&tx, refresh_secs);

    // Kick off the background update check once at startup (only when the `updates`
    // feature is built). The result arrives as an `AppEvent` that drives the
    // dismissible banner; the disk-cached, once-a-day check never blocks the loop.
    #[cfg(feature = "updates")]
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let version = tokio::task::spawn_blocking(crate::update::check_for_update)
                .await
                .ok()
                .flatten();
            let _ = tx.send(AppEvent::UpdateAvailable(version)).await;
        });
    }

    // Kick off the Nav-pane count probe once at startup (background, no spinner);
    // a profile switch issues its own refresh via `LoadNavCounts`.
    dispatch(
        AppCommand::LoadNavCounts,
        app.client.clone(),
        app.cache.clone(),
        tx.clone(),
    );

    // If `[ui].last_browsed` restored a kind, preload its list at startup so the
    // Nav rail lands populated where the user left off (focus stays on Nav). `req`
    // 0 is current since `browse_gen` starts at 0; any user navigation supersedes it.
    if let Some(kind) = app.startup_browse() {
        dispatch(
            AppCommand::Browse {
                kind,
                req: 0,
                filter: None,
            },
            app.client.clone(),
            app.cache.clone(),
            tx.clone(),
        );
    }

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
                dispatch(command, app.client.clone(), app.cache.clone(), tx.clone());
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

fn dispatch(command: AppCommand, client: NetBoxClient, cache: Cache, tx: mpsc::Sender<AppEvent>) {
    match command {
        AppCommand::Search {
            query,
            req,
            filters,
        } => {
            tokio::spawn(async move {
                let result = Box::pin(client.search(SearchRequest {
                    query,
                    limit: 50,
                    filters,
                }))
                .await;
                // Echo the request id back so a stale (superseded) search result
                // is dropped by the pure handler.
                let _ = tx.send(AppEvent::SearchComplete { req, result }).await;
            });
        }
        AppCommand::Browse { kind, req, filter } => {
            tokio::spawn(async move {
                let result = crate::netbox::browse::browse(
                    &client,
                    kind,
                    crate::netbox::browse::BROWSE_CAP,
                    filter.as_deref(),
                )
                .await;
                let _ = tx
                    .send(AppEvent::BrowseComplete { req, kind, result })
                    .await;
            });
        }
        AppCommand::LoadDetail {
            kind,
            id,
            req,
            force,
        } => {
            tokio::spawn(async move {
                let key = CacheKey::detail(kind, id);
                // An explicit refresh busts the entry first so it can't be re-served.
                if force {
                    cache.invalidate(&key);
                }
                match cache
                    .get_or_fetch(&key, || {
                        Box::pin(load_detail(&client, kind, id))
                            as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
                    })
                    .await
                {
                    Ok(c) => {
                        let _ = tx
                            .send(AppEvent::DetailLoaded {
                                req,
                                result: Ok(c.value),
                            })
                            .await;
                        let _ = tx
                            .send(AppEvent::DetailFreshness {
                                req,
                                freshness: c.freshness,
                            })
                            .await;
                    }
                    Err(e) => {
                        let _ = tx
                            .send(AppEvent::DetailLoaded {
                                req,
                                result: Err(e),
                            })
                            .await;
                    }
                }
            });
        }
        AppCommand::LoadDashboard { req } => {
            tokio::spawn(async move {
                let result = crate::netbox::dashboard::load_dashboard(&client).await;
                let _ = tx.send(AppEvent::DashboardLoaded { req, result }).await;
            });
        }
        AppCommand::LoadPrefixTree { req } => {
            tokio::spawn(async move {
                let result = crate::netbox::prefix_tree::load_prefix_tree(&client).await;
                let _ = tx.send(AppEvent::PrefixTreeLoaded { req, result }).await;
            });
        }
        AppCommand::LoadNavCounts => {
            tokio::spawn(async move {
                let counts = crate::netbox::browse::nav_counts(&client).await;
                let _ = tx.send(AppEvent::NavCounts(counts)).await;
            });
        }
        AppCommand::LoadPreview { kind, id } => {
            tokio::spawn(async move {
                // Share the detail cache key: scrolling back over a seen row is an
                // instant hit, and a preview warms the cache so opening that object
                // (LoadDetail, same key) is instant too. Never force — a preview is
                // always happy with a within-TTL copy.
                let key = CacheKey::detail(kind, id);
                let result = cache
                    .get_or_fetch(&key, || {
                        Box::pin(load_detail(&client, kind, id))
                            as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
                    })
                    .await
                    .map(|c| c.value);
                // Tag with (kind, id) so a stale response (cursor moved on) can
                // be dropped by the pure handler.
                let _ = tx.send(AppEvent::PreviewLoaded { kind, id, result }).await;
            });
        }
        AppCommand::LoadByRef {
            kind,
            value,
            req,
            force,
        } => {
            tokio::spawn(async move {
                let key = CacheKey::detail_ref(kind, &value);
                if force {
                    cache.invalidate(&key);
                }
                match cache
                    .get_or_fetch(&key, || load_detail_by_ref(&client, kind, &value))
                    .await
                {
                    Ok(c) => {
                        let _ = tx
                            .send(AppEvent::DetailLoaded {
                                req,
                                result: Ok(c.value),
                            })
                            .await;
                        let _ = tx
                            .send(AppEvent::DetailFreshness {
                                req,
                                freshness: c.freshness,
                            })
                            .await;
                    }
                    Err(e) => {
                        let _ = tx
                            .send(AppEvent::DetailLoaded {
                                req,
                                result: Err(e),
                            })
                            .await;
                    }
                }
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
                // reads the env + config token here (not in the pure handler). The
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
    let client = NetBoxClient::new(&profile, req.resolved_token())?;
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
    // `resolve_token` keeps the config-path + profile-name params for signature
    // stability; with no backing file (config_path None) we pass an empty path.
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
