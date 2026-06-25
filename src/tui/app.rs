//! TUI entry point and event loop.

use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use ratatui::backend::Backend;
use ratatui::{DefaultTerminal, Terminal};
use std::io::Write;
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
    // a profile switch issues its own refresh via `LoadNavCounts`. The in-flight
    // preview handle is tracked across dispatch calls so a superseded preview
    // fetch can be aborted (see `LoadPreview`).
    let mut preview_task: Option<tokio::task::JoinHandle<()>> = None;
    dispatch(
        AppCommand::LoadNavCounts,
        app.client.clone(),
        app.cache.clone(),
        tx.clone(),
        &mut preview_task,
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
            &mut preview_task,
        );
    }

    let mut render_gate = RenderGate::default();
    while !app.should_quit {
        draw_if_dirty(terminal, app, &mut render_gate)?;

        let Some(event) = rx.recv().await else { break };
        // Never await network here — dispatch each command on its own task,
        // which posts results back as AppEvents.
        let commands = match event {
            AppEvent::CopyViaTerminal(text) => {
                app.handle_event(AppEvent::Status(copy_via_terminal_status(&text)))
            }
            event => app.handle_event(event),
        };
        for command in commands {
            // `ArmRefresh` re-arms the ticker in place (it owns no client/network):
            // abort the old task and spawn one at the new interval. Everything else
            // is side-effecting work spawned off the render thread.
            if let AppCommand::ArmRefresh(secs) = command {
                if let Some(handle) = refresh_ticker.take() {
                    handle.abort();
                }
                refresh_ticker = arm_refresh(&tx, secs);
            } else {
                dispatch(
                    command,
                    app.client.clone(),
                    app.cache.clone(),
                    tx.clone(),
                    &mut preview_task,
                );
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RenderSignature {
    digest: u64,
}

impl RenderSignature {
    fn new(app: &App, size: (u16, u16)) -> Self {
        Self {
            digest: app.render_digest(size),
        }
    }
}

#[derive(Debug, Default)]
struct RenderGate {
    last_drawn: Option<RenderSignature>,
}

impl RenderGate {
    fn needs_draw(&self, app: &App, size: (u16, u16)) -> bool {
        self.last_drawn != Some(RenderSignature::new(app, size))
    }

    fn record_drawn(&mut self, app: &App, size: (u16, u16)) {
        self.last_drawn = Some(RenderSignature::new(app, size));
    }
}

fn draw_if_dirty<B>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    gate: &mut RenderGate,
) -> Result<bool>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let size = terminal.size()?;
    let size = (size.width, size.height);
    if !gate.needs_draw(app, size) {
        return Ok(false);
    }
    terminal.draw(|frame| ui::render(frame, app))?;
    gate.record_drawn(app, size);
    Ok(true)
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

fn dispatch(
    command: AppCommand,
    client: NetBoxClient,
    cache: Cache,
    tx: mpsc::Sender<AppEvent>,
    preview_task: &mut Option<tokio::task::JoinHandle<()>>,
) {
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
                    .send(AppEvent::BrowseComplete {
                        req,
                        kind,
                        filter,
                        result,
                    })
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
            // Abort any superseded preview fetch before starting a new one. The
            // debounce coalesces a burst of j/k into one load after the cursor
            // settles, but a second settle can land while the first fetch is
            // still in flight (NetBox detail fetches take hundreds of ms to
            // seconds); aborting frees the connection + CPU instead of letting
            // the abandoned task run to completion and be dropped on arrival.
            // Safe with the cache: get_or_fetch's per-key async mutex releases on
            // future drop, so a concurrent open of the same object re-acquires
            // and re-fetches — no deadlock, no poisoned entry.
            if let Some(h) = preview_task.take() {
                h.abort();
            }
            *preview_task = Some(spawn_preview(kind, id, client, cache, tx));
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
            dispatch_copy(text, tx);
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

/// Spawn a background preview fetch for `(kind, id)`, returning its handle so the
/// caller can [`tokio::task::JoinHandle::abort`] a superseded one. Shares the
/// detail cache key with `LoadDetail` so scrolling back over a seen row is an
/// instant hit and a preview warms the cache for opening that object; never
/// forces (a preview is always happy with a within-TTL copy).
fn spawn_preview(
    kind: crate::netbox::search::ObjectKind,
    id: u64,
    client: NetBoxClient,
    cache: Cache,
    tx: mpsc::Sender<AppEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let key = CacheKey::detail(kind, id);
        let result = cache
            .get_or_fetch(&key, || {
                Box::pin(load_detail(&client, kind, id))
                    as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            })
            .await
            .map(|c| c.value);
        // Tag with (kind, id) so a stale response (cursor moved on) is dropped
        // by the pure handler — the abort here avoids even running it.
        let _ = tx.send(AppEvent::PreviewLoaded { kind, id, result }).await;
    })
}

#[cfg(feature = "clipboard")]
fn dispatch_copy(text: String, tx: mpsc::Sender<AppEvent>) {
    let env = ClipboardEnv::from_process();
    if should_skip_desktop_clipboard(ClipboardPlatform::CURRENT, &env) {
        send_status(&tx, copy_via_terminal_status(&text));
        return;
    }

    tokio::spawn(async move {
        let message = match copy_to_desktop_clipboard(&text) {
            Ok(()) => CopyMethod::System.status_message(&text),
            Err(e) => {
                tracing::debug!("desktop clipboard failed; falling back to OSC 52: {e:#}");
                let _ = tx.send(AppEvent::CopyViaTerminal(text)).await;
                return;
            }
        };
        let _ = tx.send(AppEvent::Status(message)).await;
    });
}

#[cfg(not(feature = "clipboard"))]
fn dispatch_copy(text: String, tx: mpsc::Sender<AppEvent>) {
    send_status(&tx, copy_via_terminal_status(&text));
}

#[cfg(feature = "clipboard")]
fn copy_to_desktop_clipboard(text: &str) -> Result<()> {
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_text(text.to_string()))
        .map_err(Into::into)
}

