//! TUI application state and (pure) input handling.
//!
//! `handle_event`/`handle_key` mutate state and return the commands to run —
//! they perform no I/O, so they're unit-testable without a terminal. Network
//! work happens in spawned tasks (see `tui::app`), never in the render loop.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

use crate::config::ProfileConfig;
use crate::domain::detail::DetailView;
use crate::netbox::client::NetBoxClient;
use crate::netbox::search::{ObjectKind, SearchOutcome, SearchResult};
use crate::tui::cheese::{Spinner, TextInput};
use crate::tui::palette::{self, PaletteCommand};
use crate::tui::theme::{Severity, Theme};

/// Which screen is in the body area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Detail,
}

/// Input mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Search,
    Command,
}

/// Which pane on the split home screen has focus. Movement keys route to it:
/// the list moves the selection, the preview scrolls its body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Preview,
}

/// Events delivered to the event loop.
pub enum AppEvent {
    Key(KeyEvent),
    Resize(u16, u16),
    Tick,
    /// A fast, always-on debounce tick: flushes a settled selection into a
    /// preview load (see [`App::on_preview_tick`]). Distinct from `Tick`, which
    /// is the optional auto-refresh and may not be running.
    PreviewTick,
    /// A full-search result, tagged with the search channel's request id it was
    /// issued for so a stale (older-id) response can be dropped on arrival.
    SearchComplete {
        req: RequestId,
        result: anyhow::Result<SearchOutcome>,
    },
    /// A full-detail load (from Enter / a palette lookup), tagged with the detail
    /// channel's request id so a stale (older-id) response can be dropped.
    DetailLoaded {
        req: RequestId,
        result: anyhow::Result<DetailView>,
    },
    /// A preview-pane detail load, tagged with the (kind, id) it was issued for
    /// so a stale response (the selection moved on) can be dropped.
    PreviewLoaded {
        kind: ObjectKind,
        id: u64,
        result: anyhow::Result<DetailView>,
    },
    /// A profile switch finished re-probing the new instance: on success carries
    /// the rebuilt client and the new instance's `/api/status/` version; on
    /// failure carries the error to surface. Tagged with the profile `name` it
    /// was issued for so a switch superseded by a newer one is dropped on arrival
    /// (the latest-switch-wins guard, mirroring the search/detail request-id
    /// guard — see [`App::is_current_profile`]).
    ProfileSwitched {
        name: String,
        result: anyhow::Result<(NetBoxClient, String)>,
    },
    Status(String),
}

/// A recently-opened object, for quick reopening from the home screen.
#[derive(Debug, Clone)]
pub struct RecentItem {
    pub kind: ObjectKind,
    pub id: u64,
    pub title: String,
}

/// A configured NetBox profile the session can switch to without restarting: its
/// config-file name and the [`ProfileConfig`] used to rebuild the client. Held in
/// [`App::profiles`] in config order; cycling/selecting picks one and triggers an
/// async reconnect + re-probe (see [`AppCommand::SwitchProfile`]).
#[derive(Debug, Clone)]
pub struct ProfileEntry {
    pub name: String,
    pub config: ProfileConfig,
}

/// Most-recent-first cap for the recents list.
const RECENT_CAP: usize = 20;

/// Pre-render fallback for the results-list PgUp/PgDn step, used before the first
/// render has stashed a live viewport height (see [`App::list_page`]). Once a
/// frame has been drawn the list pages by ~one screenful instead, matching the
/// detail/preview panes.
const PAGE_JUMP_FALLBACK: usize = 10;

/// A monotonic per-channel request id, stamped on a spawned full-search or
/// full-detail command and echoed back on its result event. The pure handler
/// drops a result whose id is older than the latest spawned for that channel, so
/// a slow earlier request can't clobber the UI after a newer one (mirrors the
/// preview path's `(kind, id)` stale-response suppression, scoped per channel).
pub type RequestId = u64;

/// Side-effecting work the loop should spawn off the render thread.
#[derive(Debug, Clone)]
pub enum AppCommand {
    /// A full search. Carries the search channel's request id (stamped at
    /// dispatch) so a stale `SearchComplete` can be dropped on arrival.
    Search {
        query: String,
        req: RequestId,
    },
    LoadDetail {
        kind: ObjectKind,
        id: u64,
        /// The detail channel's request id, for stale-`DetailLoaded` suppression.
        req: RequestId,
    },
    /// Load the highlighted result's detail into the live preview pane. Carries
    /// its (kind, id) so the response can be matched against the selection it
    /// was issued for (stale ones are dropped on arrival).
    LoadPreview {
        kind: ObjectKind,
        id: u64,
    },
    LoadByRef {
        kind: ObjectKind,
        value: String,
        /// The detail channel's request id (shared with `LoadDetail`, since both
        /// resolve back through `DetailLoaded`), for stale-response suppression.
        req: RequestId,
    },
    OpenBrowser(String),
    Copy(String),
    /// Switch the live session to the named profile: rebuild the NetBox client
    /// from its [`ProfileConfig`] and re-probe `/api/status/`, off the render
    /// thread. The result returns as [`AppEvent::ProfileSwitched`]; `name` tags
    /// it so a superseded switch is dropped on arrival.
    SwitchProfile {
        name: String,
        config: ProfileConfig,
    },
}

impl AppCommand {
    /// True when this command kicks off a network fetch whose result returns as
    /// an `AppEvent` (Search/LoadDetail/LoadPreview/LoadByRef/SwitchProfile).
    /// These bump the in-flight counter so the footer spinner runs until the
    /// matching result event lands. `OpenBrowser`/`Copy` are fire-and-forget side
    /// effects (their async `Status` push isn't a tracked fetch), so they don't
    /// count as loading.
    fn is_fetch(&self) -> bool {
        matches!(
            self,
            AppCommand::Search { .. }
                | AppCommand::LoadDetail { .. }
                | AppCommand::LoadPreview { .. }
                | AppCommand::LoadByRef { .. }
                | AppCommand::SwitchProfile { .. }
        )
    }
}

/// The whole TUI application state.
pub struct App {
    pub client: NetBoxClient,
    pub theme: Theme,
    pub theme_index: usize,
    pub initial_theme: String,
    pub config_path: Option<PathBuf>,
    pub profile_name: String,
    pub base_url: String,
    pub netbox_version: String,
    /// All configured profiles, in config order, that the session can switch
    /// between without restarting. Empty/one-element ⇒ cycling is a graceful
    /// no-op. Populated at launch via [`App::set_profiles`].
    pub profiles: Vec<ProfileEntry>,
    /// Index into [`Self::profiles`] of the active profile. Cycling advances /
    /// wraps it; the palette `profile <name>` verb jumps to a named one.
    pub profile_index: usize,
    /// Monotonic profile-switch generation. Each dispatched [`AppCommand::SwitchProfile`]
    /// stamps the target name as the latest; a [`AppEvent::ProfileSwitched`] whose
    /// name no longer matches the active profile is from a superseded switch and
    /// is dropped (latest-switch-wins; see [`Self::is_current_profile`]).
    pub pending_profile: Option<String>,

    pub mode: Mode,
    pub screen: Screen,
    /// Whether the help overlay is open. Orthogonal to `screen`: help floats over
    /// the live Home/Detail view as a centered modal (see `tui::ui::render_help`),
    /// so opening or closing it never disturbs the underlying screen or history.
    /// `?`/`F1` toggle it; while open, any key (or `Esc`) closes it.
    pub help_open: bool,
    /// Which home pane has focus: the results list or the preview. Movement keys
    /// route to it. `Tab`/`Shift+Tab` toggle it; only meaningful on Home.
    pub focus: Focus,
    pub history: Vec<Screen>,
    pub status: String,
    /// Severity of the current `status` message, so the footer can color it via
    /// [`Theme::message_style`]. Set in lockstep with `status` (see
    /// [`App::set_status`]); resets to `Info` whenever the status is cleared.
    pub status_severity: Severity,

    /// The `/` search line editor. Text entry, backspace/delete, cursor
    /// movement and the visible cursor are delegated to the cheese-backed
    /// [`TextInput`] wrapper (see `tui::cheese`); the submit/cancel flow stays
    /// here. Read its text with [`TextInput::value`].
    pub search_input: TextInput,
    /// The `:` command-palette line editor — same cheese-backed wrapper.
    pub command_input: TextInput,
    pub last_query: Option<String>,

    pub results: Vec<SearchResult>,
    /// Indices into `results` in display order (fuzzy-filtered while searching).
    pub view: Vec<usize>,
    /// Index of the cursor into the active home list (results or recents). The
    /// pure movement keys (j/k/g/G/PgUp/PgDn) own this; `table_state` is synced
    /// from it at render time so ratatui scrolls the offset to keep it visible.
    pub selected: usize,
    /// Stateful-table selection/offset, driven by `selected` each frame so the
    /// selected row is always on screen no matter how tall the table gets.
    /// `TableState` keeps the selected row visible exactly like `ListState` did,
    /// so the selection-stays-visible / page-jump viewport behaviour is unchanged.
    pub table_state: TableState,
    /// Last-known results-list inner height (visible rows), stashed during render
    /// so the pure PgUp/PgDn handler can page by one screenful. Mirrors the
    /// detail/preview live-viewport pattern. 0 until the first render, where the
    /// pre-render fallback ([`PAGE_JUMP_FALLBACK`]) applies instead.
    pub list_viewport: u16,
    /// On the next `SearchComplete`, try to re-select this (kind, id) — used to
    /// keep the cursor stable across an auto-refresh.
    pub pending_reselect: Option<(ObjectKind, u64)>,
    pub recent: Vec<RecentItem>,
    pub detail: Option<DetailView>,
    /// Active detail tab: 0 = summary, n>0 = `detail.tabs[n-1]`.
    pub detail_tab: usize,
    /// Vertical scroll offset (in lines) of the detail body. The pure scroll
    /// keys (j/k/g/G/PgUp/PgDn) own this; the render path feeds it straight to
    /// `Paragraph::scroll`. Reset to 0 on navigation and on a tab switch.
    pub detail_scroll: u16,
    /// Last-known detail pane inner height (content rows), stashed during render
    /// so the pure handler can clamp scrolling at the bottom without doing I/O.
    /// Mirrors the home list's live-viewport pattern. 0 until the first render.
    pub detail_viewport: u16,

    /// The (kind, id) whose full detail is currently loaded in the preview pane,
    /// if any. Compared against the live selection to detect staleness.
    pub preview_for: Option<(ObjectKind, u64)>,
    /// The detail loaded into the preview pane (None until a load returns, or
    /// while the selection is empty). Separate from `detail` so opening the full
    /// detail screen (Enter) never collides with the live peek.
    pub preview: Option<DetailView>,
    /// Set whenever the selection changes; cleared once a `LoadPreview` for the
    /// settled selection has been dispatched. The debounce: a preview load fires
    /// on the next `PreviewTick`, not on every keystroke — so a burst of j/k
    /// scrolling issues at most one fetch when the cursor finally settles.
    pub preview_dirty: bool,
    /// Vertical scroll offset of the preview body, owned by the pure scroll keys
    /// while the Preview pane is focused. Reset when the previewed object changes.
    pub preview_scroll: u16,
    /// Last-known preview pane inner height (content rows), stashed at render so
    /// the pure scroll handler can clamp at the bottom. 0 until the first render.
    pub preview_viewport: u16,

    /// Monotonic request-id source for the full-search and full-detail channels.
    /// Each spawned `Search`/`LoadDetail`/`LoadByRef` is stamped with the next id
    /// here (see [`App::stamp_request_ids`]); the latest stamped id per channel is
    /// kept in [`Self::search_gen`]/[`Self::detail_gen`] so a stale (older-id)
    /// result event can be dropped, the same way preview loads drop a stale
    /// `(kind, id)`.
    pub request_seq: RequestId,
    /// The latest request id stamped on a spawned full search. A `SearchComplete`
    /// older than this is from a superseded request and is dropped on arrival.
    pub search_gen: RequestId,
    /// The latest request id stamped on a spawned full-detail load (`LoadDetail`
    /// or `LoadByRef`). A `DetailLoaded` older than this is dropped on arrival.
    pub detail_gen: RequestId,

    /// Count of fetching async commands currently in flight (Search / LoadDetail
    /// / LoadPreview / LoadByRef / refresh). Bumped when such a command is
    /// dispatched, decremented when its matching result event arrives; clamped at
    /// 0 so a stray/duplicate result can't drive it negative. The footer shows
    /// the spinner iff this is non-zero (see [`App::loading`]), so concurrent
    /// fetches all have to resolve before the UI reads as idle.
    pub pending: u32,
    /// The footer loading spinner. Advances one frame per tick *only while*
    /// [`App::loading`] (so it's still at rest), and resets to frame 0 when the
    /// last request settles. Confined behind the cheese adapter.
    pub spinner: Spinner,

    pub should_quit: bool,
}

impl App {
    /// Construct a fresh app on the home screen.
    pub fn new(
        client: NetBoxClient,
        theme_name: &str,
        profile_name: String,
        base_url: String,
        netbox_version: String,
        config_path: Option<PathBuf>,
    ) -> Self {
        Self {
            client,
            theme: Theme::by_name(theme_name),
            theme_index: Theme::index_of(theme_name),
            initial_theme: Theme::by_name(theme_name).name().to_string(),
            config_path,
            profile_name,
            base_url,
            netbox_version,
            profiles: Vec::new(),
            profile_index: 0,
            pending_profile: None,
            mode: Mode::Normal,
            screen: Screen::Home,
            help_open: false,
            focus: Focus::List,
            history: Vec::new(),
            status: String::new(),
            status_severity: Severity::Info,
            search_input: TextInput::new("search NetBox…"),
            command_input: TextInput::new("command (e.g. device edge01)"),
            last_query: None,
            results: Vec::new(),
            view: Vec::new(),
            selected: 0,
            table_state: TableState::default(),
            list_viewport: 0,
            pending_reselect: None,
            recent: Vec::new(),
            detail: None,
            detail_tab: 0,
            detail_scroll: 0,
            detail_viewport: 0,
            preview_for: None,
            preview: None,
            preview_dirty: false,
            preview_scroll: 0,
            preview_viewport: 0,
            request_seq: 0,
            search_gen: 0,
            detail_gen: 0,
            pending: 0,
            spinner: Spinner::new(),
            should_quit: false,
        }
    }

    /// Switch the app into the monochrome `NO_COLOR` mode: the theme renders with
    /// no color at all (see [`Theme::no_color`]). Wired at TUI startup when
    /// `NO_COLOR` is set in the environment. `initial_theme` is pinned to the
    /// no-color sentinel too, so the exit-time theme-persistence guard
    /// (`theme.name() != initial_theme`) stays a no-op and we never write
    /// `"no_color"` back into the user's config.
    pub fn set_no_color(&mut self) {
        self.theme = Theme::no_color();
        self.theme_index = 0;
        self.initial_theme = self.theme.name().to_string();
    }

    /// Attach the configured profiles the session can switch between, and point
    /// [`Self::profile_index`] at whichever matches the active [`Self::profile_name`]
    /// (falling back to 0). Called once at launch (see `tui::run_tui`). With zero
    /// or one profile, the switch hotkey is a graceful no-op (see
    /// [`Self::cycle_profile`]).
    pub fn set_profiles(&mut self, profiles: Vec<ProfileEntry>) {
        self.profile_index = profiles
            .iter()
            .position(|p| p.name == self.profile_name)
            .unwrap_or(0);
        self.profiles = profiles;
    }

    /// Apply an event, returning any commands to dispatch. The commands handed
    /// back are accounted into the in-flight counter (each fetch bumps it) so the
    /// footer spinner runs until the matching result event lands.
    pub fn handle_event(&mut self, event: AppEvent) -> Vec<AppCommand> {
        let mut commands = self.dispatch_event(event);
        self.stamp_request_ids(&mut commands);
        self.track_dispatched(&commands);
        commands
    }

