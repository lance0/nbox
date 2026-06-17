//! TUI application state and (pure) input handling.
//!
//! `handle_event`/`handle_key` mutate state and return the commands to run —
//! they perform no I/O, so they're unit-testable without a terminal. Network
//! work happens in spawned tasks (see `tui::app`), never in the render loop.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

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
    Help,
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
    SearchComplete(anyhow::Result<SearchOutcome>),
    DetailLoaded(anyhow::Result<DetailView>),
    /// A preview-pane detail load, tagged with the (kind, id) it was issued for
    /// so a stale response (the selection moved on) can be dropped.
    PreviewLoaded {
        kind: ObjectKind,
        id: u64,
        result: anyhow::Result<DetailView>,
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

/// Most-recent-first cap for the recents list.
const RECENT_CAP: usize = 20;

/// Pre-render fallback for the results-list PgUp/PgDn step, used before the first
/// render has stashed a live viewport height (see [`App::list_page`]). Once a
/// frame has been drawn the list pages by ~one screenful instead, matching the
/// detail/preview panes.
const PAGE_JUMP_FALLBACK: usize = 10;

/// Side-effecting work the loop should spawn off the render thread.
#[derive(Debug, Clone)]
pub enum AppCommand {
    Search(String),
    LoadDetail {
        kind: ObjectKind,
        id: u64,
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
    },
    OpenBrowser(String),
    Copy(String),
}

impl AppCommand {
    /// True when this command kicks off a network fetch whose result returns as
    /// an `AppEvent` (Search/LoadDetail/LoadPreview/LoadByRef). These bump the
    /// in-flight counter so the footer spinner runs until the matching result
    /// lands. `OpenBrowser`/`Copy` are fire-and-forget side effects (their async
    /// `Status` push isn't a tracked fetch), so they don't count as loading.
    fn is_fetch(&self) -> bool {
        matches!(
            self,
            AppCommand::Search(_)
                | AppCommand::LoadDetail { .. }
                | AppCommand::LoadPreview { .. }
                | AppCommand::LoadByRef { .. }
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

    pub mode: Mode,
    pub screen: Screen,
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
            mode: Mode::Normal,
            screen: Screen::Home,
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

    /// Apply an event, returning any commands to dispatch. The commands handed
    /// back are accounted into the in-flight counter (each fetch bumps it) so the
    /// footer spinner runs until the matching result event lands.
    pub fn handle_event(&mut self, event: AppEvent) -> Vec<AppCommand> {
        let commands = self.dispatch_event(event);
        self.track_dispatched(&commands);
        commands
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
            AppEvent::SearchComplete(result) => {
                // A search result settles its in-flight fetch (clean or error).
                self.end_request();
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
            AppEvent::DetailLoaded(result) => {
                // The full-detail load (from Enter / a palette lookup) settled.
                self.end_request();
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
        match key.code {
            KeyCode::Char('c') if ctrl => self.should_quit = true,
            KeyCode::Char('q') => {
                if self.screen == Screen::Home {
                    self.should_quit = true;
                } else {
                    self.go_back();
                }
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                if self.screen == Screen::Help {
                    self.go_back();
                } else {
                    self.navigate_to(Screen::Help);
                }
            }
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
                    self.set_status("loading…", Severity::Info);
                    return vec![AppCommand::LoadDetail { kind, id }];
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
                    self.last_query = Some(query.clone());
                    self.set_status(format!("searching {query}…"), Severity::Info);
                    return vec![AppCommand::Search(query)];
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
        match cmd {
            PaletteCommand::Lookup { kind, value } => {
                self.set_status(format!("loading {value}…"), Severity::Info);
                vec![AppCommand::LoadByRef { kind, value }]
            }
            PaletteCommand::Search(query) => {
                self.last_query = Some(query.clone());
                self.set_status(format!("searching {query}…"), Severity::Info);
                vec![AppCommand::Search(query)]
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
            PaletteCommand::Refresh => match self.last_query.clone() {
                Some(query) => {
                    self.set_status(format!("refreshing {query}…"), Severity::Info);
                    vec![AppCommand::Search(query)]
                }
                None => {
                    self.set_status("nothing to refresh", Severity::Warning);
                    Vec::new()
                }
            },
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
        // a search/command (a fuzzy filter marks dirty; let the cursor settle).
        if self.screen != Screen::Home || self.mode != Mode::Normal || !self.preview_dirty {
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
        if self.mode == Mode::Normal
            && self.screen == Screen::Home
            && let Some(query) = self.last_query.clone()
        {
            self.pending_reselect = self.selected_result().map(|r| (r.kind, r.id));
            return vec![AppCommand::Search(query)];
        }
        Vec::new()
    }

    /// Advance to the next built-in theme.
    pub fn cycle_theme(&mut self) {
        let list = Theme::list();
        self.theme_index = (self.theme_index + 1) % list.len();
        self.theme = Theme::by_name(list[self.theme_index]);
        self.set_status(format!("theme: {}", list[self.theme_index]), Severity::Info);
    }

    fn set_theme_by_name(&mut self, name: &str) {
        self.theme = Theme::by_name(name);
        self.theme_index = Theme::index_of(name);
        self.set_status(format!("theme: {}", self.theme.name()), Severity::Info);
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
    fn home_len(&self) -> usize {
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

    fn set_results(a: &mut App, items: Vec<SearchResult>) {
        a.handle_event(AppEvent::SearchComplete(Ok(SearchOutcome {
            results: items,
            errors: Vec::new(),
        })));
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
        a.handle_event(AppEvent::SearchComplete(Ok(SearchOutcome {
            results: vec![result(1, "edge01")],
            errors: vec!["dcim/devices: 500".into()],
        })));
        assert_eq!(a.status_severity, Severity::Warning);
        assert!(a.status.contains("partial"));
    }

    #[test]
    fn request_error_sets_error_severity() {
        let mut a = app();
        a.handle_event(AppEvent::SearchComplete(Err(anyhow::anyhow!(
            "403 forbidden"
        ))));
        assert_eq!(a.status_severity, Severity::Error);
        a.handle_event(AppEvent::DetailLoaded(Err(anyhow::anyhow!(
            "404 not found"
        ))));
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
        assert!(matches!(cmds.as_slice(), [AppCommand::Search(_)]));
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
        a.handle_event(AppEvent::DetailLoaded(Ok(preview_view(1, "body"))));
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
        a.handle_event(AppEvent::DetailLoaded(Err(anyhow::anyhow!("404"))));
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
        assert!(matches!(refresh.as_slice(), [AppCommand::Search(_)]));
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
    fn help_toggles_and_q_closes_it_without_quitting() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('?')));
        assert_eq!(a.screen, Screen::Help);
        a.handle_event(press(KeyCode::Char('q')));
        assert_eq!(a.screen, Screen::Home);
        assert!(!a.should_quit);
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
        assert!(matches!(cmds.as_slice(), [AppCommand::Search(q)] if q == "edge01"));
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
        assert!(matches!(cmds.as_slice(), [AppCommand::Search(q)] if q == "xedge1"));
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
            [AppCommand::LoadByRef { kind: ObjectKind::Device, value }] if value == "edge01"
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
                id: 1
            }]
        ));

        a.handle_event(AppEvent::DetailLoaded(Ok(DetailView {
            kind: ObjectKind::Device,
            id: 1,
            title: "device edge01".into(),
            body: "name: edge01".into(),
            tabs: Vec::new(),
        })));
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
                [AppCommand::LoadDetail { kind: k, id }] => {
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
        a.handle_event(AppEvent::DetailLoaded(Ok(DetailView {
            kind: ObjectKind::Asn,
            id: 3,
            title: "asn 64500".into(),
            body: "asn: 64500\nrir: ARIN".into(),
            tabs: Vec::new(),
        })));
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
            a.handle_event(AppEvent::DetailLoaded(Ok(DetailView {
                kind: ObjectKind::Device,
                id,
                title: title.into(),
                body: String::new(),
                tabs: Vec::new(),
            })));
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
            [AppCommand::LoadByRef { kind: ObjectKind::Device, value }] if value == "edge01"
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
    fn tick_refreshes_last_query_only_when_idle_on_home() {
        let mut a = app();
        // No last query → tick does nothing.
        assert!(a.handle_event(AppEvent::Tick).is_empty());

        a.last_query = Some("edge".into());
        let cmds = a.handle_event(AppEvent::Tick);
        assert!(matches!(cmds.as_slice(), [AppCommand::Search(q)] if q == "edge"));

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
        a.handle_event(AppEvent::DetailLoaded(Ok(DetailView {
            kind: ObjectKind::Device,
            id: 1,
            title: "device edge01".into(),
            body,
            tabs: Vec::new(),
        })));
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
        a.handle_event(AppEvent::DetailLoaded(Ok(DetailView {
            kind: ObjectKind::Device,
            id: 1,
            title: "device edge01".into(),
            body: String::new(),
            tabs: Vec::new(),
        })));
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
        a.handle_event(AppEvent::DetailLoaded(Ok(DetailView {
            kind: ObjectKind::Device,
            id: 1,
            title: "t".into(),
            body: "a\nb\nc".into(),
            tabs: Vec::new(),
        })));
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
        a.handle_event(AppEvent::DetailLoaded(Ok(DetailView {
            kind: ObjectKind::Device,
            id: 2,
            title: "device edge02".into(),
            body: "fresh".into(),
            tabs: Vec::new(),
        })));
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
        a.handle_event(AppEvent::DetailLoaded(Ok(DetailView {
            kind: ObjectKind::Device,
            id: 1,
            title: "device edge01".into(),
            body: long("summary"),
            tabs: vec![DetailTab {
                key: 'i',
                label: "interfaces".into(),
                body: long("iface"),
            }],
        })));
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
        a.handle_event(AppEvent::DetailLoaded(Ok(DetailView {
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
        })));
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
        a.handle_event(AppEvent::DetailLoaded(Ok(DetailView {
            kind: ObjectKind::Device,
            id: 1,
            title: "device edge01".into(),
            body: "summary".into(),
            tabs: vec![tab('i', "interfaces"), tab('p', "ips"), tab('v', "vlans")],
        })));
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
        a.handle_event(AppEvent::DetailLoaded(Ok(preview_view(1, "detail"))));
        assert_eq!(a.screen, Screen::Detail);
        a.mark_preview_dirty_for_test();
        assert!(preview_tick(&mut a).is_empty());
    }
}