fn send_status(tx: &mpsc::Sender<AppEvent>, message: String) {
    if let Err(e) = tx.try_send(AppEvent::Status(message)) {
        tracing::debug!("dropping clipboard status; event queue is full or closed: {e}");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyMethod {
    #[cfg(feature = "clipboard")]
    System,
    Terminal,
}

impl CopyMethod {
    fn status_message(self, text: &str) -> String {
        #[cfg(not(feature = "clipboard"))]
        let _ = text;
        match self {
            #[cfg(feature = "clipboard")]
            Self::System => format!("copied: {text}"),
            Self::Terminal => format!("copied via terminal: {text}"),
        }
    }
}

#[cfg(any(feature = "clipboard", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClipboardEnv {
    display: Option<String>,
    wayland_display: Option<String>,
}

#[cfg(any(feature = "clipboard", test))]
impl ClipboardEnv {
    #[cfg(feature = "clipboard")]
    fn from_process() -> Self {
        Self {
            display: std::env::var("DISPLAY").ok().filter(|s| !s.is_empty()),
            wayland_display: std::env::var("WAYLAND_DISPLAY")
                .ok()
                .filter(|s| !s.is_empty()),
        }
    }

    fn has_graphical_display(&self) -> bool {
        self.display.is_some() || self.wayland_display.is_some()
    }
}

#[cfg(any(feature = "clipboard", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardPlatform {
    /// arboard needs an X11/Wayland display on Linux and other non-macOS Unix
    /// backends; without one, skip the X11 timeout and use OSC 52.
    #[cfg(any(test, all(unix, not(target_os = "macos"))))]
    X11WaylandUnix,
    /// macOS and Windows use native pasteboards and do not depend on DISPLAY.
    #[cfg(any(test, not(all(unix, not(target_os = "macos")))))]
    NativeDesktop,
}

#[cfg(feature = "clipboard")]
impl ClipboardPlatform {
    #[cfg(all(unix, not(target_os = "macos")))]
    const CURRENT: Self = Self::X11WaylandUnix;
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    const CURRENT: Self = Self::NativeDesktop;
}