    /// Stamp the about-to-be-spawned full-search / full-detail commands with a
    /// fresh per-channel request id, recording the newest in
    /// [`Self::search_gen`]/[`Self::detail_gen`]. Handlers build these commands
    /// with a placeholder id (0); the single id source lives here so every
    /// dispatch site is tagged consistently and the latest-wins guard
    /// ([`Self::is_current_search`]/[`Self::is_current_detail`]) has an
    /// authoritative high-water mark. Preview loads carry their own `(kind, id)`
    /// tag and are untouched. Order is preserved within the (rare) multi-fetch
    /// batch so the last of a kind sets the high-water mark.
    fn stamp_request_ids(&mut self, commands: &mut [AppCommand]) {
        for command in commands.iter_mut() {
            match command {
                AppCommand::Search { req, .. } => {
                    self.request_seq += 1;
                    *req = self.request_seq;
                    self.search_gen = self.request_seq;
                }
                AppCommand::LoadDetail { req, .. } | AppCommand::LoadByRef { req, .. } => {
                    self.request_seq += 1;
                    *req = self.request_seq;
                    self.detail_gen = self.request_seq;
                }
                // Preview loads carry (kind,id) and profile switches carry their
                // target name; neither rides the per-channel request-id guard.
                AppCommand::LoadPreview { .. }
                | AppCommand::OpenBrowser(_)
                | AppCommand::Copy(_)
                | AppCommand::SwitchProfile { .. } => {}
            }
        }
    }

    /// True when `req` is the newest full-search request spawned. A
    /// `SearchComplete` for an older id is from a superseded request and is
    /// dropped so a slow earlier search can't overwrite newer results.
    fn is_current_search(&self, req: RequestId) -> bool {
        req >= self.search_gen
    }

    /// True when `req` is the newest full-detail request spawned (the
    /// `LoadDetail`/`LoadByRef` channel). An older `DetailLoaded` is dropped.
    fn is_current_detail(&self, req: RequestId) -> bool {
        req >= self.detail_gen
    }