#[cfg(any(feature = "clipboard", test))]
fn should_skip_desktop_clipboard(platform: ClipboardPlatform, env: &ClipboardEnv) -> bool {
    match platform {
        #[cfg(any(test, all(unix, not(target_os = "macos"))))]
        ClipboardPlatform::X11WaylandUnix => !env.has_graphical_display(),
        #[cfg(any(test, not(all(unix, not(target_os = "macos")))))]
        ClipboardPlatform::NativeDesktop => false,
    }
}

fn copy_via_terminal(text: &str) -> Result<CopyMethod> {
    write_osc52(text, &mut std::io::stdout())?;
    Ok(CopyMethod::Terminal)
}

fn copy_via_terminal_status(text: &str) -> String {
    match copy_via_terminal(text) {
        Ok(method) => method.status_message(text),
        Err(e) => format!("copy failed: {e}"),
    }
}

fn write_osc52(text: &str, out: &mut impl Write) -> std::io::Result<()> {
    out.write_all(osc52_sequence(text).as_bytes())?;
    out.flush()
}

fn osc52_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", BASE64_STANDARD.encode(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProfileConfig;
    use crate::netbox::search::{ObjectKind, SearchOutcome, SearchResult};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use serde_json::json;
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn app() -> App {
        let profile = ProfileConfig {
            url: "http://localhost".into(),
            ..Default::default()
        };
        let client = NetBoxClient::new(&profile, None).unwrap();
        App::new(
            client,
            "default",
            "test".into(),
            "http://localhost".into(),
            "4.6.0".into(),
            None,
        )
    }

    fn terminal() -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(80, 24)).unwrap()
    }

    fn buffer_text(term: &Terminal<TestBackend>) -> String {
        term.backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    fn result() -> SearchResult {
        SearchResult {
            kind: ObjectKind::Device,
            id: 42,
            display: "edge01".into(),
            subtitle: Some("dc1".into()),
            url: "http://localhost/dcim/devices/42/".into(),
            score: 100,
        }
    }

    fn press(code: KeyCode) -> AppEvent {
        AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn render_gate_initial_draw_happens() {
        let mut app = app();
        let mut term = terminal();
        let mut gate = RenderGate::default();

        assert!(draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
        assert!(buffer_text(&term).contains("profile:"));
    }

    #[test]
    fn render_gate_idle_preview_tick_after_stable_frame_skips_draw() {
        let mut app = app();
        let mut term = terminal();
        let mut gate = RenderGate::default();

        assert!(draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
        let before = buffer_text(&term);
        assert!(app.handle_event(AppEvent::PreviewTick).is_empty());

        assert!(!draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
        assert_eq!(buffer_text(&term), before);
    }

    #[test]
    fn render_gate_spinner_draws_while_loading_and_stops_after_settle() {
        let mut app = app();
        let mut term = terminal();
        let mut gate = RenderGate::default();

        assert!(draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
        app.pending = 1;
        assert!(draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());

        assert!(app.handle_event(AppEvent::PreviewTick).is_empty());
        assert!(
            draw_if_dirty(&mut term, &mut app, &mut gate).unwrap(),
            "spinner frame change redraws"
        );

        app.handle_event(AppEvent::SearchComplete {
            req: 0,
            result: Ok(SearchOutcome {
                results: Vec::new(),
                errors: Vec::new(),
            }),
        });
        assert!(
            draw_if_dirty(&mut term, &mut app, &mut gate).unwrap(),
            "settle redraws to clear spinner"
        );
        assert!(app.handle_event(AppEvent::PreviewTick).is_empty());
        assert!(
            !draw_if_dirty(&mut term, &mut app, &mut gate).unwrap(),
            "idle ticks stop drawing after settle"
        );
    }

    #[test]
    fn render_gate_transient_status_draws_until_it_clears() {
        let mut app = app();
        let mut term = terminal();
        let mut gate = RenderGate::default();

        assert!(draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
        app.handle_event(AppEvent::Status("copied: edge01".into()));
        assert!(draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
        assert!(buffer_text(&term).contains("copied: edge01"));

        // Intermediate TTL ticks only decrement the (unrendered) countdown — the
        // status text is unchanged, so the gate must NOT redraw, yet the status
        // stays on screen (the last drawn frame still holds it).
        for _ in 0..9 {
            assert!(app.handle_event(AppEvent::PreviewTick).is_empty());
            assert!(
                !draw_if_dirty(&mut term, &mut app, &mut gate).unwrap(),
                "intermediate ttl ticks do not redraw"
            );
            assert!(buffer_text(&term).contains("copied: edge01"));
        }

        // The expiry tick clears the status: `self.status` empties (which IS
        // hashed), so this redraws exactly once to wipe the message.
        assert!(app.handle_event(AppEvent::PreviewTick).is_empty());
        assert!(
            draw_if_dirty(&mut term, &mut app, &mut gate).unwrap(),
            "ttl expiry redraws once to clear"
        );
        assert!(!buffer_text(&term).contains("copied: edge01"));

        assert!(app.handle_event(AppEvent::PreviewTick).is_empty());
        assert!(!draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
    }

    #[test]
    fn render_gate_preview_debounce_command_alone_does_not_redraw() {
        let mut app = app();
        let mut term = terminal();
        let mut gate = RenderGate::default();
        app.results = vec![result()];
        app.view = vec![0];
        app.selected = 0;
        app.preview_dirty = true;

        assert!(draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
        let commands = app.handle_event(AppEvent::PreviewTick);
        assert!(matches!(
            commands.as_slice(),
            [AppCommand::LoadPreview {
                kind: ObjectKind::Device,
                id: 42
            }]
        ));
        assert!(!draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
    }

    #[test]
    fn render_gate_async_result_events_redraw() {
        let mut app = app();
        let mut term = terminal();
        let mut gate = RenderGate::default();

        assert!(draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
        app.handle_event(AppEvent::SearchComplete {
            req: 0,
            result: Ok(SearchOutcome {
                results: vec![result()],
                errors: Vec::new(),
            }),
        });

        assert!(draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
        assert!(buffer_text(&term).contains("edge01"));
    }

    #[test]
    fn render_gate_keypress_visible_change_redraws() {
        let mut app = app();
        let mut term = terminal();
        let mut gate = RenderGate::default();

        assert!(draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
        assert!(app.handle_event(press(KeyCode::Tab)).is_empty());

        assert!(draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
    }

    #[test]
    fn render_gate_terminal_resize_redraws() {
        let mut app = app();
        let mut term = terminal();
        let mut gate = RenderGate::default();

        assert!(draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
        assert!(!draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());

        term.backend_mut().resize(100, 30);
        assert!(draw_if_dirty(&mut term, &mut app, &mut gate).unwrap());
    }

    #[test]
    fn clipboard_env_detects_graphical_display() {
        assert!(
            !ClipboardEnv {
                display: None,
                wayland_display: None,
            }
            .has_graphical_display()
        );
        assert!(
            ClipboardEnv {
                display: Some(":0".into()),
                wayland_display: None,
            }
            .has_graphical_display()
        );
        assert!(
            ClipboardEnv {
                display: None,
                wayland_display: Some("wayland-0".into()),
            }
            .has_graphical_display()
        );
    }

    #[test]
    fn desktop_clipboard_skip_is_linux_x11_wayland_only() {
        let headless = ClipboardEnv {
            display: None,
            wayland_display: None,
        };
        let x11 = ClipboardEnv {
            display: Some(":0".into()),
            wayland_display: None,
        };
        let wayland = ClipboardEnv {
            display: None,
            wayland_display: Some("wayland-0".into()),
        };

        assert!(should_skip_desktop_clipboard(
            ClipboardPlatform::X11WaylandUnix,
            &headless
        ));
        assert!(!should_skip_desktop_clipboard(
            ClipboardPlatform::X11WaylandUnix,
            &x11
        ));
        assert!(!should_skip_desktop_clipboard(
            ClipboardPlatform::X11WaylandUnix,
            &wayland
        ));
        assert!(!should_skip_desktop_clipboard(
            ClipboardPlatform::NativeDesktop,
            &headless
        ));
    }

    #[test]
    fn osc52_sequence_encodes_clipboard_payload() {
        assert_eq!(osc52_sequence("edge01"), "\u{1b}]52;c;ZWRnZTAx\u{7}");
        assert_eq!(
            osc52_sequence("device edge99"),
            "\u{1b}]52;c;ZGV2aWNlIGVkZ2U5OQ==\u{7}"
        );
    }

    #[test]
    fn write_osc52_writes_and_flushes_the_sequence() {
        let mut out = Vec::new();
        write_osc52("edge01", &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), osc52_sequence("edge01"));
    }

    #[test]
    fn terminal_copy_status_is_honest() {
        assert_eq!(
            CopyMethod::Terminal.status_message("edge01"),
            "copied via terminal: edge01"
        );
    }

    /// A superseded preview fetch is aborted, not run to completion. `Asn` is a
    /// single GET with no fan-out, so the mock setup is deterministic: id=1 hangs
    /// (a slow NetBox), id=2 answers fast. Aborting the id=1 task must resolve
    /// its `JoinHandle` (with a cancelled error) instead of hanging, and its
    /// `PreviewLoaded` event must never be delivered — the cursor moved on.
    #[tokio::test]
    async fn preview_supersede_aborts_in_flight_fetch() {
        let server = MockServer::start().await;
        // id=1: a response that never lands within the test (simulates a slow
        // NetBox); the abort must drop the awaiting task before it can send.
        Mock::given(method("GET"))
            .and(path("/api/ipam/asns/1/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": 1, "url": "u", "asn": 65001}))
                    .set_delay(Duration::from_secs(30)),
            )
            .mount(&server)
            .await;
        // id=2: a fast valid Asn so the superseding preview completes.
        Mock::given(method("GET"))
            .and(path("/api/ipam/asns/2/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": 2, "url": "u", "asn": 65002})),
            )
            .mount(&server)
            .await;

        let profile = ProfileConfig {
            url: server.uri(),
            ..Default::default()
        };
        let client = NetBoxClient::new(&profile, None).unwrap();
        let cache = Cache::disabled();
        let (tx, mut rx) = mpsc::channel::<AppEvent>(8);

        // Start the superseded preview and let it reach the network await.
        let h1 = spawn_preview(
            ObjectKind::Asn,
            1,
            client.clone(),
            cache.clone(),
            tx.clone(),
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
        h1.abort();

        // The aborted handle resolves promptly with a cancelled error rather than
        // hanging on the 30s response — proving the in-flight fetch was cancelled.
        let join = tokio::time::timeout(Duration::from_secs(1), h1)
            .await
            .expect("aborted task resolves, it does not hang on the slow response");
        assert!(
            join.is_err(),
            "expected a cancelled JoinError, got {join:?}"
        );

        // The superseding preview completes normally.
        let _h2 = spawn_preview(ObjectKind::Asn, 2, client, cache, tx);
        let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("the superseding preview should deliver its event")
            .expect("channel open");
        assert!(
            matches!(
                ev,
                AppEvent::PreviewLoaded {
                    kind: ObjectKind::Asn,
                    id: 2,
                    ..
                }
            ),
            "expected PreviewLoaded for id=2"
        );

        // The aborted preview's event must never arrive — it was cancelled before
        // `tx.send`. Bounded wait so a regression fails fast rather than hanging.
        // Drain any further events for a short window: the aborted preview must
        // not deliver, and the superseding one already did. A regression fails
        // fast rather than hanging.
        let mut extra = 0;
        let drain = tokio::time::timeout(Duration::from_millis(300), async {
            while rx.recv().await.is_some() {
                extra += 1;
            }
        })
        .await;
        let _ = drain;
        assert_eq!(
            extra, 0,
            "the aborted preview must not deliver its event (got {extra} extra)"
        );
    }
}