    /// The event→commands transition itself. Result events that settle an
    /// in-flight fetch decrement the counter here; the commands they (don't)
    /// return are then accounted by [`Self::handle_event`].
    fn dispatch_event(&mut self, event: AppEvent) -> Vec<AppCommand> {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::Resize(_, _) => Vec::new(),
            AppEvent::Tick => self.on_tick(),
            AppEvent::PreviewTick => {
                // Reuse the always-on preview tick to drive the spinner: advance
                // a frame only while something is in flight, so it's still at
                // rest (no busy-spin) when idle. The preview debounce flush is
                // independent and runs regardless.
                if self.loading() {
                    self.spinner.tick();
                }
                self.on_preview_tick()
            }
            AppEvent::SearchComplete { req, result } => {
                // A search result settles its in-flight fetch (clean or error),
                // counted down even when dropped as stale below — otherwise the
                // spinner would hang on a superseded request (mirrors the preview
                // path). Then drop a stale (older-id) response so a slow earlier
                // search can't clobber newer results.
                self.end_request();
                if !self.is_current_search(req) {
                    return Vec::new();
                }
                match result {
                    Ok(outcome) => {
                        if outcome.errors.is_empty() {
                            // A clean, complete result set is a confirmation.
                            self.set_status(
                                format!("{} result(s)", outcome.results.len()),
                                Severity::Success,
                            );
                        } else {
                            // Some endpoints failed: a degraded / partial result.
                            self.set_status(
                                format!(
                                    "{} result(s) (partial: {} endpoint(s) failed)",
                                    outcome.results.len(),
                                    outcome.errors.len()
                                ),
                                Severity::Warning,
                            );
                        }
                        self.results = outcome.results;
                        self.view = (0..self.results.len()).collect();
                        self.selected = self
                            .pending_reselect
                            .take()
                            .and_then(|(kind, id)| {
                                self.view.iter().position(|&i| {
                                    self.results[i].kind == kind && self.results[i].id == id
                                })
                            })
                            .unwrap_or(0);
                        // The highlighted result may have changed; let the
                        // debounce decide whether to (re)load the preview.
                        self.mark_preview_dirty();
                    }
                    Err(e) => self.set_status(format!("error: {e:#}"), Severity::Error),
                }
                Vec::new()
            }
            AppEvent::DetailLoaded { req, result } => {
                // The full-detail load (from Enter / a palette lookup) settled —
                // counted down even if dropped as stale, so the spinner can't hang
                // on a superseded request. A stale (older-id) response is dropped
                // so a slow earlier load can't navigate over a newer one.
                self.end_request();
                if !self.is_current_detail(req) {
                    return Vec::new();
                }
                match result {
                    Ok(view) => {
                        self.record_recent(view.kind, view.id, view.title.clone());
                        self.navigate_to(Screen::Detail);
                        self.detail = Some(view);
                        self.detail_tab = 0;
                        self.detail_scroll = 0;
                        self.clear_status();
                    }
                    Err(e) => self.set_status(format!("error: {e:#}"), Severity::Error),
                }
                Vec::new()
            }
            AppEvent::PreviewLoaded { kind, id, result } => {
                // The preview load settled — count it down even if its body is
                // dropped as stale below (the request itself is no longer in
                // flight, so the spinner must not hang on a moved cursor).
                self.end_request();
                // Stale-response suppression: only adopt this load if it still
                // matches the highlighted result. A response for a selection the
                // user has already scrolled past is dropped.
                if self.preview_selection() == Some((kind, id)) {
                    match result {
                        Ok(view) => {
                            self.preview = Some(view);
                            self.preview_for = Some((kind, id));
                            self.preview_scroll = 0;
                        }
                        // Keep the lightweight peek; surface the failure quietly.
                        Err(e) => self.set_status(format!("preview error: {e:#}"), Severity::Error),
                    }
                }
                Vec::new()
            }
            AppEvent::ProfileSwitched { name, result } => {
                // The reconnect+re-probe settled — count it down even when dropped
                // as stale below, so the spinner can't hang on a superseded switch.
                self.end_request();
                // Latest-switch-wins: a result for a profile the user has already
                // cycled away from is dropped (its client/version is moot now).
                if !self.is_current_profile(&name) {
                    return Vec::new();
                }
                self.pending_profile = None;
                match result {
                    Ok((client, version)) => {
                        // Atomically adopt the new instance: swap in its client and
                        // flip the header/`base_url`/index/version to the target
                        // profile together, then drop the old instance's data and
                        // bump the request generations so any straggler response
                        // from the previous instance lands stale. This is the ONLY
                        // place the header moves, so it always matches `client`.
                        self.client = client;
                        self.netbox_version = version;
                        if let Some(idx) = self.profiles.iter().position(|p| p.name == name) {
                            self.profile_index = idx;
                            self.base_url.clone_from(&self.profiles[idx].config.url);
                        }
                        self.profile_name.clone_from(&name);
                        self.clear_for_profile_switch();
                        self.set_status(
                            format!("switched to '{name}' (NetBox v{})", self.netbox_version),
                            Severity::Success,
                        );
                    }
                    // The new instance was unreachable/incompatible: the old client
                    // is still valid, so a failed switch is a no-op + error toast.
                    // The header was never flipped, so it still matches the connected
                    // instance — no phantom. The UI stays fully usable on the old one.
                    Err(e) => {
                        self.set_status(format!("profile '{name}' error: {e:#}"), Severity::Error);
                    }
                }
                Vec::new()
            }
            AppEvent::Status(message) => {
                // An async status push (e.g. "copied …"/"opened …"): classify it
                // so confirmations and failures still get the right color.
                let severity = classify_status(&message);
                self.set_status(message, severity);
                Vec::new()
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        if key.kind != KeyEventKind::Press {
            return Vec::new();
        }
        match self.mode {
            Mode::Normal => self.handle_normal_key(key),
            Mode::Search => self.handle_search_key(key),
            Mode::Command => self.handle_command_key(key),
        }
    }

    /// True when at least one fetching request is in flight, so the footer should
    /// show the loading spinner. Pure: just reads the [`Self::pending`] counter.
    pub fn loading(&self) -> bool {
        self.pending > 0
    }

    /// Mark one more fetching request as dispatched.
    fn begin_request(&mut self) {
        self.pending = self.pending.saturating_add(1);
    }

    /// Mark one in-flight fetching request as settled, clamping at 0 so a stray
    /// or duplicate result event can't drive the counter negative. When the last
    /// request settles the spinner is reset so the next one starts clean.
    fn end_request(&mut self) {
        self.pending = self.pending.saturating_sub(1);
        if self.pending == 0 {
            self.spinner.reset();
        }
    }

    /// Account for the fetching commands a handler is about to dispatch: each one
    /// bumps [`Self::pending`] so the spinner stays up until its result lands.
    /// Side-effect-free commands (open/copy) don't fetch and don't count.
    fn track_dispatched(&mut self, commands: &[AppCommand]) {
        for command in commands {
            if command.is_fetch() {
                self.begin_request();
            }
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // The help overlay is a modal: while it's open ANY key closes it and is
        // consumed here, so it never also acts on the underlying screen (ttl's
        // "press any key to close"). Ctrl+C still quits, mirroring the rest of
        // the app's hard-exit. Closing leaves `screen`/history untouched.
        if self.help_open {
            if let KeyCode::Char('c') = key.code
                && ctrl
            {
                self.should_quit = true;
            } else {
                self.help_open = false;
            }
            return Vec::new();
        }
        match key.code {
            KeyCode::Char('c') if ctrl => self.should_quit = true,
            KeyCode::Char('q') => {
                if self.screen == Screen::Home {
                    self.should_quit = true;
                } else {
                    self.go_back();
                }
            }
            KeyCode::Char('?') | KeyCode::F(1) => self.help_open = true,
            KeyCode::Esc | KeyCode::Char('b') => self.go_back(),
            KeyCode::Char('/') => {
                self.mode = Mode::Search;
                self.search_input.reset();
                self.refilter();
            }
            KeyCode::Char(':') => {
                self.mode = Mode::Command;
                self.command_input.reset();
            }
            KeyCode::Char('t') => self.cycle_theme(),
            KeyCode::Char('r') => return self.refresh_current_view(),
            // Profile switcher. `Tab` is taken on Home (pane focus), so the
            // configured-profile cycle rides `P` forward / `Ctrl+P` backward (a
            // free, mnemonic key); the palette `profile <name>` verb jumps to a
            // named one. Reconnects + re-probes the instance off the render thread.
            KeyCode::Char('p') if ctrl => return self.cycle_profile(false),
            KeyCode::Char('P') => return self.cycle_profile(true),
            // Tab / Shift+Tab cycle focus between the home split's panes.
            KeyCode::Tab if self.screen == Screen::Home => self.toggle_focus(),
            KeyCode::BackTab if self.screen == Screen::Home => self.toggle_focus(),
            // Movement keys route to whatever owns scrolling/selection right now:
            // the detail screen scrolls its body; on Home the focused pane decides
            // (List → move the selection, Preview → scroll the preview body).
            KeyCode::Char('j') | KeyCode::Down if self.scrolls_body() => self.body_scroll_down(1),
            KeyCode::Char('k') | KeyCode::Up if self.scrolls_body() => self.body_scroll_up(1),
            KeyCode::Char('g') | KeyCode::Home if self.scrolls_body() => self.body_scroll_top(),
            KeyCode::Char('G') | KeyCode::End if self.scrolls_body() => self.body_scroll_bottom(),
            KeyCode::PageDown if self.scrolls_body() => {
                let page = self.body_page();
                self.body_scroll_down(page);
            }
            KeyCode::PageUp if self.scrolls_body() => {
                let page = self.body_page();
                self.body_scroll_up(page);
            }
            KeyCode::Char('j') | KeyCode::Down => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.select_prev(),
            KeyCode::Char('g') | KeyCode::Home => self.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.select_last(),
            KeyCode::PageDown => self.select_page_down(),
            KeyCode::PageUp => self.select_page_up(),
            KeyCode::Enter => {
                if self.screen == Screen::Home
                    && let Some((kind, id)) = self.home_target()
                {
                    // Don't open a detail against the old client mid-switch.
                    if self.fence_during_switch() {
                        return Vec::new();
                    }
                    self.set_status("loading…", Severity::Info);
                    return vec![AppCommand::LoadDetail { kind, id, req: 0 }];
                }
            }
            KeyCode::Char('o') => {
                if let Some(r) = self.selected_result() {
                    return vec![AppCommand::OpenBrowser(r.url.clone())];
                }
            }
            KeyCode::Char('y') => {
                if let Some(r) = self.selected_result() {
                    return vec![AppCommand::Copy(r.display.clone())];
                }
            }
            KeyCode::Char(c @ ('i' | 'p' | 'c' | 'v' | 's')) if !ctrl => self.select_detail_tab(c),
            _ => {}
        }
        Vec::new()
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        match key.code {
            // Esc cancels the search; Enter submits it. Everything else is text
            // editing, delegated to the cheese-backed input (chars, backspace,
            // delete, cursor movement, Ctrl+U clear, Ctrl+W word-delete).
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let query = self.search_input.value().trim().to_string();
                self.mode = Mode::Normal;
                if !query.is_empty() {
                    // Don't search the old client mid-switch.
                    if self.fence_during_switch() {
                        return Vec::new();
                    }
                    self.last_query = Some(query.clone());
                    self.set_status(format!("searching {query}…"), Severity::Info);
                    return vec![AppCommand::Search { query, req: 0 }];
                }
            }
            _ => {
                // An edit refilters the live view; a non-editing key is ignored.
                if self.search_input.handle_key(key) {
                    self.refilter();
                }
            }
        }
        Vec::new()
    }

    fn handle_command_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        match key.code {
            // Esc cancels the palette; Enter executes it. Everything else is text
            // editing, delegated to the cheese-backed input.
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let input = self.command_input.value().trim().to_string();
                self.mode = Mode::Normal;
                if !input.is_empty() {
                    match palette::parse(&input) {
                        Ok(cmd) => return self.apply_palette(cmd),
                        Err(e) => self.set_status(e, Severity::Error),
                    }
                }
            }
            _ => {
                self.command_input.handle_key(key);
            }
        }
        Vec::new()
    }

    /// Map a parsed palette command onto state changes / commands.
    fn apply_palette(&mut self, cmd: PaletteCommand) -> Vec<AppCommand> {
        // Fence palette fetches mid-switch so they don't hit the old client; the
        // theme/open/copy/profile verbs below are unaffected (no fetch, or the
        // switch itself).
        match cmd {
            PaletteCommand::Lookup { kind, value } => {
                if self.fence_during_switch() {
                    return Vec::new();
                }
                self.set_status(format!("loading {value}…"), Severity::Info);
                vec![AppCommand::LoadByRef {
                    kind,
                    value,
                    req: 0,
                }]
            }
            PaletteCommand::Search(query) => {
                if self.fence_during_switch() {
                    return Vec::new();
                }
                self.last_query = Some(query.clone());
                self.set_status(format!("searching {query}…"), Severity::Info);
                vec![AppCommand::Search { query, req: 0 }]
            }
            PaletteCommand::Open => match self.selected_result() {
                Some(r) => vec![AppCommand::OpenBrowser(r.url.clone())],
                None => {
                    self.set_status("no selection", Severity::Warning);
                    Vec::new()
                }
            },
            PaletteCommand::Copy => match self.selected_result() {
                Some(r) => vec![AppCommand::Copy(r.display.clone())],
                None => {
                    self.set_status("no selection", Severity::Warning);
                    Vec::new()
                }
            },
            PaletteCommand::Theme(name) => {
                self.set_theme_by_name(&name);
                Vec::new()
            }
            PaletteCommand::Profile(name) => self.select_profile(&name),
            PaletteCommand::Refresh => {
                if self.fence_during_switch() {
                    return Vec::new();
                }
                match self.last_query.clone() {
                    Some(query) => {
                        self.set_status(format!("refreshing {query}…"), Severity::Info);
                        vec![AppCommand::Search { query, req: 0 }]
                    }
                    None => {
                        self.set_status("nothing to refresh", Severity::Warning);
                        Vec::new()
                    }
                }
            }
        }
    }

    /// Push the current screen onto the history stack and switch to `screen`.
    fn navigate_to(&mut self, screen: Screen) {
        if self.screen != screen {
            self.history.push(self.screen);
            self.screen = screen;
        }
    }

    /// Pop back to the previous screen, or Home if the stack is empty.
    fn go_back(&mut self) {
        self.screen = self.history.pop().unwrap_or(Screen::Home);
    }

    /// Set the transient status message together with its severity, so the
    /// footer colors it via [`Theme::message_style`]. The one place message text
    /// and its color classification are kept in lockstep.
    fn set_status(&mut self, message: impl Into<String>, severity: Severity) {
        self.status = message.into();
        self.status_severity = severity;
    }

    /// Clear the status line back to its neutral resting state.
    fn clear_status(&mut self) {
        self.status.clear();
        self.status_severity = Severity::Info;
    }

    /// Flip focus between the home split's list and preview panes. Switching to
    /// the preview re-clamps its scroll in case the loaded body changed since.
    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::List => Focus::Preview,
            Focus::Preview => Focus::List,
        };
    }

    /// True when the active movement keys should scroll a body rather than move a
    /// selection: the detail screen always, or the home Preview pane when focused.
    fn scrolls_body(&self) -> bool {
        self.screen == Screen::Detail
            || (self.screen == Screen::Home && self.focus == Focus::Preview)
    }

    /// The (kind, id) of the result the preview should reflect — the highlighted
    /// search result or recent. `None` when nothing is selectable. This is the
    /// identity preview loads are tagged with for stale-response suppression.
    pub fn preview_selection(&self) -> Option<(ObjectKind, u64)> {
        self.home_target()
    }

    /// Note that the highlighted result may have changed, so the preview is now
    /// potentially out of date. The actual (de-duplicated, debounced) load is
    /// deferred to the next [`Self::on_preview_tick`] — never fired per keystroke.
    fn mark_preview_dirty(&mut self) {
        self.preview_dirty = true;
    }

    /// Test-only: force the dirty flag so the debounce flush can be exercised
    /// without driving a real selection change.
    #[cfg(test)]
    fn mark_preview_dirty_for_test(&mut self) {
        self.preview_dirty = true;
    }

    /// The debounce flush. Called on the fast always-on `PreviewTick`. When the
    /// selection has settled on something the preview doesn't already hold, issue
    /// exactly one [`AppCommand::LoadPreview`] tagged with that selection's
    /// identity. A burst of cursor movement coalesces into a single fetch here;
    /// no movement (or no change) issues nothing.
    fn on_preview_tick(&mut self) -> Vec<AppCommand> {
        // Only reconcile the preview while it's on screen and we're not mid-typing
        // a search/command (a fuzzy filter marks dirty; let the cursor settle), and
        // never while a profile switch is in flight (the preview would hit the old
        // client; it'll reconcile once the switch settles on the new instance).
        if self.screen != Screen::Home
            || self.mode != Mode::Normal
            || !self.preview_dirty
            || self.switch_in_flight()
        {
            return Vec::new();
        }
        self.preview_dirty = false;
        match self.preview_selection() {
            // Already showing this object → no fetch (idempotent on a still cursor).
            Some(target) if self.preview_for == Some(target) => Vec::new(),
            Some((kind, id)) => vec![AppCommand::LoadPreview { kind, id }],
            None => {
                // Nothing selectable: drop any stale preview so the pane shows
                // its placeholder instead of an orphaned object.
                self.preview = None;
                self.preview_for = None;
                self.preview_scroll = 0;
                Vec::new()
            }
        }
    }

    /// On an auto-refresh tick, re-run the last query (preserving the cursor)
    /// only when idle on the home screen — so it never fights user input.
    fn on_tick(&mut self) -> Vec<AppCommand> {
        // Skip the auto-refresh while a profile switch is in flight: it would
        // re-search the old client. The next tick after the switch settles picks up.
        if self.mode == Mode::Normal
            && self.screen == Screen::Home
            && !self.switch_in_flight()
            && let Some(query) = self.last_query.clone()
        {
            self.pending_reselect = self.selected_result().map(|r| (r.kind, r.id));
            return vec![AppCommand::Search { query, req: 0 }];
        }
        Vec::new()
    }

    /// Manual refresh of whatever's on screen, bound to `r`. On the detail screen
    /// it reloads the current object; on home it re-runs the active search the
    /// same way the auto-refresh tick does — through the same `AppCommand::Search`
    /// dispatch, capturing the cursor in `pending_reselect` so the selection is
    /// preserved when the results land. With nothing to refresh (no detail loaded,
    /// or an empty home with no prior query) it's a safe no-op with a gentle
    /// status, never a stray fetch.
    fn refresh_current_view(&mut self) -> Vec<AppCommand> {
        // Don't reload against the old client mid-switch.
        if self.fence_during_switch() {
            return Vec::new();
        }
        match self.screen {
            Screen::Detail => match &self.detail {
                Some(d) => {
                    let (kind, id) = (d.kind, d.id);
                    self.set_status("refreshing…", Severity::Info);
                    vec![AppCommand::LoadDetail { kind, id, req: 0 }]
                }
                None => Vec::new(),
            },
            Screen::Home => match self.last_query.clone() {
                Some(query) => {
                    self.pending_reselect = self.selected_result().map(|r| (r.kind, r.id));
                    self.set_status(format!("refreshing {query}…"), Severity::Info);
                    vec![AppCommand::Search { query, req: 0 }]
                }
                None => {
                    self.set_status("nothing to refresh", Severity::Warning);
                    Vec::new()
                }
            },
        }
    }

    /// Advance to the next built-in theme. A no-op under `NO_COLOR`: when the app
    /// was started in no-color mode (see [`Self::set_no_color`]) cycling to a
    /// colored theme would override the user's `NO_COLOR` request, so the no-color
    /// theme is left in place and a brief status explains why.
    pub fn cycle_theme(&mut self) {
        if self.no_color_blocks_theme_change() {
            return;
        }
        let list = Theme::list();
        self.theme_index = (self.theme_index + 1) % list.len();
        self.theme = Theme::by_name(list[self.theme_index]);
        self.set_status(format!("theme: {}", list[self.theme_index]), Severity::Info);
    }

    fn set_theme_by_name(&mut self, name: &str) {
        // Same NO_COLOR guard as the `t` cycle path: the palette `:theme <name>`
        // must not re-enable color under NO_COLOR (and so can't persist a colored
        // theme on exit). Reuses the one guard so the two paths can't diverge.
        if self.no_color_blocks_theme_change() {
            return;
        }
        self.theme = Theme::by_name(name);
        self.theme_index = Theme::index_of(name);
        self.set_status(format!("theme: {}", self.theme.name()), Severity::Info);
    }

    /// Shared NO_COLOR guard for both theme-change paths (`t` cycle and the
    /// palette `:theme <name>`). When the app was started in no-color mode (see
    /// [`Self::set_no_color`]) changing to a colored theme would override the
    /// user's `NO_COLOR` request — and would then persist a colored theme on exit
    /// — so we leave the no-color theme in place and explain why. Returns `true`
    /// when the change is blocked.
    fn no_color_blocks_theme_change(&mut self) -> bool {
        if self.theme.is_no_color() {
            self.set_status("NO_COLOR is set — theme change disabled", Severity::Info);
            true
        } else {
            false
        }
    }

    /// Cycle the active profile by `delta` (+1 forward, -1 backward), wrapping at
    /// both ends, and kick off a reconnect to it. A no-op with a brief status when
    /// fewer than two profiles are configured (nothing to cycle to). The header is
    /// NOT flipped here — it keeps pointing at the connected instance until the
    /// reconnect succeeds (see [`Self::switch_to_index`]), so the UI never lies
    /// about which server `client` is talking to. The network is spawned via
    /// [`AppCommand::SwitchProfile`]; the swap lands on [`AppEvent::ProfileSwitched`].
    fn cycle_profile(&mut self, forward: bool) -> Vec<AppCommand> {
        let len = self.profiles.len();
        if len < 2 {
            self.set_status("only one profile configured", Severity::Info);
            return Vec::new();
        }
        // Step from the pending target if a switch is in flight (so rapid cycling
        // advances as the user sees it), else from the connected profile. Wrap with
        // usize modular arithmetic: forward is +1 mod len; backward adds (len - 1)
        // mod len, so index 0 lands on the last profile without ever going negative.
        let from = self.pending_target_index().unwrap_or(self.profile_index);
        let step = if forward { 1 } else { len - 1 };
        let next = (from + step) % len;
        self.switch_to_index(next)
    }

    /// Jump to the profile named `name` (the palette `profile <name>` verb) and
    /// reconnect to it. An unknown name is a clear error status and no-op;
    /// selecting the already-active profile reconnects it (a cheap manual probe).
    fn select_profile(&mut self, name: &str) -> Vec<AppCommand> {
        match self.profiles.iter().position(|p| p.name == name) {
            Some(idx) => self.switch_to_index(idx),
            None => {
                self.set_status(format!("no profile named '{name}'"), Severity::Error);
                Vec::new()
            }
        }
    }

    /// Common path for cycling and named selection: kick off a reconnect to
    /// `profiles[idx]` WITHOUT touching the header — the header/`base_url`/client
    /// keep pointing at the currently connected instance until the switch
    /// succeeds (see the [`AppEvent::ProfileSwitched`] handler), so the UI can
    /// never claim to be on a server `client` isn't actually talking to. We only
    /// record the pending target (latest-switch-wins) and show a "switching…"
    /// status; the atomic swap (+ data clear + request-gen bump) happens on
    /// success. `idx` must be in range (callers guarantee it).
    fn switch_to_index(&mut self, idx: usize) -> Vec<AppCommand> {
        let entry = self.profiles[idx].clone();
        // Tag this as the latest switch so a slower, superseded one is dropped on
        // arrival, and so in-flight fetches are fenced until it settles.
        self.pending_profile = Some(entry.name.clone());
        self.set_status(format!("switching to '{}'…", entry.name), Severity::Info);
        vec![AppCommand::SwitchProfile {
            name: entry.name,
            config: entry.config,
        }]
    }

    /// True while a profile switch is in flight: the reconnect to a new instance
    /// has been dispatched but not yet settled. New search/detail/preview fetches
    /// are fenced during this window so they can't be issued against the old
    /// client mid-switch (see [`Self::fence_during_switch`]).
    fn switch_in_flight(&self) -> bool {
        self.pending_profile.is_some()
    }

    /// The `profiles` index of the in-flight switch's target, if any. Used so
    /// rapid cycling steps from the pending target rather than the (unchanged)
    /// connected profile.
    fn pending_target_index(&self) -> Option<usize> {
        let name = self.pending_profile.as_deref()?;
        self.profiles.iter().position(|p| p.name == name)
    }

    /// Guard for user-initiated fetches while a profile switch is in flight:
    /// returns `true` (and shows a brief status) when a fetch must be suppressed,
    /// so it isn't dispatched against the old client mid-switch. Returns `false`
    /// when no switch is pending and the caller should proceed normally.
    fn fence_during_switch(&mut self) -> bool {
        if self.switch_in_flight() {
            self.set_status("switching profile — try again in a moment", Severity::Info);
            true
        } else {
            false
        }
    }

    /// Wipe the data tied to the instance we're leaving and advance the search /
    /// detail request generations so any response still in flight for the old
    /// profile is dropped by the stale-response guard. Recents are cleared too:
    /// they reference the old instance's ids. The home cursor resets to the top.
    fn clear_for_profile_switch(&mut self) {
        // Advance both channels' high-water marks so outstanding requests land
        // stale (the guard compares `req >= *_gen`; bumping past every issued id
        // invalidates them). Preview loads carry (kind,id) and are cleared below.
        self.request_seq += 1;
        self.search_gen = self.request_seq;
        self.detail_gen = self.request_seq;

        self.results.clear();
        self.view.clear();
        self.recent.clear();
        self.selected = 0;
        self.detail = None;
        self.detail_tab = 0;
        self.detail_scroll = 0;
        self.preview = None;
        self.preview_for = None;
        self.preview_dirty = false;
        self.preview_scroll = 0;
        self.last_query = None;
        self.pending_reselect = None;
        // Back to the home screen on the new instance; the old history is moot.
        self.screen = Screen::Home;
        self.history.clear();
    }

    /// True when `name` is the profile the latest switch targeted. A
    /// `ProfileSwitched` for any other profile is from a superseded switch (the
    /// user cycled again before it returned) and is dropped on arrival.
    fn is_current_profile(&self, name: &str) -> bool {
        self.pending_profile.as_deref() == Some(name)
    }

    /// Recompute the visible `view` by fuzzy-filtering results on `search_input`.
    fn refilter(&mut self) {
        let displays: Vec<&str> = self.results.iter().map(|r| r.display.as_str()).collect();
        self.view = crate::tui::fuzzy::rank(self.search_input.value(), &displays);
        self.selected = 0;
        // The highlighted result moved; let the debounce reconcile the preview.
        self.mark_preview_dirty();
    }

    /// The currently selected result (through the filtered view), if any.
    pub fn selected_result(&self) -> Option<&SearchResult> {
        self.view
            .get(self.selected)
            .and_then(|&i| self.results.get(i))
    }

    /// Switch the active detail tab by its key (`i`/`p`/`c`/`v`); pressing the
    /// active tab's key again returns to the summary. No-op off the detail screen.
    fn select_detail_tab(&mut self, key: char) {
        if self.screen != Screen::Detail {
            return;
        }
        if let Some(detail) = &self.detail
            && let Some(pos) = detail.tabs.iter().position(|t| t.key == key)
        {
            let target = pos + 1;
            self.detail_tab = if self.detail_tab == target { 0 } else { target };
            // Each tab (and the summary) starts scrolled to the top.
            self.detail_scroll = 0;
        }
    }

    /// The body text for the active detail tab (summary when `detail_tab` is 0).
    pub fn detail_body(&self) -> &str {
        match &self.detail {
            Some(d) if self.detail_tab > 0 => d
                .tabs
                .get(self.detail_tab - 1)
                .map_or(d.body.as_str(), |t| t.body.as_str()),
            Some(d) => d.body.as_str(),
            None => "loading…",
        }
    }

    /// Total number of rendered content lines for the active detail view: the
    /// body's lines, plus the two-row tab bar (the bar + a blank spacer) when a
    /// device view has tabs. Mirrors what `render_detail` paints.
    pub fn detail_content_lines(&self) -> usize {
        let body_lines = self.detail_body().lines().count();
        let tab_rows = match &self.detail {
            Some(d) if !d.tabs.is_empty() => 2,
            _ => 0,
        };
        body_lines + tab_rows
    }

    /// The largest valid scroll offset: enough that the last content line sits at
    /// the bottom of the pane, never past it. 0 when the content fits (a no-op).
    /// Uses the last-known viewport height stashed at render time.
    fn detail_max_scroll(&self) -> u16 {
        let content = self.detail_content_lines() as u16;
        content.saturating_sub(self.detail_viewport)
    }

    /// A page of detail scroll: the visible height, minus one row of overlap so
    /// the reader keeps their place. At least one line even on a tiny pane.
    fn detail_page(&self) -> u16 {
        self.detail_viewport.saturating_sub(1).max(1)
    }

    fn detail_scroll_down(&mut self, lines: u16) {
        let max = self.detail_max_scroll();
        self.detail_scroll = self.detail_scroll.saturating_add(lines).min(max);
    }

    fn detail_scroll_up(&mut self, lines: u16) {
        self.detail_scroll = self.detail_scroll.saturating_sub(lines);
    }

    fn detail_scroll_top(&mut self) {
        self.detail_scroll = 0;
    }

    fn detail_scroll_bottom(&mut self) {
        self.detail_scroll = self.detail_max_scroll();
    }

    /// Stash the detail pane's inner height (content rows) so the pure scroll
    /// handler can clamp at the bottom, and re-clamp the current offset in case
    /// the pane shrank under it. Called from the render path each frame.
    pub fn sync_detail_viewport(&mut self, height: u16) {
        self.detail_viewport = height;
        self.detail_scroll = self.detail_scroll.min(self.detail_max_scroll());
    }

    // --- Body-scroll routing -------------------------------------------------
    //
    // The movement keys (j/k/g/G/PgUp/PgDn) scroll a body when [`Self::scrolls_body`]
    // is true. These dispatchers send that scroll to the detail screen's body or,
    // on Home, to the focused preview pane — reusing the same clamp-at-bottom
    // mechanics for both.

    fn body_scroll_down(&mut self, lines: u16) {
        if self.screen == Screen::Detail {
            self.detail_scroll_down(lines);
        } else {
            self.preview_scroll_down(lines);
        }
    }

    fn body_scroll_up(&mut self, lines: u16) {
        if self.screen == Screen::Detail {
            self.detail_scroll_up(lines);
        } else {
            self.preview_scroll_up(lines);
        }
    }

    fn body_scroll_top(&mut self) {
        if self.screen == Screen::Detail {
            self.detail_scroll_top();
        } else {
            self.preview_scroll = 0;
        }
    }

    fn body_scroll_bottom(&mut self) {
        if self.screen == Screen::Detail {
            self.detail_scroll_bottom();
        } else {
            self.preview_scroll = self.preview_max_scroll();
        }
    }

    /// A page of scroll for whichever body is active (detail vs preview).
    fn body_page(&self) -> u16 {
        if self.screen == Screen::Detail {
            self.detail_page()
        } else {
            self.preview_page()
        }
    }

    // --- Preview pane scrolling ----------------------------------------------
    //
    // Mirrors the detail-scroll machinery, scoped to the live preview body. The
    // preview shows the loaded detail's summary (tab 0); tabs aren't switchable
    // in the peek, so the line count is just the body's lines.

    /// The preview body shown in the right pane: the loaded detail's summary when
    /// it matches the highlighted row, otherwise a built-from-the-result
    /// lightweight peek (shown instantly while a full load is in flight, so the
    /// pane never displays a stale object's body under a moved cursor).
    pub fn preview_body(&self) -> String {
        match &self.preview {
            Some(d) if self.preview_for == self.preview_selection() => d.body.clone(),
            _ => self.preview_placeholder(),
        }
    }

    /// The preview pane's title: the loaded detail's title when current, else a
    /// short label from the highlighted row, else a neutral "Preview".
    pub fn preview_title(&self) -> String {
        match &self.preview {
            Some(d) if self.preview_for == self.preview_selection() => d.title.clone(),
            _ => match self.selected_home_preview() {
                Some(stub) => format!("{} {}", stub.kind, stub.display),
                None => "Preview".to_string(),
            },
        }
    }

    /// A lightweight peek assembled from the data already in the `SearchResult`
    /// (kind, display, subtitle, url) — shown instantly with no fetch while the
    /// full detail loads, or as the resting content if the load hasn't fired yet.
    fn preview_placeholder(&self) -> String {
        match self.selected_home_preview() {
            Some(PreviewStub {
                kind,
                display,
                subtitle,
                url,
            }) => {
                use std::fmt::Write;
                let mut s = format!("{kind}: {display}\n");
                if let Some(sub) = subtitle {
                    let _ = writeln!(s, "{sub}");
                }
                if let Some(url) = url {
                    let _ = writeln!(s, "\n{url}");
                }
                s.push_str("\nLoading details…");
                s
            }
            None => "Nothing selected.\n\nPress / to search NetBox.".to_string(),
        }
    }

    /// The data for a lightweight preview of the highlighted home row: a search
    /// result carries a URL/subtitle; a recent carries only its title.
    fn selected_home_preview(&self) -> Option<PreviewStub<'_>> {
        if self.results.is_empty() {
            self.recent.get(self.selected).map(|r| PreviewStub {
                kind: r.kind.as_str(),
                display: &r.title,
                subtitle: None,
                url: None,
            })
        } else {
            self.selected_result().map(|r| PreviewStub {
                kind: r.kind.as_str(),
                display: &r.display,
                subtitle: r.subtitle.as_deref(),
                url: Some(&r.url),
            })
        }
    }

    /// Number of rendered preview content lines (the body only — no tab bar).
    pub fn preview_content_lines(&self) -> usize {
        self.preview_body().lines().count()
    }

    fn preview_max_scroll(&self) -> u16 {
        (self.preview_content_lines() as u16).saturating_sub(self.preview_viewport)
    }

    fn preview_page(&self) -> u16 {
        self.preview_viewport.saturating_sub(1).max(1)
    }

    fn preview_scroll_down(&mut self, lines: u16) {
        let max = self.preview_max_scroll();
        self.preview_scroll = self.preview_scroll.saturating_add(lines).min(max);
    }

    fn preview_scroll_up(&mut self, lines: u16) {
        self.preview_scroll = self.preview_scroll.saturating_sub(lines);
    }

    /// Stash the preview pane's inner height and re-clamp the offset, mirroring
    /// [`Self::sync_detail_viewport`]. Called from the render path each frame.
    pub fn sync_preview_viewport(&mut self, height: u16) {
        self.preview_viewport = height;
        self.preview_scroll = self.preview_scroll.min(self.preview_max_scroll());
    }

    /// Record an opened object at the front of the recents list (deduped, capped).
    fn record_recent(&mut self, kind: ObjectKind, id: u64, title: String) {
        self.recent.retain(|r| !(r.kind == kind && r.id == id));
        self.recent.insert(0, RecentItem { kind, id, title });
        self.recent.truncate(RECENT_CAP);
    }

    /// Length of the active home list: search results, or recents when empty.
    /// Public so the render path can show a `selected/len` row-position hint.
    pub fn home_len(&self) -> usize {
        if self.results.is_empty() {
            self.recent.len()
        } else {
            self.view.len()
        }
    }

    /// The (kind, id) the home cursor points at — a result, or a recent.
    fn home_target(&self) -> Option<(ObjectKind, u64)> {
        if self.results.is_empty() {
            self.recent.get(self.selected).map(|r| (r.kind, r.id))
        } else {
            self.selected_result().map(|r| (r.kind, r.id))
        }
    }

    fn select_next(&mut self) {
        if self.selected + 1 < self.home_len() {
            self.selected += 1;
            self.mark_preview_dirty();
        }
    }

    fn select_prev(&mut self) {
        let before = self.selected;
        self.selected = self.selected.saturating_sub(1);
        if self.selected != before {
            self.mark_preview_dirty();
        }
    }

    fn select_first(&mut self) {
        if self.selected != 0 {
            self.selected = 0;
            self.mark_preview_dirty();
        }
    }

    fn select_last(&mut self) {
        let last = self.home_len().saturating_sub(1);
        if self.selected != last {
            self.selected = last;
            self.mark_preview_dirty();
        }
    }

    /// Rows the results-list cursor jumps on PgUp/PgDn: one screenful of the live
    /// list viewport, minus a row of overlap so the reader keeps their place.
    /// Before the first render (viewport still 0) the pre-render fallback applies;
    /// always at least one row.
    fn list_page(&self) -> usize {
        if self.list_viewport == 0 {
            PAGE_JUMP_FALLBACK
        } else {
            (self.list_viewport as usize).saturating_sub(1).max(1)
        }
    }

    fn select_page_down(&mut self) {
        let last = self.home_len().saturating_sub(1);
        let next = (self.selected + self.list_page()).min(last);
        if next != self.selected {
            self.selected = next;
            self.mark_preview_dirty();
        }
    }

    fn select_page_up(&mut self) {
        let next = self.selected.saturating_sub(self.list_page());
        if next != self.selected {
            self.selected = next;
            self.mark_preview_dirty();
        }
    }

    /// Stash the results-list pane's inner height (visible rows) so the pure
    /// PgUp/PgDn handler can page by one screenful. Called from the render path
    /// each frame. Mirrors [`Self::sync_detail_viewport`]/[`Self::sync_preview_viewport`].
    pub fn sync_list_viewport(&mut self, height: u16) {
        self.list_viewport = height;
    }

    /// Sync the stateful-table selection from the pure `selected` index so the
    /// render path scrolls the offset to keep the cursor visible. Empty tables
    /// select nothing (no panic); otherwise the index is clamped into range.
    /// `TableState` drives row visibility identically to the old `ListState`.
    pub fn sync_table_state(&mut self) {
        let len = self.home_len();
        if len == 0 {
            self.table_state.select(None);
        } else {
            self.selected = self.selected.min(len - 1);
            self.table_state.select(Some(self.selected));
        }
    }
}

/// The three aligned column cells for a search-result row: its kind, its display
/// label, and its site (taken from the result's `subtitle`, since the search
/// result carries no separate site field — the subtitle holds the site/scope).
/// Pure so the render path's row-building can be unit-tested without a terminal.
pub fn result_row_cells(result: &SearchResult) -> [String; 3] {
    [
        result.kind.as_str().to_string(),
        result.display.clone(),
        result.subtitle.clone().unwrap_or_default(),
    ]
}

/// A borrowed, lightweight view of the highlighted home row used to render the
/// preview placeholder before (or without) a full detail load.
struct PreviewStub<'a> {
    kind: &'a str,
    display: &'a str,
    subtitle: Option<&'a str>,
    url: Option<&'a str>,
}

/// Classify a free-form status message (typically pushed from an async task,
/// e.g. "copied: …" / "open failed: …") into a [`Severity`] so the footer can
/// color it. Failures → error, confirmations (copied/opened/refreshed/done) →
/// success, anything else → neutral. Pure and case-insensitive.
fn classify_status(message: &str) -> Severity {
    let m = message.to_ascii_lowercase();
    // "partial" wins over "failed": a partial result names failed endpoints but
    // is a degraded, not a hard, failure.
    if m.contains("partial") {
        Severity::Warning
    } else if m.contains("error") || m.contains("failed") || m.contains("failure") {
        Severity::Error
    } else if m.starts_with("copied")
        || m.starts_with("opened")
        || m.contains("refreshed")
        || m.contains("done")
    {
        Severity::Success
    } else {
        Severity::Info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProfileConfig;

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
            "4.5.5".into(),
            None,
        )
    }

    fn result(id: u64, display: &str) -> SearchResult {
        SearchResult {
            kind: ObjectKind::Device,
            id,
            display: display.into(),
            subtitle: None,
            url: format!("http://nb/dcim/devices/{id}/"),
            score: 100,
        }
    }

    fn result_of(kind: ObjectKind, id: u64, display: &str) -> SearchResult {
        SearchResult {
            kind,
            display: display.into(),
            ..result(id, display)
        }
    }

    #[test]
    fn result_row_cells_maps_kind_display_and_site() {
        // The aligned table's three columns come from the result: kind, display,
        // and site — where "site" is the subtitle (the search result has no
        // dedicated site field; the subtitle carries the site/scope).
        let r = SearchResult {
            subtitle: Some("iad1".into()),
            ..result(1, "edge01")
        };
        assert_eq!(
            result_row_cells(&r),
            [
                "device".to_string(),
                "edge01".to_string(),
                "iad1".to_string()
            ]
        );
    }

    #[test]
    fn result_row_cells_site_is_blank_without_subtitle() {
        // No subtitle → the SITE cell is empty (not "None"); the column still
        // aligns because the empty string keeps its place in the row.
        let r = result(2, "core02"); // subtitle is None
        assert_eq!(
            result_row_cells(&r),
            ["device".to_string(), "core02".to_string(), String::new()]
        );
    }

    fn press(code: KeyCode) -> AppEvent {
        AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Deliver a clean search result as if it were the freshest in-flight one:
    /// tagged with the app's current `search_gen` so the latest-wins guard
    /// adopts it (whether or not a search was actually dispatched first).
    fn set_results(a: &mut App, items: Vec<SearchResult>) {
        let req = a.search_gen;
        a.handle_event(AppEvent::SearchComplete {
            req,
            result: Ok(SearchOutcome {
                results: items,
                errors: Vec::new(),
            }),
        });
    }

    /// Deliver `result` on the search channel tagged as the current request, so
    /// it passes the latest-wins guard. For the stale-drop path a test stamps an
    /// older `req` explicitly.
    fn search_complete(a: &mut App, result: anyhow::Result<SearchOutcome>) {
        let req = a.search_gen;
        a.handle_event(AppEvent::SearchComplete { req, result });
    }

    /// Deliver a detail load tagged as the current request (passes the guard).
    fn detail_loaded(a: &mut App, result: anyhow::Result<DetailView>) {
        let req = a.detail_gen;
        a.handle_event(AppEvent::DetailLoaded { req, result });
    }

    #[test]
    fn classify_status_maps_messages_to_severity() {
        assert_eq!(classify_status("copied: edge01"), Severity::Success);
        assert_eq!(classify_status("opened in browser"), Severity::Success);
        assert_eq!(
            classify_status("copy failed: no clipboard"),
            Severity::Error
        );
        assert_eq!(classify_status("open failed: x"), Severity::Error);
        assert_eq!(classify_status("error: 404 not found"), Severity::Error);
        assert_eq!(
            classify_status("3 result(s) (partial: 1 endpoint(s) failed)"),
            Severity::Warning
        );
        assert_eq!(classify_status("searching edge…"), Severity::Info);
        assert_eq!(classify_status("theme: nord"), Severity::Info);
    }

    #[test]
    fn search_complete_sets_success_severity_clean_and_warning_partial() {
        let mut a = app();
        // A clean result set is a success-colored confirmation.
        set_results(&mut a, results_n(2));
        assert_eq!(a.status_severity, Severity::Success);
        // A partial result (some endpoints failed) is warning-colored.
        search_complete(
            &mut a,
            Ok(SearchOutcome {
                results: vec![result(1, "edge01")],
                errors: vec!["dcim/devices: 500".into()],
            }),
        );
        assert_eq!(a.status_severity, Severity::Warning);
        assert!(a.status.contains("partial"));
    }

    #[test]
    fn request_error_sets_error_severity() {
        let mut a = app();
        search_complete(&mut a, Err(anyhow::anyhow!("403 forbidden")));
        assert_eq!(a.status_severity, Severity::Error);
        detail_loaded(&mut a, Err(anyhow::anyhow!("404 not found")));
        assert_eq!(a.status_severity, Severity::Error);
    }

    #[test]
    fn async_status_event_is_classified() {
        let mut a = app();
        a.handle_event(AppEvent::Status("copied: edge01".into()));
        assert_eq!(a.status_severity, Severity::Success);
        a.handle_event(AppEvent::Status("copy failed: x".into()));
        assert_eq!(a.status_severity, Severity::Error);
    }

    // --- Loading spinner / in-flight counter --------------------------------

    #[test]
    fn dispatched_search_sets_loading_and_result_clears_it() {
        let mut a = app();
        assert!(!a.loading(), "idle at rest");
        // Submitting a search dispatches a fetch → loading.
        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(cmds.as_slice(), [AppCommand::Search { .. }]));
        assert!(a.loading(), "a dispatched search is in flight");
        assert_eq!(a.pending, 1);
        // The matching result clears it.
        set_results(&mut a, results_n(2));
        assert!(!a.loading(), "the result settles the fetch");
        assert_eq!(a.pending, 0);
    }

    #[test]
    fn enter_load_detail_sets_loading_until_detail_arrives() {
        let mut a = app();
        set_results(&mut a, vec![result(1, "edge01")]);
        assert!(!a.loading());
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(cmds.as_slice(), [AppCommand::LoadDetail { .. }]));
        assert!(a.loading());
        detail_loaded(&mut a, Ok(preview_view(1, "body")));
        assert!(!a.loading());
    }

    #[test]
    fn preview_load_sets_loading_until_preview_arrives() {
        let mut a = app();
        set_results(&mut a, results_n(3));
        // The settle of SearchComplete left us idle…
        assert!(!a.loading());
        // …the debounce flush issues a preview fetch → loading.
        let cmds = preview_tick(&mut a);
        assert!(matches!(cmds.as_slice(), [AppCommand::LoadPreview { .. }]));
        assert!(a.loading());
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 1,
            result: Ok(preview_view(1, "body")),
        });
        assert!(!a.loading());
    }

    #[test]
    fn stale_preview_response_still_clears_loading() {
        // A preview response that's dropped as stale (cursor moved on) must still
        // count the request down — otherwise the spinner would hang forever.
        let mut a = app();
        set_results(&mut a, results_n(3));
        let _ = preview_tick(&mut a); // LoadPreview for id 1
        assert!(a.loading());
        // A response for a *different* selection is dropped as stale…
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 2,
            result: Ok(preview_view(2, "stale")),
        });
        // …but the in-flight request is still accounted as settled.
        assert!(!a.loading());
        assert!(a.preview.is_none(), "stale body not adopted");
    }

    #[test]
    fn pending_counter_never_goes_negative() {
        // A result event with nothing in flight must not underflow the counter.
        let mut a = app();
        assert_eq!(a.pending, 0);
        detail_loaded(&mut a, Err(anyhow::anyhow!("404")));
        assert_eq!(a.pending, 0, "clamped at zero, no underflow");
        assert!(!a.loading());
    }

    #[test]
    fn concurrent_fetches_require_all_to_resolve_before_idle() {
        // A refresh search and a preview load are both in flight; idle is only
        // reached once *both* have resolved (the counter, not a bool, tracks it).
        let mut a = app();
        set_results(&mut a, results_n(3)); // settles to idle
        a.last_query = Some("edge".into());
        // A refresh tick dispatches a Search (counts as one).
        let refresh = a.handle_event(AppEvent::Tick);
        assert!(matches!(refresh.as_slice(), [AppCommand::Search { .. }]));
        // The debounce flush dispatches a preview load (counts as a second).
        let preview = preview_tick(&mut a);
        assert!(matches!(
            preview.as_slice(),
            [AppCommand::LoadPreview { .. }]
        ));
        assert_eq!(a.pending, 2);
        assert!(a.loading());
        // The preview resolves first — still loading (search outstanding).
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 1,
            result: Ok(preview_view(1, "body")),
        });
        assert_eq!(a.pending, 1);
        assert!(a.loading(), "still loading: the search is outstanding");
        // The search resolves — now idle.
        set_results(&mut a, results_n(3));
        assert_eq!(a.pending, 0);
        assert!(!a.loading());
    }

    #[test]
    fn spinner_advances_only_while_loading_and_stops_when_idle() {
        let mut a = app();
        // Idle: a preview tick must not advance the spinner (no busy-spin).
        let resting = a.spinner.frame().to_string();
        a.handle_event(AppEvent::PreviewTick);
        assert_eq!(a.spinner.frame(), resting, "idle spinner is still");

        // Put a fetch in flight, then ticks animate the spinner.
        set_results(&mut a, results_n(3));
        let _ = preview_tick(&mut a); // dispatch a LoadPreview → loading
        assert!(a.loading());
        let before = a.spinner.frame().to_string();
        a.handle_event(AppEvent::PreviewTick);
        assert_ne!(
            a.spinner.frame(),
            before,
            "loading spinner advances on tick"
        );

        // Resolve it: the spinner resets and stops advancing again.
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 1,
            result: Ok(preview_view(1, "body")),
        });
        assert!(!a.loading());
        let idle = a.spinner.frame().to_string();
        a.handle_event(AppEvent::PreviewTick);
        assert_eq!(a.spinner.frame(), idle, "idle again: spinner holds still");
    }

    #[test]
    fn open_and_copy_are_not_tracked_as_loading() {
        // Fire-and-forget side effects (open/copy) don't count as in-flight
        // fetches, so they never raise the spinner.
        let mut a = app();
        set_results(&mut a, vec![result(1, "edge01")]);
        let _ = a.handle_event(press(KeyCode::Char('o')));
        assert!(!a.loading(), "OpenBrowser is not a tracked fetch");
        let _ = a.handle_event(press(KeyCode::Char('y')));
        assert!(!a.loading(), "Copy is not a tracked fetch");
    }

    #[test]
    fn q_quits_from_home() {
        let mut a = app();
        assert!(a.handle_event(press(KeyCode::Char('q'))).is_empty());
        assert!(a.should_quit);
    }

    #[test]
    fn help_toggles_open_and_any_key_closes_it_without_quitting() {
        // Help is an overlay flag, not a screen: ?/F1 open it without changing the
        // underlying screen or pushing history; while open, any key closes it and
        // q (consumed by the modal) does NOT quit the app.
        let mut a = app();
        assert!(!a.help_open);
        a.handle_event(press(KeyCode::Char('?')));
        assert!(a.help_open);
        assert_eq!(a.screen, Screen::Home, "help doesn't change the screen");
        assert!(a.history.is_empty(), "help doesn't push history");
        // q while help is open closes the modal, not the app.
        a.handle_event(press(KeyCode::Char('q')));
        assert!(!a.help_open);
        assert!(!a.should_quit);
        assert_eq!(a.screen, Screen::Home);

        // F1 toggles it open too, and ?/F1 again (a fresh handler call) re-toggles
        // by way of any-key-close.
        a.handle_event(press(KeyCode::F(1)));
        assert!(a.help_open);
        a.handle_event(press(KeyCode::F(1)));
        assert!(!a.help_open, "any key (incl. F1) closes the open modal");

        // Esc also closes it.
        a.handle_event(press(KeyCode::Char('?')));
        assert!(a.help_open);
        a.handle_event(press(KeyCode::Esc));
        assert!(!a.help_open);
    }

    #[test]
    fn open_help_consumes_the_key_without_acting_on_underlying_screen() {
        // With help open, an arbitrary key (here `j`, normally a selection move on
        // Home) only closes the modal — it must NOT also move the selection or
        // disturb the underlying screen.
        let mut a = app();
        set_results(&mut a, results_n(3));
        assert_eq!(a.selected, 0);
        a.handle_event(press(KeyCode::Char('?')));
        assert!(a.help_open);
        let cmds = a.handle_event(press(KeyCode::Char('j')));
        assert!(cmds.is_empty(), "an any-key-close issues no command");
        assert!(!a.help_open, "the key closed the modal");
        assert_eq!(a.selected, 0, "the key did not move the selection");
        assert_eq!(a.screen, Screen::Home);
    }

    #[test]
    fn slash_enters_search_and_enter_emits_command() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('/')));
        assert_eq!(a.mode, Mode::Search);
        for c in "edge01".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Normal);
        assert!(matches!(cmds.as_slice(), [AppCommand::Search { query: q, .. }] if q == "edge01"));
        assert_eq!(a.last_query.as_deref(), Some("edge01"));
    }

    #[test]
    fn typing_in_search_fuzzy_filters_the_view() {
        let mut a = app();
        set_results(
            &mut a,
            vec![
                result(1, "edge01"),
                result(2, "core02"),
                result(3, "edge-rtr"),
            ],
        );
        assert_eq!(a.view.len(), 3);

        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        // Only the two "edge" results remain in the filtered view.
        assert_eq!(a.view.len(), 2);
        let visible: Vec<u64> = a.view.iter().map(|&i| a.results[i].id).collect();
        assert!(visible.contains(&1) && visible.contains(&3));
        assert!(!visible.contains(&2));
    }

    #[test]
    fn esc_cancels_search_without_emitting_a_command() {
        // Esc must leave search mode and issue no command, with the typed text
        // discarded (the next `/` opens a fresh, empty line).
        let mut a = app();
        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        assert_eq!(a.search_input.value(), "edge");
        let cmds = a.handle_event(press(KeyCode::Esc));
        assert!(cmds.is_empty());
        assert_eq!(a.mode, Mode::Normal);
        // Reopening search starts empty.
        a.handle_event(press(KeyCode::Char('/')));
        assert_eq!(a.search_input.value(), "");
    }

    #[test]
    fn search_cursor_editing_inserts_mid_string() {
        // The cheese-backed cursor lets Home/Left position the insertion point so
        // characters land mid-string, driven entirely through the pure handler.
        let mut a = app();
        a.handle_event(press(KeyCode::Char('/')));
        for c in "ege".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        // Move to the start, step right once (between 'e' and 'g'), insert 'd'.
        a.handle_event(press(KeyCode::Home));
        a.handle_event(press(KeyCode::Right));
        a.handle_event(press(KeyCode::Char('d')));
        assert_eq!(a.search_input.value(), "edge");
        // Backspace removes the char before the cursor (the 'd' just typed).
        a.handle_event(press(KeyCode::Backspace));
        assert_eq!(a.search_input.value(), "ege");
        // Delete (forward) at this position removes the 'g' under the cursor.
        a.handle_event(press(KeyCode::Delete));
        assert_eq!(a.search_input.value(), "ee");
    }

    #[test]
    fn search_enter_after_cursor_editing_submits_full_value() {
        // After mid-string editing, Enter still submits the whole buffer as the
        // search query — the submit semantics are unchanged by the cursor work.
        let mut a = app();
        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge1".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        a.handle_event(press(KeyCode::Home));
        a.handle_event(press(KeyCode::Char('x'))); // prepend
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(cmds.as_slice(), [AppCommand::Search { query: q, .. }] if q == "xedge1"));
        assert_eq!(a.last_query.as_deref(), Some("xedge1"));
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn esc_cancels_command_palette() {
        // Esc closes the palette, runs nothing, and discards the typed command.
        let mut a = app();
        a.handle_event(press(KeyCode::Char(':')));
        for c in "device edge01".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        assert_eq!(a.command_input.value(), "device edge01");
        let cmds = a.handle_event(press(KeyCode::Esc));
        assert!(cmds.is_empty());
        assert_eq!(a.mode, Mode::Normal);
        a.handle_event(press(KeyCode::Char(':')));
        assert_eq!(a.command_input.value(), "");
    }

    #[test]
    fn command_cursor_editing_then_enter_executes_edited_command() {
        // Cursor editing in the palette feeds into the same parse → AppCommand
        // flow: build "device edge01" by inserting the digit mid-token, submit.
        let mut a = app();
        a.handle_event(press(KeyCode::Char(':')));
        for c in "device edge0".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        a.handle_event(press(KeyCode::Char('1')));
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadByRef { kind: ObjectKind::Device, value, .. }] if value == "edge01"
        ));
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn enter_on_result_loads_detail_and_back_returns_home() {
        let mut a = app();
        set_results(&mut a, vec![result(1, "edge01")]);
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadDetail {
                kind: ObjectKind::Device,
                id: 1,
                ..
            }]
        ));

        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "device edge01".into(),
                body: "name: edge01".into(),
                tabs: Vec::new(),
            }),
        );
        assert_eq!(a.screen, Screen::Detail);
        // Opening recorded it in recents.
        assert_eq!(a.recent.len(), 1);
        assert_eq!(a.recent[0].id, 1);

        a.handle_event(press(KeyCode::Char('b')));
        assert_eq!(a.screen, Screen::Home);
    }

    #[test]
    fn enter_on_new_kind_result_loads_detail_with_that_kind() {
        // The newer search kinds (circuit/aggregate/ASN/IP-range) must dispatch
        // a LoadDetail carrying their own ObjectKind + id — the same glue the
        // device path uses — so they open in the TUI like every other kind.
        for kind in [
            ObjectKind::Circuit,
            ObjectKind::Aggregate,
            ObjectKind::Asn,
            ObjectKind::IpRange,
        ] {
            let mut a = app();
            set_results(&mut a, vec![result_of(kind, 7, "thing")]);
            let cmds = a.handle_event(press(KeyCode::Enter));
            match cmds.as_slice() {
                [AppCommand::LoadDetail { kind: k, id, .. }] => {
                    assert_eq!(*k, kind, "wrong kind dispatched for {kind:?}");
                    assert_eq!(*id, 7);
                }
                other => panic!("expected LoadDetail for {kind:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn new_kind_detail_renders_non_empty_body() {
        // Once a new-kind detail loads, its (tab-less) view body must be the
        // string the render path paints — non-empty and unaffected by the
        // device-only tab machinery.
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Asn,
                id: 3,
                title: "asn 64500".into(),
                body: "asn: 64500\nrir: ARIN".into(),
                tabs: Vec::new(),
            }),
        );
        assert_eq!(a.screen, Screen::Detail);
        assert_eq!(a.detail_tab, 0);
        assert_eq!(a.detail_body(), "asn: 64500\nrir: ARIN");
        // No sub-resource tabs for these kinds, so a device tab key is a no-op.
        a.handle_event(press(KeyCode::Char('i')));
        assert_eq!(a.detail_tab, 0);
        assert_eq!(a.detail_body(), "asn: 64500\nrir: ARIN");
        // Reopening recorded it under its own kind.
        assert_eq!(a.recent[0].kind, ObjectKind::Asn);
    }

    #[test]
    fn recents_dedup_and_reopen_from_home() {
        let mut a = app();
        let load = |a: &mut App, id, title: &str| {
            detail_loaded(
                a,
                Ok(DetailView {
                    kind: ObjectKind::Device,
                    id,
                    title: title.into(),
                    body: String::new(),
                    tabs: Vec::new(),
                }),
            );
            a.handle_event(press(KeyCode::Char('b')));
        };
        load(&mut a, 1, "device a");
        load(&mut a, 2, "device b");
        load(&mut a, 1, "device a"); // reopening 1 moves it to front, no dup
        assert_eq!(a.recent.len(), 2);
        assert_eq!(a.recent[0].id, 1);

        // No search results → Home shows recents; Enter reopens the selected one.
        assert!(a.results.is_empty());
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadDetail { id: 1, .. }]
        ));
    }

    #[test]
    fn palette_lookup_emits_load_by_ref() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char(':')));
        for c in "device edge01".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Normal);
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadByRef { kind: ObjectKind::Device, value, .. }] if value == "edge01"
        ));
    }

    #[test]
    fn palette_theme_changes_theme_in_place() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char(':')));
        for c in "theme nord".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(cmds.is_empty());
        assert_eq!(a.theme.name(), "nord");
    }

    #[test]
    fn palette_theme_respects_no_color_and_cannot_reenable_color() {
        // M1: the palette `:theme <name>` path must honour NO_COLOR the same way
        // the `t` cycle does — it cannot re-enable color, and so cannot persist a
        // colored theme on exit (the persist guard keys off theme.name()).
        let mut a = app();
        a.set_no_color();
        assert!(a.theme.is_no_color());

        a.handle_event(press(KeyCode::Char(':')));
        for c in "theme nord".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(cmds.is_empty());
        assert!(
            a.theme.is_no_color(),
            "still no-color: :theme can't override NO_COLOR"
        );
        assert_eq!(a.theme.name(), "no_color");
        // The exit-time persist guard (theme.name() != initial_theme) stays a
        // no-op, so no colored theme can be written back under NO_COLOR.
        assert_eq!(a.theme.name(), a.initial_theme);
        assert_eq!(a.status, "NO_COLOR is set — theme change disabled");

        // Control: without NO_COLOR the palette path still changes the theme.
        let mut b = app();
        b.handle_event(press(KeyCode::Char(':')));
        for c in "theme nord".chars() {
            b.handle_event(press(KeyCode::Char(c)));
        }
        b.handle_event(press(KeyCode::Enter));
        assert_eq!(
            b.theme.name(),
            "nord",
            "palette theme works without NO_COLOR"
        );
    }

    #[test]
    fn o_and_y_emit_open_and_copy() {
        let mut a = app();
        set_results(&mut a, vec![result(1, "edge01")]);
        let open = a.handle_event(press(KeyCode::Char('o')));
        assert!(
            matches!(open.as_slice(), [AppCommand::OpenBrowser(u)] if u == "http://nb/dcim/devices/1/")
        );
        let copy = a.handle_event(press(KeyCode::Char('y')));
        assert!(matches!(copy.as_slice(), [AppCommand::Copy(v)] if v == "edge01"));
    }

    #[test]
    fn ctrl_w_deletes_last_word() {
        // Word-delete is delegated to the cheese-backed input, but it must still
        // work through the pure search handler: type a two-word query, Ctrl+W
        // eats the trailing word (and its space).
        let mut a = app();
        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge router".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        )));
        // The trailing word is gone; the separating space remains, matching the
        // previous trim-last-word behavior.
        assert_eq!(a.search_input.value(), "edge ");
    }

    #[test]
    fn theme_cycles() {
        let mut a = app();
        let before = a.theme_index;
        a.handle_event(press(KeyCode::Char('t')));
        assert_ne!(a.theme_index, before);
    }

    #[test]
    fn set_no_color_switches_to_monochrome_theme_and_pins_initial() {
        let mut a = app();
        assert!(!a.theme.is_no_color());
        a.set_no_color();
        assert!(a.theme.is_no_color());
        assert_eq!(a.theme.name(), "no_color");
        // initial_theme is pinned to the same sentinel so the exit-time persist
        // guard (theme.name() != initial_theme) stays a no-op: NO_COLOR must not
        // overwrite the user's configured theme.
        assert_eq!(a.initial_theme, a.theme.name());
    }

    #[test]
    fn cycle_theme_is_disabled_under_no_color() {
        let mut a = app();
        a.set_no_color();
        assert!(a.theme.is_no_color());
        let name_before = a.theme.name().to_string();

        a.cycle_theme();

        // The no-color theme stays put (no swap to a colored theme) and a status
        // explains why, instead of overriding the user's NO_COLOR request.
        assert!(a.theme.is_no_color(), "still no-color after cycle");
        assert_eq!(a.theme.name(), name_before);
        assert_eq!(a.status, "NO_COLOR is set — theme change disabled");

        // Control: a normal (colored) theme still cycles to a different one.
        let mut b = app();
        let before = b.theme.name().to_string();
        b.cycle_theme();
        assert_ne!(b.theme.name(), before, "a colored theme cycles normally");
    }

    #[test]
    fn tick_refreshes_last_query_only_when_idle_on_home() {
        let mut a = app();
        // No last query → tick does nothing.
        assert!(a.handle_event(AppEvent::Tick).is_empty());

        a.last_query = Some("edge".into());
        let cmds = a.handle_event(AppEvent::Tick);
        assert!(matches!(cmds.as_slice(), [AppCommand::Search { query: q, .. }] if q == "edge"));

        // While typing a search, a tick must not fire a refresh.
        a.mode = Mode::Search;
        assert!(a.handle_event(AppEvent::Tick).is_empty());
    }

    #[test]
    fn refresh_preserves_selection_by_id() {
        let mut a = app();
        set_results(&mut a, vec![result(1, "a"), result(2, "b"), result(3, "c")]);
        a.selected = 2; // select id=3
        a.last_query = Some("x".into());

        let _ = a.handle_event(AppEvent::Tick); // captures pending_reselect = (Device, 3)
        // New results arrive in a different order; cursor should track id=3.
        set_results(&mut a, vec![result(3, "c"), result(1, "a"), result(2, "b")]);
        assert_eq!(a.selected_result().map(|r| r.id), Some(3));
    }

    #[test]
    fn r_on_home_with_results_dispatches_refresh_search_preserving_selection() {
        // `r` re-runs the active search through the same Search dispatch the
        // auto-refresh tick uses, capturing the cursor so it's preserved when
        // the (possibly reordered) results land.
        let mut a = app();
        set_results(&mut a, vec![result(1, "a"), result(2, "b"), result(3, "c")]);
        a.last_query = Some("edge".into());
        a.selected = 2; // id = 3

        let cmds = a.handle_event(press(KeyCode::Char('r')));
        assert!(matches!(cmds.as_slice(), [AppCommand::Search { query: q, .. }] if q == "edge"));
        assert!(a.loading(), "refresh is a tracked fetch");
        assert!(a.status.contains("refreshing"));
        // Results return reordered; the cursor still tracks id=3.
        set_results(&mut a, vec![result(3, "c"), result(1, "a"), result(2, "b")]);
        assert_eq!(a.selected_result().map(|r| r.id), Some(3));
    }

    #[test]
    fn r_on_detail_dispatches_a_reload_of_the_current_object() {
        // `r` on the detail screen reloads the open object through the same
        // LoadDetail dispatch Enter uses, so the spinner/status flow is shared.
        let mut a = app();
        set_results(&mut a, vec![result(7, "edge07")]);
        a.handle_event(press(KeyCode::Enter));
        detail_loaded(&mut a, Ok(preview_view(7, "body")));
        assert_eq!(a.screen, Screen::Detail);

        let cmds = a.handle_event(press(KeyCode::Char('r')));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadDetail {
                kind: ObjectKind::Device,
                id: 7,
                ..
            }]
        ));
        assert!(a.loading(), "a detail reload is a tracked fetch");
        assert!(a.status.contains("refreshing"));
    }

    #[test]
    fn r_on_empty_home_is_a_safe_noop() {
        // Nothing searched yet: `r` must not fire a stray fetch, only leave a
        // gentle status.
        let mut a = app();
        assert!(a.last_query.is_none());
        let cmds = a.handle_event(press(KeyCode::Char('r')));
        assert!(cmds.is_empty(), "no query → no fetch");
        assert!(!a.loading());
        assert_eq!(a.status_severity, Severity::Warning);
        assert!(a.status.contains("nothing to refresh"));
    }

    fn results_n(n: u64) -> Vec<SearchResult> {
        (1..=n).map(|i| result(i, &format!("dev{i}"))).collect()
    }

    #[test]
    fn j_k_clamp_at_both_ends() {
        let mut a = app();
        set_results(&mut a, results_n(3));
        // k at the top stays at 0 (no underflow).
        a.handle_event(press(KeyCode::Up));
        assert_eq!(a.selected, 0);
        // j walks down and stops at the last row.
        for _ in 0..10 {
            a.handle_event(press(KeyCode::Down));
        }
        assert_eq!(a.selected, 2);
        // k walks back up to the top.
        for _ in 0..10 {
            a.handle_event(press(KeyCode::Up));
        }
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn g_and_capital_g_jump_to_first_and_last() {
        let mut a = app();
        set_results(&mut a, results_n(50));
        a.handle_event(press(KeyCode::Char('G')));
        assert_eq!(a.selected, 49);
        a.handle_event(press(KeyCode::Char('g')));
        assert_eq!(a.selected, 0);
        // Home/End are aliases for g/G.
        a.handle_event(press(KeyCode::End));
        assert_eq!(a.selected, 49);
        a.handle_event(press(KeyCode::Home));
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn page_down_and_up_jump_and_clamp() {
        // Updated for the viewport-aware page jump: with a stashed list viewport
        // of 21 the step is one screenful minus a row of overlap = 20 (was the
        // old fixed PAGE_JUMP of 10). The jump/clamp behaviour is otherwise
        // unchanged — this test now exercises the live-viewport math, not a
        // hard-coded constant.
        let mut a = app();
        set_results(&mut a, results_n(50));
        a.sync_list_viewport(21); // page = 21 - 1 = 20
        let page = 20;
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.selected, page); // 0 → 20
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.selected, page * 2); // 20 → 40 (still room before the last row)
        // PgUp steps back by a page (40 → 20).
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.selected, page);
        // PgUp clamps at the top without underflow.
        a.handle_event(press(KeyCode::PageUp));
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.selected, 0);
        // PgDn clamps at the last row.
        for _ in 0..20 {
            a.handle_event(press(KeyCode::PageDown));
        }
        assert_eq!(a.selected, 49);
    }

    #[test]
    fn page_jump_uses_live_list_viewport() {
        // A tall viewport pages by ~one screenful (viewport - 1 of overlap).
        let mut a = app();
        set_results(&mut a, results_n(100));
        a.sync_list_viewport(20); // page = 19
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.selected, 19);
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.selected, 0);

        // A short viewport pages by a correspondingly smaller step.
        let mut b = app();
        set_results(&mut b, results_n(100));
        b.sync_list_viewport(5); // page = 4
        b.handle_event(press(KeyCode::PageDown));
        assert_eq!(b.selected, 4);
        b.handle_event(press(KeyCode::PageUp));
        assert_eq!(b.selected, 0);
    }

    #[test]
    fn page_jump_falls_back_before_first_render() {
        // Before any render the viewport is 0, so the pre-render fallback applies.
        let mut a = app();
        set_results(&mut a, results_n(50));
        assert_eq!(a.list_viewport, 0);
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.selected, PAGE_JUMP_FALLBACK);
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn page_jump_minimum_one_row_on_tiny_viewport() {
        // A 1-row viewport still advances at least one row (never stalls).
        let mut a = app();
        set_results(&mut a, results_n(10));
        a.sync_list_viewport(1); // page = max(0, 1) = 1
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.selected, 1);
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn page_jump_clamps_at_both_ends_with_viewport() {
        let mut a = app();
        set_results(&mut a, results_n(50));
        a.sync_list_viewport(11); // page = 10
        // PgDn clamps at the last row no matter how many pages we ask for.
        for _ in 0..20 {
            a.handle_event(press(KeyCode::PageDown));
        }
        assert_eq!(a.selected, 49);
        // PgUp clamps at the top.
        for _ in 0..20 {
            a.handle_event(press(KeyCode::PageUp));
        }
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn page_jump_empty_list_is_a_noop() {
        let mut a = app();
        a.sync_list_viewport(20);
        a.handle_event(press(KeyCode::PageDown));
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.selected, 0);
        a.sync_table_state();
        assert_eq!(a.table_state.selected(), None);
    }

    #[test]
    fn selection_past_viewport_stays_valid_and_syncs_table_state() {
        // Walk the cursor well past any plausible viewport height; the index
        // must stay in range and the stateful selection must track it so
        // ratatui scrolls the offset to keep the row visible.
        let mut a = app();
        set_results(&mut a, results_n(100));
        for _ in 0..40 {
            a.handle_event(press(KeyCode::Down));
        }
        assert_eq!(a.selected, 40);
        a.sync_table_state();
        assert_eq!(a.table_state.selected(), Some(40));
        // And the selected row resolves to a real result.
        assert_eq!(a.selected_result().map(|r| r.id), Some(41));
    }

    #[test]
    fn empty_list_movement_is_a_noop_and_selects_nothing() {
        let mut a = app();
        // No results, no recents: every movement key is harmless.
        for key in [
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Char('G'),
            KeyCode::Char('g'),
            KeyCode::PageDown,
            KeyCode::PageUp,
        ] {
            a.handle_event(press(key));
        }
        assert_eq!(a.selected, 0);
        a.sync_table_state();
        assert_eq!(a.table_state.selected(), None);
    }

    #[test]
    fn single_item_list_does_not_panic_and_selects_row_zero() {
        let mut a = app();
        set_results(&mut a, results_n(1));
        a.handle_event(press(KeyCode::Down));
        a.handle_event(press(KeyCode::Char('G')));
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.selected, 0);
        a.sync_table_state();
        assert_eq!(a.table_state.selected(), Some(0));
    }

    #[test]
    fn sync_clamps_stale_selection_into_range() {
        // If the list shrinks under a stale cursor, sync must clamp rather than
        // hand ratatui an out-of-range selection.
        let mut a = app();
        set_results(&mut a, results_n(10));
        a.selected = 8;
        set_results(&mut a, results_n(3)); // SearchComplete resets selected to 0…
        a.selected = 9; // …but simulate a stale index anyway.
        a.sync_table_state();
        assert_eq!(a.selected, 2);
        assert_eq!(a.table_state.selected(), Some(2));
    }

    /// Load a tab-less detail whose body is `n` lines, then stash a viewport so
    /// the scroll handler can clamp at the bottom.
    fn detail_with_body_lines(a: &mut App, n: usize, viewport: u16) {
        let body = (0..n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        detail_loaded(
            a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "device edge01".into(),
                body,
                tabs: Vec::new(),
            }),
        );
        a.sync_detail_viewport(viewport);
    }

    #[test]
    fn detail_scroll_down_and_up_clamp_at_both_ends() {
        let mut a = app();
        // 30 lines of content in a 10-row pane → max offset is 20.
        detail_with_body_lines(&mut a, 30, 10);
        assert_eq!(a.detail_scroll, 0);

        // Up at the top is a no-op (no underflow).
        a.handle_event(press(KeyCode::Up));
        assert_eq!(a.detail_scroll, 0);
        a.handle_event(press(KeyCode::Char('k')));
        assert_eq!(a.detail_scroll, 0);

        // j / Down scroll one line each.
        a.handle_event(press(KeyCode::Char('j')));
        a.handle_event(press(KeyCode::Down));
        assert_eq!(a.detail_scroll, 2);

        // Walk past the bottom; the offset clamps at content - viewport = 20.
        for _ in 0..50 {
            a.handle_event(press(KeyCode::Char('j')));
        }
        assert_eq!(a.detail_scroll, 20);

        // k walks back up and clamps at the top.
        for _ in 0..50 {
            a.handle_event(press(KeyCode::Char('k')));
        }
        assert_eq!(a.detail_scroll, 0);
    }

    #[test]
    fn detail_page_jump_and_clamp() {
        let mut a = app();
        // 100 lines in a 10-row pane → a page is viewport-1 = 9, max offset 90.
        detail_with_body_lines(&mut a, 100, 10);

        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.detail_scroll, 9);
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.detail_scroll, 18);

        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.detail_scroll, 9);

        // PgUp clamps at the top.
        a.handle_event(press(KeyCode::PageUp));
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.detail_scroll, 0);

        // PgDn clamps at the bottom (max = 90).
        for _ in 0..50 {
            a.handle_event(press(KeyCode::PageDown));
        }
        assert_eq!(a.detail_scroll, 90);
    }

    #[test]
    fn detail_g_and_capital_g_jump_to_top_and_bottom() {
        let mut a = app();
        detail_with_body_lines(&mut a, 50, 10); // max offset = 40

        a.handle_event(press(KeyCode::Char('G')));
        assert_eq!(a.detail_scroll, 40);
        a.handle_event(press(KeyCode::Char('g')));
        assert_eq!(a.detail_scroll, 0);

        // Home / End are aliases for g / G.
        a.handle_event(press(KeyCode::End));
        assert_eq!(a.detail_scroll, 40);
        a.handle_event(press(KeyCode::Home));
        assert_eq!(a.detail_scroll, 0);
    }

    #[test]
    fn detail_short_content_scrolling_is_a_noop() {
        let mut a = app();
        // 5 lines comfortably fit a 20-row pane → nothing scrolls.
        detail_with_body_lines(&mut a, 5, 20);
        for key in [
            KeyCode::Char('j'),
            KeyCode::Down,
            KeyCode::Char('G'),
            KeyCode::End,
            KeyCode::PageDown,
        ] {
            a.handle_event(press(key));
            assert_eq!(
                a.detail_scroll, 0,
                "key {key:?} must not scroll short content"
            );
        }
    }

    #[test]
    fn detail_empty_body_is_safe() {
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "device edge01".into(),
                body: String::new(),
                tabs: Vec::new(),
            }),
        );
        a.sync_detail_viewport(10);
        // Every scroll key is a harmless no-op on an empty body.
        for key in [
            KeyCode::Char('j'),
            KeyCode::Char('G'),
            KeyCode::PageDown,
            KeyCode::Char('k'),
        ] {
            a.handle_event(press(key));
        }
        assert_eq!(a.detail_scroll, 0);
    }

    #[test]
    fn detail_scroll_clamps_when_viewport_unknown() {
        // Before the first render the viewport is 0, so max scroll equals the
        // full content length — but we must never panic or overflow.
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "t".into(),
                body: "a\nb\nc".into(),
                tabs: Vec::new(),
            }),
        );
        assert_eq!(a.detail_viewport, 0);
        for _ in 0..10 {
            a.handle_event(press(KeyCode::Char('j')));
        }
        // Content is 3 lines; with a 0 viewport max scroll is 3.
        assert_eq!(a.detail_scroll, 3);
    }

    #[test]
    fn detail_scroll_resets_on_navigation_to_new_object() {
        let mut a = app();
        detail_with_body_lines(&mut a, 50, 10);
        a.handle_event(press(KeyCode::Char('G')));
        assert_eq!(a.detail_scroll, 40);

        // Opening a different object resets the offset to the top.
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 2,
                title: "device edge02".into(),
                body: "fresh".into(),
                tabs: Vec::new(),
            }),
        );
        assert_eq!(a.detail_scroll, 0);
    }

    #[test]
    fn detail_scroll_resets_on_tab_switch() {
        use crate::domain::detail::DetailTab;
        let long = |label: &str| {
            (0..40)
                .map(|i| format!("{label} {i}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "device edge01".into(),
                body: long("summary"),
                tabs: vec![DetailTab {
                    key: 'i',
                    label: "interfaces".into(),
                    body: long("iface"),
                }],
            }),
        );
        a.sync_detail_viewport(10);
        a.handle_event(press(KeyCode::Char('G')));
        assert!(a.detail_scroll > 0);

        // Switching to the interfaces tab starts at the top.
        a.handle_event(press(KeyCode::Char('i')));
        assert_eq!(a.detail_tab, 1);
        assert_eq!(a.detail_scroll, 0);

        // Scroll within the tab, then toggle back to summary — also top.
        a.sync_detail_viewport(10);
        a.handle_event(press(KeyCode::Char('G')));
        assert!(a.detail_scroll > 0);
        a.handle_event(press(KeyCode::Char('i')));
        assert_eq!(a.detail_tab, 0);
        assert_eq!(a.detail_scroll, 0);
    }

    #[test]
    fn detail_scroll_keys_do_not_collide_with_device_tab_keys() {
        use crate::domain::detail::DetailTab;
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "device edge01".into(),
                body: (0..40)
                    .map(|i| format!("s {i}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                tabs: vec![DetailTab {
                    key: 'i',
                    label: "interfaces".into(),
                    body: "iface body".into(),
                }],
            }),
        );
        a.sync_detail_viewport(10);
        // The tab key still switches tabs (and resets scroll)…
        a.handle_event(press(KeyCode::Char('i')));
        assert_eq!(a.detail_tab, 1);
        // …while j/k scroll without disturbing the active tab.
        a.sync_detail_viewport(10);
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.detail_tab, 1);
        assert_eq!(a.detail_scroll, 0); // 10-line body fits a 10-row pane → no-op
    }

    #[test]
    fn home_movement_keys_still_move_selection_not_scroll() {
        // Guarding the detail-scroll arms on Screen::Detail must not change the
        // home list behaviour: j/G there still move the selection cursor.
        let mut a = app();
        set_results(&mut a, results_n(20));
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.selected, 1);
        assert_eq!(a.detail_scroll, 0);
        a.handle_event(press(KeyCode::Char('G')));
        assert_eq!(a.selected, 19);
    }

    #[test]
    fn device_tabs_select_and_toggle() {
        use crate::domain::detail::DetailTab;
        let tab = |key, label: &str| DetailTab {
            key,
            label: label.into(),
            body: format!("{label} body"),
        };
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "device edge01".into(),
                body: "summary".into(),
                tabs: vec![tab('i', "interfaces"), tab('p', "ips"), tab('v', "vlans")],
            }),
        );
        assert_eq!(a.screen, Screen::Detail);
        assert_eq!(a.detail_tab, 0);
        assert_eq!(a.detail_body(), "summary");

        a.handle_event(press(KeyCode::Char('i')));
        assert_eq!(a.detail_tab, 1);
        assert_eq!(a.detail_body(), "interfaces body");

        a.handle_event(press(KeyCode::Char('v')));
        assert_eq!(a.detail_tab, 3);
        assert_eq!(a.detail_body(), "vlans body");

        // Pressing the active tab's key again returns to the summary.
        a.handle_event(press(KeyCode::Char('v')));
        assert_eq!(a.detail_tab, 0);

        // A tab key with no matching section is a no-op (no cables here).
        a.handle_event(press(KeyCode::Char('c')));
        assert_eq!(a.detail_tab, 0);
    }

    // --- Home split: focus + preview ----------------------------------------

    /// Drive the debounce flush: the preview load only fires on a `PreviewTick`,
    /// never per keystroke.
    fn preview_tick(a: &mut App) -> Vec<AppCommand> {
        a.handle_event(AppEvent::PreviewTick)
    }

    fn preview_view(id: u64, body: &str) -> DetailView {
        DetailView {
            kind: ObjectKind::Device,
            id,
            title: format!("device {id}"),
            body: body.into(),
            tabs: Vec::new(),
        }
    }

    #[test]
    fn tab_and_backtab_cycle_focus() {
        let mut a = app();
        set_results(&mut a, results_n(3));
        assert_eq!(a.focus, Focus::List);
        a.handle_event(press(KeyCode::Tab));
        assert_eq!(a.focus, Focus::Preview);
        a.handle_event(press(KeyCode::Tab));
        assert_eq!(a.focus, Focus::List);
        // Shift+Tab (BackTab) toggles the same way.
        a.handle_event(press(KeyCode::BackTab));
        assert_eq!(a.focus, Focus::Preview);
        a.handle_event(press(KeyCode::BackTab));
        assert_eq!(a.focus, Focus::List);
    }

    #[test]
    fn list_focus_moves_selection_preview_focus_scrolls_preview() {
        let mut a = app();
        set_results(&mut a, results_n(5));
        // Load a tall preview so there's something to scroll.
        let _ = preview_tick(&mut a); // dirty from SearchComplete → issues a load
        let body = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 1,
            result: Ok(preview_view(1, &body)),
        });
        a.sync_preview_viewport(10);

        // List focused (default): j/k move the selection, preview unscrolled.
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.selected, 1);
        assert_eq!(a.preview_scroll, 0);

        // Move back to id=1 and let its preview reload+settle so the body is tall.
        a.handle_event(press(KeyCode::Char('k')));
        assert_eq!(a.selected, 0);
        let _ = preview_tick(&mut a);
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 1,
            result: Ok(preview_view(1, &body)),
        });
        a.sync_preview_viewport(10);

        // Focus the preview: now j/k scroll its body; the selection holds still.
        a.handle_event(press(KeyCode::Tab));
        assert_eq!(a.focus, Focus::Preview);
        let sel = a.selected;
        a.handle_event(press(KeyCode::Char('j')));
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.preview_scroll, 2);
        assert_eq!(
            a.selected, sel,
            "preview scroll must not move the selection"
        );
        // g/G jump to top/bottom of the preview body.
        a.handle_event(press(KeyCode::Char('G')));
        assert!(a.preview_scroll > 0);
        a.handle_event(press(KeyCode::Char('g')));
        assert_eq!(a.preview_scroll, 0);
    }

    #[test]
    fn preview_loads_only_on_settle_not_per_keystroke() {
        // A burst of cursor movement must NOT fire a preview load per key — only
        // the debounce tick flushes a single load for the settled selection.
        let mut a = app();
        set_results(&mut a, results_n(10));
        // Each j returns no command (no per-keystroke fetch).
        for _ in 0..5 {
            let cmds = a.handle_event(press(KeyCode::Char('j')));
            assert!(cmds.is_empty(), "movement must not issue a network command");
        }
        assert_eq!(a.selected, 5);
        // The tick coalesces the whole burst into one load for the settled row.
        let cmds = preview_tick(&mut a);
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadPreview {
                kind: ObjectKind::Device,
                id: 6
            }]
        ));
        // A second tick with the cursor unmoved issues nothing (idempotent).
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 6,
            result: Ok(preview_view(6, "body")),
        });
        assert!(preview_tick(&mut a).is_empty());
    }

    #[test]
    fn stale_preview_response_is_dropped_matching_one_is_shown() {
        let mut a = app();
        set_results(&mut a, results_n(3)); // ids 1,2,3; selected = 0 (id 1)
        let _ = preview_tick(&mut a); // issues LoadPreview for id 1

        // A stale response for a *different* selection (id 2) must be ignored.
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 2,
            result: Ok(preview_view(2, "STALE body for id 2")),
        });
        assert_eq!(a.preview_for, None, "stale response must not be adopted");
        assert!(a.preview.is_none());

        // The matching response (id 1) is shown.
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 1,
            result: Ok(preview_view(1, "FRESH body for id 1")),
        });
        assert_eq!(a.preview_for, Some((ObjectKind::Device, 1)));
        assert_eq!(a.preview_body(), "FRESH body for id 1");
    }

    #[test]
    fn preview_body_falls_back_to_lightweight_peek_when_cursor_moves() {
        // After loading id 1's preview, moving to id 2 must not show id 1's body;
        // the pane falls back to the lightweight peek for the new row.
        let mut a = app();
        set_results(&mut a, results_n(3));
        let _ = preview_tick(&mut a);
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 1,
            result: Ok(preview_view(1, "full id 1 body")),
        });
        assert_eq!(a.preview_body(), "full id 1 body");
        a.handle_event(press(KeyCode::Char('j'))); // now on id 2
        assert_ne!(a.preview_body(), "full id 1 body");
        assert!(a.preview_body().contains("dev2")); // lightweight peek of the row
    }

    #[test]
    fn empty_results_focus_and_movement_are_safe() {
        let mut a = app();
        // No results, no recents. Tab focus, scroll-in-preview, and select keys
        // are all harmless no-ops.
        a.handle_event(press(KeyCode::Tab));
        assert_eq!(a.focus, Focus::Preview);
        for key in [
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::Char('G'),
            KeyCode::Char('g'),
            KeyCode::PageDown,
            KeyCode::PageUp,
        ] {
            a.handle_event(press(key));
        }
        assert_eq!(a.preview_scroll, 0);
        assert_eq!(a.selected, 0);
        // A tick with no selectable target clears any preview and issues nothing.
        a.mark_preview_dirty_for_test();
        let cmds = preview_tick(&mut a);
        assert!(cmds.is_empty());
        assert!(a.preview.is_none());
        // The preview body shows a tasteful placeholder, not a panic.
        assert!(a.preview_body().contains("Nothing selected"));
    }

    #[test]
    fn preview_tick_is_inert_off_home_screen() {
        // A pending dirty flag must not fire a preview fetch while reading detail.
        let mut a = app();
        set_results(&mut a, results_n(3));
        detail_loaded(&mut a, Ok(preview_view(1, "detail")));
        assert_eq!(a.screen, Screen::Detail);
        a.mark_preview_dirty_for_test();
        assert!(preview_tick(&mut a).is_empty());
    }

    // --- Bug B: stale full-search / full-detail response suppression ----------

    /// Dispatch a search through the real key path so a request id is stamped,
    /// returning that id (the newest spawned on the search channel).
    fn dispatch_search(a: &mut App, query: &str) -> RequestId {
        a.handle_event(press(KeyCode::Char('/')));
        for c in query.chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        match cmds.as_slice() {
            [AppCommand::Search { req, .. }] => *req,
            other => panic!("expected a Search command, got {other:?}"),
        }
    }

    #[test]
    fn stale_search_complete_is_dropped_newest_wins() {
        // Two searches are spawned; the EARLIER one's result lands LAST. The
        // guard must drop it so the newer search's results aren't clobbered.
        let mut a = app();
        let first = dispatch_search(&mut a, "alpha");
        let second = dispatch_search(&mut a, "beta");
        assert!(second > first, "ids are monotonic per channel");
        assert_eq!(a.pending, 2, "both searches are in flight");

        // The NEWER search resolves first and is adopted.
        a.handle_event(AppEvent::SearchComplete {
            req: second,
            result: Ok(SearchOutcome {
                results: vec![result(2, "beta-hit")],
                errors: Vec::new(),
            }),
        });
        assert_eq!(a.selected_result().map(|r| r.id), Some(2));
        assert_eq!(a.pending, 1, "the second search settled");

        // The STALE earlier search lands afterwards: it must be dropped, leaving
        // the newer results in place — but still counted down (no hung spinner).
        a.handle_event(AppEvent::SearchComplete {
            req: first,
            result: Ok(SearchOutcome {
                results: vec![result(1, "alpha-hit")],
                errors: Vec::new(),
            }),
        });
        assert_eq!(
            a.selected_result().map(|r| r.id),
            Some(2),
            "stale search must not overwrite the newer results"
        );
        assert_eq!(a.pending, 0, "stale result still settles its fetch");
        assert!(!a.loading());
    }

    #[test]
    fn stale_detail_loaded_is_dropped_newest_wins() {
        // Two detail loads are spawned (Enter on two different rows); the EARLIER
        // one's result lands LAST and must not navigate over the newer object.
        let mut a = app();
        set_results(&mut a, vec![result(10, "ten"), result(20, "twenty")]);

        // Enter on row 0 (id 10) → first detail request.
        let first = match a.handle_event(press(KeyCode::Enter)).as_slice() {
            [AppCommand::LoadDetail { id: 10, req, .. }] => *req,
            other => panic!("expected LoadDetail for id 10, got {other:?}"),
        };
        // Go back, move to row 1 (id 20), Enter → second (newer) detail request.
        a.handle_event(press(KeyCode::Char('b')));
        a.handle_event(press(KeyCode::Char('j')));
        let second = match a.handle_event(press(KeyCode::Enter)).as_slice() {
            [AppCommand::LoadDetail { id: 20, req, .. }] => *req,
            other => panic!("expected LoadDetail for id 20, got {other:?}"),
        };
        assert!(second > first);
        assert_eq!(a.pending, 2);

        // The newer load (id 20) resolves first and is shown.
        a.handle_event(AppEvent::DetailLoaded {
            req: second,
            result: Ok(preview_view(20, "twenty body")),
        });
        assert_eq!(a.detail.as_ref().map(|d| d.id), Some(20));

        // The stale earlier load (id 10) lands afterwards: dropped, so it does not
        // navigate/overwrite the newer detail — but still counted down.
        a.handle_event(AppEvent::DetailLoaded {
            req: first,
            result: Ok(preview_view(10, "ten body")),
        });
        assert_eq!(
            a.detail.as_ref().map(|d| d.id),
            Some(20),
            "stale detail must not overwrite the newer object"
        );
        assert_eq!(a.pending, 0);
        assert!(!a.loading());
    }

    #[test]
    fn detail_and_search_channels_are_independent() {
        // The detail guard must not drop a search result (and vice versa): the two
        // channels keep separate high-water marks even off one monotonic source.
        let mut a = app();
        set_results(&mut a, vec![result(5, "five")]);
        // Spawn a detail load (bumps detail_gen, not search_gen).
        let detail_req = match a.handle_event(press(KeyCode::Enter)).as_slice() {
            [AppCommand::LoadDetail { req, .. }] => *req,
            other => panic!("expected LoadDetail, got {other:?}"),
        };
        // A fresh search result (current on the search channel) is still adopted
        // even though detail_gen is now higher than search_gen.
        search_complete(
            &mut a,
            Ok(SearchOutcome {
                results: vec![result(6, "six")],
                errors: Vec::new(),
            }),
        );
        assert_eq!(a.selected_result().map(|r| r.id), Some(6));
        // And the detail load resolves on its own channel.
        a.handle_event(AppEvent::DetailLoaded {
            req: detail_req,
            result: Ok(preview_view(5, "five body")),
        });
        assert_eq!(a.detail.as_ref().map(|d| d.id), Some(5));
    }

    #[test]
    fn refresh_then_stale_prior_search_keeps_refreshed_results() {
        // The recents/refresh path also rides the guard: a manual refresh (`r`)
        // spawns a newer search; a slow result from before the refresh is dropped.
        let mut a = app();
        set_results(&mut a, vec![result(1, "a"), result(2, "b")]);
        a.last_query = Some("edge".into());
        // Pretend a search was in flight before the refresh.
        let stale = a.search_gen; // current high-water; the refresh will exceed it
        // Manual refresh dispatches a newer search (bumps search_gen).
        let cmds = a.handle_event(press(KeyCode::Char('r')));
        assert!(matches!(cmds.as_slice(), [AppCommand::Search { .. }]));
        assert!(a.search_gen > stale);
        // The refreshed results land and are adopted.
        set_results(&mut a, vec![result(3, "fresh")]);
        assert_eq!(a.selected_result().map(|r| r.id), Some(3));
        // A straggler search from before the refresh is dropped.
        a.handle_event(AppEvent::SearchComplete {
            req: stale,
            result: Ok(SearchOutcome {
                results: vec![result(99, "straggler")],
                errors: Vec::new(),
            }),
        });
        assert_eq!(
            a.selected_result().map(|r| r.id),
            Some(3),
            "a pre-refresh straggler must not clobber the refreshed results"
        );
    }

    // --- Bug A: ambiguous palette IP lookup surfaces as an error -------------

    #[test]
    fn ambiguous_detail_error_surfaces_as_error_status() {
        // The palette IP path resolves through the ambiguity-aware resolver; an
        // ambiguous ref returns a typed Ambiguous error on DetailLoaded, which the
        // TUI must surface as an error status (the same path a NotFound takes),
        // never silently navigating to a guessed object.
        let mut a = app();
        let ambiguous = anyhow::Error::from(crate::error::NboxError::Ambiguous {
            noun: "IP address".into(),
            value: "10.0.0.1".into(),
            matches: "10.0.0.1/24 (vrf-a), 10.0.0.1/24 (vrf-b)".into(),
        });
        detail_loaded(&mut a, Err(ambiguous));
        assert_eq!(
            a.screen,
            Screen::Home,
            "ambiguity must not navigate to detail"
        );
        assert!(a.detail.is_none(), "no object is adopted on ambiguity");
        assert_eq!(a.status_severity, Severity::Error);
        assert!(a.status.contains("ambiguous"));
    }

    // --- Item 7: profile switcher -------------------------------------------

    /// Build an `App` whose configured profiles are `names`, with the first as the
    /// active one (matching `App::new`'s "test" → falls back to index 0 unless a
    /// name matches; here the active `profile_name` is set to `names[0]`).
    fn app_with_profiles(names: &[&str]) -> App {
        let mut a = app();
        let profiles: Vec<ProfileEntry> = names
            .iter()
            .map(|n| ProfileEntry {
                name: (*n).to_string(),
                config: ProfileConfig {
                    url: format!("http://{n}.example"),
                    ..Default::default()
                },
            })
            .collect();
        // Point the active profile (name + url) at the first entry so the fixture
        // starts with the header matching the connected instance — the invariant
        // these tests guard. set_profiles then indexes to it.
        if let Some(first) = profiles.first() {
            a.profile_name.clone_from(&first.name);
            a.base_url.clone_from(&first.config.url);
        }
        a.set_profiles(profiles);
        a
    }

    /// Deliver a successful profile-switch result tagged as the current pending
    /// switch (passes the latest-switch-wins guard), with the given version.
    fn profile_switched_ok(a: &mut App, version: &str) {
        let name = a.pending_profile.clone().expect("a switch must be pending");
        let profile = ProfileConfig {
            url: "http://x".into(),
            ..Default::default()
        };
        let client = NetBoxClient::new(&profile, None).unwrap();
        a.handle_event(AppEvent::ProfileSwitched {
            name,
            result: Ok((client, version.to_string())),
        });
    }

    #[test]
    fn cycle_profile_advances_and_wraps_forward() {
        let mut a = app_with_profiles(&["alpha", "beta", "gamma"]);
        assert_eq!(a.profile_index, 0);
        assert_eq!(a.profile_name, "alpha");

        // P dispatches a reconnect to the next profile but does NOT flip the
        // header — that waits for success, so the UI never lies about the client.
        let cmds = a.handle_event(press(KeyCode::Char('P')));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "beta"
        ));
        assert_eq!(a.profile_index, 0, "header stays on the connected profile");
        assert_eq!(a.profile_name, "alpha", "header doesn't flip until success");
        assert_eq!(a.pending_profile.as_deref(), Some("beta"));
        assert!(a.loading(), "a reconnect is a tracked fetch");

        // The switch settles → the header flips atomically to the new profile.
        profile_switched_ok(&mut a, "4.5.0");
        assert_eq!(a.profile_index, 1);
        assert_eq!(a.profile_name, "beta");
        assert_eq!(a.base_url, "http://beta.example");

        // Continue forward to the last, then wrap to the first (each settles).
        a.handle_event(press(KeyCode::Char('P')));
        profile_switched_ok(&mut a, "4.5.0");
        assert_eq!(a.profile_name, "gamma");
        let cmds = a.handle_event(press(KeyCode::Char('P')));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "alpha"
        ));
        profile_switched_ok(&mut a, "4.5.0");
        assert_eq!(a.profile_index, 0, "wraps past the end to the first");
        assert_eq!(a.profile_name, "alpha");
    }

    #[test]
    fn cycle_profile_backward_wraps_to_last() {
        let mut a = app_with_profiles(&["alpha", "beta", "gamma"]);
        // Ctrl+P steps backward; from the first it wraps to the last.
        let cmds = a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        )));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "gamma"
        ));
        assert_eq!(a.pending_profile.as_deref(), Some("gamma"));
        // Still on the connected profile until the reconnect succeeds.
        assert_eq!(a.profile_index, 0);
        assert_eq!(a.profile_name, "alpha");
        profile_switched_ok(&mut a, "4.5.0");
        assert_eq!(a.profile_index, 2);
        assert_eq!(a.profile_name, "gamma");
    }

    #[test]
    fn cycle_profile_steps_from_pending_target_when_switch_in_flight() {
        // Rapid cycling before the first reconnect settles must advance from the
        // pending target, not the (still-connected) profile, so the user's sense
        // of "where am I cycling" matches.
        let mut a = app_with_profiles(&["alpha", "beta", "gamma"]);
        a.handle_event(press(KeyCode::Char('P'))); // → beta (pending)
        assert_eq!(a.pending_profile.as_deref(), Some("beta"));
        assert_eq!(a.profile_name, "alpha", "header still on alpha");
        let cmds = a.handle_event(press(KeyCode::Char('P'))); // step from beta → gamma
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "gamma"
        ));
        assert_eq!(a.pending_profile.as_deref(), Some("gamma"));
    }

    #[test]
    fn cycle_profile_single_profile_is_a_graceful_noop() {
        let mut a = app_with_profiles(&["only"]);
        let cmds = a.handle_event(press(KeyCode::Char('P')));
        assert!(cmds.is_empty(), "nothing to switch to");
        assert_eq!(a.profile_name, "only");
        assert_eq!(a.profile_index, 0);
        assert_eq!(a.status, "only one profile configured");
        assert!(!a.loading(), "a no-op switch is not a tracked fetch");
    }

    #[test]
    fn cycle_profile_with_no_profiles_is_a_noop() {
        // An app whose profile list was never populated never panics on the key.
        let mut a = app();
        assert!(a.profiles.is_empty());
        let cmds = a.handle_event(press(KeyCode::Char('P')));
        assert!(cmds.is_empty());
        assert_eq!(a.status, "only one profile configured");
    }

    #[test]
    fn palette_profile_verb_jumps_to_named_profile() {
        let mut a = app_with_profiles(&["alpha", "beta", "gamma"]);
        a.handle_event(press(KeyCode::Char(':')));
        for c in "profile gamma".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "gamma"
        ));
        // Header waits for the reconnect; the switch is pending meanwhile.
        assert_eq!(a.pending_profile.as_deref(), Some("gamma"));
        assert_eq!(a.profile_name, "alpha");
        profile_switched_ok(&mut a, "4.5.0");
        assert_eq!(a.profile_index, 2);
        assert_eq!(a.profile_name, "gamma");
    }

    #[test]
    fn palette_profile_unknown_name_is_a_clear_error() {
        let mut a = app_with_profiles(&["alpha", "beta"]);
        a.handle_event(press(KeyCode::Char(':')));
        for c in "profile nope".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(cmds.is_empty(), "an unknown profile dispatches nothing");
        assert_eq!(a.profile_name, "alpha", "active profile is unchanged");
        assert_eq!(a.profile_index, 0);
        assert_eq!(a.status_severity, Severity::Error);
        assert!(a.status.contains("no profile named 'nope'"));
    }

    #[test]
    fn profile_switched_ok_updates_version_when_current() {
        let mut a = app_with_profiles(&["alpha", "beta"]);
        a.handle_event(press(KeyCode::Char('P'))); // switching to beta
        assert!(a.loading());
        assert_eq!(a.pending_profile.as_deref(), Some("beta"));
        // Until it settles the header stays on the connected profile (alpha).
        assert_eq!(a.profile_name, "alpha");
        assert_eq!(a.base_url, "http://alpha.example");
        profile_switched_ok(&mut a, "4.7.0");
        // On success the whole header flips atomically to match the new client.
        assert_eq!(a.netbox_version, "4.7.0");
        assert_eq!(a.profile_name, "beta");
        assert_eq!(a.profile_index, 1);
        assert_eq!(a.base_url, "http://beta.example");
        assert!(a.pending_profile.is_none(), "the switch settled");
        assert!(!a.loading());
        assert_eq!(a.status_severity, Severity::Success);
        assert!(a.status.contains("beta"));
    }

    #[test]
    fn profile_switch_failure_leaves_no_phantom_and_keeps_ui_usable() {
        // The invariant under test: a FAILED switch is a no-op + error toast. The
        // header must still match the instance `client` is connected to (alpha),
        // never the requested-but-unreachable one — no phantom.
        let mut a = app_with_profiles(&["alpha", "beta"]);
        a.handle_event(press(KeyCode::Char('P')));
        assert_eq!(a.pending_profile.as_deref(), Some("beta"));
        let name = a.pending_profile.clone().unwrap();
        a.handle_event(AppEvent::ProfileSwitched {
            name,
            result: Err(anyhow::anyhow!("connection refused")),
        });
        // Header + url + index + version all stay on the still-connected alpha.
        assert_eq!(a.profile_name, "alpha", "header stays on the connected one");
        assert_eq!(a.profile_index, 0);
        assert_eq!(a.base_url, "http://alpha.example");
        assert_eq!(
            a.netbox_version, "4.5.5",
            "unchanged: the old client is intact"
        );
        assert!(
            a.pending_profile.is_none(),
            "the switch is no longer pending"
        );
        assert_eq!(a.status_severity, Severity::Error);
        assert!(a.status.contains("connection refused"));
        assert!(!a.loading(), "a failed switch still settles the fetch");
        // The UI is usable again on the old instance: fetches are no longer fenced.
        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(
            matches!(cmds.as_slice(), [AppCommand::Search { query, .. }] if query == "edge"),
            "a search dispatches normally after a failed switch"
        );
    }

    #[test]
    fn switching_profile_clears_old_instance_data() {
        // Old-profile results/recents/detail must not linger after a switch — they
        // reference the instance we're leaving.
        let mut a = app_with_profiles(&["alpha", "beta"]);
        set_results(&mut a, results_n(3));
        detail_loaded(&mut a, Ok(preview_view(1, "alpha body")));
        a.handle_event(press(KeyCode::Char('b'))); // back to home
        assert!(!a.results.is_empty());
        assert!(!a.recent.is_empty());

        a.handle_event(press(KeyCode::Char('P'))); // switch to beta (pending)
        // The old instance's data is still shown while the switch is in flight (we
        // remain connected to it); it's dropped atomically only once the new
        // instance is adopted, so a failed switch wouldn't have wiped a live UI.
        assert!(
            !a.results.is_empty(),
            "old data kept until the switch succeeds"
        );
        profile_switched_ok(&mut a, "4.5.0");
        assert!(a.results.is_empty(), "old results dropped on success");
        assert!(a.view.is_empty());
        assert!(a.recent.is_empty(), "old recents dropped");
        assert!(a.detail.is_none(), "old detail dropped");
        assert!(a.last_query.is_none());
        assert_eq!(a.screen, Screen::Home);
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn switching_profile_drops_stale_old_profile_search_response() {
        // A search dispatched against the OLD profile is still in flight when the
        // switch SUCCEEDS. After the switch lands, that old-instance result must be
        // dropped (the success path bumps the search high-water mark past it), not
        // painted onto the new instance.
        let mut a = app_with_profiles(&["alpha", "beta"]);
        let stale = dispatch_search(&mut a, "edge"); // in flight on alpha
        assert!(a.loading());

        // Switch to beta and let it succeed: success bumps the gen past `stale`.
        a.handle_event(press(KeyCode::Char('P')));
        profile_switched_ok(&mut a, "4.5.0");
        assert!(a.search_gen > stale, "success advanced the search gen");

        // The old-profile search finally lands — it must be dropped as stale.
        a.handle_event(AppEvent::SearchComplete {
            req: stale,
            result: Ok(SearchOutcome {
                results: vec![result(99, "alpha-straggler")],
                errors: Vec::new(),
            }),
        });
        assert!(
            a.results.is_empty(),
            "a stale old-profile search must not populate the new instance"
        );
    }

    #[test]
    fn switching_profile_drops_stale_old_profile_detail_response() {
        // Same for a full-detail load in flight on the old profile across a switch.
        let mut a = app_with_profiles(&["alpha", "beta"]);
        set_results(&mut a, vec![result(10, "ten")]);
        let stale = match a.handle_event(press(KeyCode::Enter)).as_slice() {
            [AppCommand::LoadDetail { req, .. }] => *req,
            other => panic!("expected LoadDetail, got {other:?}"),
        };
        a.handle_event(press(KeyCode::Char('P'))); // switch to beta
        profile_switched_ok(&mut a, "4.5.0"); // success bumps the detail gen
        assert!(a.detail_gen > stale);
        a.handle_event(AppEvent::DetailLoaded {
            req: stale,
            result: Ok(preview_view(10, "alpha detail")),
        });
        assert!(
            a.detail.is_none(),
            "a stale old-profile detail must not navigate the new instance"
        );
        assert_eq!(a.screen, Screen::Home);
    }

    #[test]
    fn fetches_are_fenced_while_a_switch_is_in_flight() {
        // While a switch is pending the old client must not be queried: a search
        // submit / Enter / refresh / palette fetch is suppressed (not dispatched),
        // with a brief status, until the switch settles.
        let mut a = app_with_profiles(&["alpha", "beta"]);
        set_results(&mut a, vec![result(1, "edge01")]);
        a.handle_event(press(KeyCode::Char('P'))); // switch to beta (pending)
        assert!(a.switch_in_flight());

        // A search submit is fenced.
        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(cmds.is_empty(), "search is fenced mid-switch");
        assert!(a.status.contains("switching profile"));

        // Enter to open a detail is fenced.
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(cmds.is_empty(), "open-detail is fenced mid-switch");

        // Manual refresh (r) is fenced.
        let cmds = a.handle_event(press(KeyCode::Char('r')));
        assert!(cmds.is_empty(), "refresh is fenced mid-switch");

        // The background preview/auto-refresh ticks are fenced silently.
        a.mark_preview_dirty_for_test();
        assert!(
            a.handle_event(AppEvent::PreviewTick).is_empty(),
            "preview tick is fenced mid-switch"
        );
        a.last_query = Some("edge".into());
        assert!(
            a.handle_event(AppEvent::Tick).is_empty(),
            "auto-refresh tick is fenced mid-switch"
        );

        // After the switch settles, fetches flow again.
        profile_switched_ok(&mut a, "4.5.0");
        assert!(!a.switch_in_flight());
        a.handle_event(press(KeyCode::Char('/')));
        for c in "core".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(
            matches!(cmds.as_slice(), [AppCommand::Search { query, .. }] if query == "core"),
            "searches dispatch again once the switch settles"
        );
    }

    #[test]
    fn superseded_profile_switch_result_is_dropped() {
        // Cycling twice quickly: the first switch's reconnect returns LAST. It must
        // be dropped (latest-switch-wins) so it can't overwrite the second one's
        // client/version with a superseded profile's.
        let mut a = app_with_profiles(&["alpha", "beta", "gamma"]);
        a.handle_event(press(KeyCode::Char('P'))); // → beta
        assert_eq!(a.pending_profile.as_deref(), Some("beta"));
        a.handle_event(press(KeyCode::Char('P'))); // → gamma (supersedes beta)
        assert_eq!(a.pending_profile.as_deref(), Some("gamma"));
        assert_eq!(a.pending, 2, "both reconnects are in flight");

        // The newer (gamma) switch resolves first and is adopted.
        let client = NetBoxClient::new(
            &ProfileConfig {
                url: "http://x".into(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        a.handle_event(AppEvent::ProfileSwitched {
            name: "gamma".into(),
            result: Ok((client.clone(), "4.9.0".into())),
        });
        assert_eq!(a.netbox_version, "4.9.0");
        assert!(a.pending_profile.is_none());

        // The stale beta switch lands afterwards: dropped, leaving gamma in place.
        a.handle_event(AppEvent::ProfileSwitched {
            name: "beta".into(),
            result: Ok((client, "4.4.0".into())),
        });
        assert_eq!(
            a.netbox_version, "4.9.0",
            "a superseded switch must not overwrite the current version"
        );
        assert_eq!(a.profile_name, "gamma");
        assert_eq!(a.pending, 0, "stale switch still settles its fetch");
    }

    #[test]
    fn set_profiles_indexes_the_active_profile() {
        // When the active profile isn't the first entry, set_profiles points the
        // index at it so the first cycle steps from the right place.
        let mut a = app();
        a.profile_name = "beta".into();
        a.set_profiles(vec![
            ProfileEntry {
                name: "alpha".into(),
                config: ProfileConfig::default(),
            },
            ProfileEntry {
                name: "beta".into(),
                config: ProfileConfig::default(),
            },
            ProfileEntry {
                name: "gamma".into(),
                config: ProfileConfig::default(),
            },
        ]);
        assert_eq!(a.profile_index, 1, "indexed to the active profile");
        // Forward from beta targets gamma; the header flips once it succeeds.
        let cmds = a.handle_event(press(KeyCode::Char('P')));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "gamma"
        ));
        profile_switched_ok(&mut a, "4.5.0");
        assert_eq!(a.profile_name, "gamma");
    }
}
