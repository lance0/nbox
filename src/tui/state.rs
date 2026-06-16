//! TUI application state and (pure) input handling.
//!
//! `handle_event`/`handle_key` mutate state and return the commands to run —
//! they perform no I/O, so they're unit-testable without a terminal. Network
//! work happens in spawned tasks (see `tui::app`), never in the render loop.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::domain::detail::DetailView;
use crate::netbox::client::NetBoxClient;
use crate::netbox::search::{ObjectKind, SearchOutcome, SearchResult};
use crate::tui::palette::{self, PaletteCommand};
use crate::tui::theme::Theme;

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

/// Events delivered to the event loop.
pub enum AppEvent {
    Key(KeyEvent),
    Resize(u16, u16),
    Tick,
    SearchComplete(anyhow::Result<SearchOutcome>),
    DetailLoaded(anyhow::Result<DetailView>),
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

/// Side-effecting work the loop should spawn off the render thread.
#[derive(Debug, Clone)]
pub enum AppCommand {
    Search(String),
    LoadDetail { kind: ObjectKind, id: u64 },
    LoadByRef { kind: ObjectKind, value: String },
    OpenBrowser(String),
    Copy(String),
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
    pub history: Vec<Screen>,
    pub status: String,

    pub search_input: String,
    pub command_input: String,
    pub last_query: Option<String>,

    pub results: Vec<SearchResult>,
    /// Indices into `results` in display order (fuzzy-filtered while searching).
    pub view: Vec<usize>,
    pub selected: usize,
    /// On the next `SearchComplete`, try to re-select this (kind, id) — used to
    /// keep the cursor stable across an auto-refresh.
    pub pending_reselect: Option<(ObjectKind, u64)>,
    pub recent: Vec<RecentItem>,
    pub detail: Option<DetailView>,
    /// Active detail tab: 0 = summary, n>0 = `detail.tabs[n-1]`.
    pub detail_tab: usize,
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
            history: Vec::new(),
            status: String::new(),
            search_input: String::new(),
            command_input: String::new(),
            last_query: None,
            results: Vec::new(),
            view: Vec::new(),
            selected: 0,
            pending_reselect: None,
            recent: Vec::new(),
            detail: None,
            detail_tab: 0,
            should_quit: false,
        }
    }

    /// Apply an event, returning any commands to dispatch.
    pub fn handle_event(&mut self, event: AppEvent) -> Vec<AppCommand> {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::Resize(_, _) => Vec::new(),
            AppEvent::Tick => self.on_tick(),
            AppEvent::SearchComplete(result) => {
                match result {
                    Ok(outcome) => {
                        let n = outcome.results.len();
                        self.status = if outcome.errors.is_empty() {
                            format!("{n} result(s)")
                        } else {
                            format!(
                                "{n} result(s) (partial: {} endpoint(s) failed)",
                                outcome.errors.len()
                            )
                        };
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
                    }
                    Err(e) => self.status = format!("error: {e:#}"),
                }
                Vec::new()
            }
            AppEvent::DetailLoaded(result) => {
                match result {
                    Ok(view) => {
                        self.record_recent(view.kind, view.id, view.title.clone());
                        self.navigate_to(Screen::Detail);
                        self.detail = Some(view);
                        self.detail_tab = 0;
                        self.status.clear();
                    }
                    Err(e) => self.status = format!("error: {e:#}"),
                }
                Vec::new()
            }
            AppEvent::Status(message) => {
                self.status = message;
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
                self.search_input.clear();
                self.refilter();
            }
            KeyCode::Char(':') => {
                self.mode = Mode::Command;
                self.command_input.clear();
            }
            KeyCode::Char('t') => self.cycle_theme(),
            KeyCode::Char('j') | KeyCode::Down => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.select_prev(),
            KeyCode::Char('g') => self.selected = 0,
            KeyCode::Char('G') => self.selected = self.home_len().saturating_sub(1),
            KeyCode::Enter => {
                if self.screen == Screen::Home
                    && let Some((kind, id)) = self.home_target()
                {
                    self.status = "loading…".into();
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
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let query = self.search_input.trim().to_string();
                self.mode = Mode::Normal;
                if !query.is_empty() {
                    self.last_query = Some(query.clone());
                    self.status = format!("searching {query}…");
                    return vec![AppCommand::Search(query)];
                }
            }
            KeyCode::Char('u') if ctrl => {
                self.search_input.clear();
                self.refilter();
            }
            KeyCode::Char('w') if ctrl => {
                trim_last_word(&mut self.search_input);
                self.refilter();
            }
            KeyCode::Backspace => {
                self.search_input.pop();
                self.refilter();
            }
            KeyCode::Char(c) => {
                self.search_input.push(c);
                self.refilter();
            }
            _ => {}
        }
        Vec::new()
    }

    fn handle_command_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let input = self.command_input.trim().to_string();
                self.mode = Mode::Normal;
                if !input.is_empty() {
                    match palette::parse(&input) {
                        Ok(cmd) => return self.apply_palette(cmd),
                        Err(e) => self.status = e,
                    }
                }
            }
            KeyCode::Backspace => {
                self.command_input.pop();
            }
            KeyCode::Char(c) => self.command_input.push(c),
            _ => {}
        }
        Vec::new()
    }

    /// Map a parsed palette command onto state changes / commands.
    fn apply_palette(&mut self, cmd: PaletteCommand) -> Vec<AppCommand> {
        match cmd {
            PaletteCommand::Lookup { kind, value } => {
                self.status = format!("loading {value}…");
                vec![AppCommand::LoadByRef { kind, value }]
            }
            PaletteCommand::Search(query) => {
                self.last_query = Some(query.clone());
                self.status = format!("searching {query}…");
                vec![AppCommand::Search(query)]
            }
            PaletteCommand::Open => match self.selected_result() {
                Some(r) => vec![AppCommand::OpenBrowser(r.url.clone())],
                None => {
                    self.status = "no selection".into();
                    Vec::new()
                }
            },
            PaletteCommand::Copy => match self.selected_result() {
                Some(r) => vec![AppCommand::Copy(r.display.clone())],
                None => {
                    self.status = "no selection".into();
                    Vec::new()
                }
            },
            PaletteCommand::Theme(name) => {
                self.set_theme_by_name(&name);
                Vec::new()
            }
            PaletteCommand::Refresh => match self.last_query.clone() {
                Some(query) => {
                    self.status = format!("refreshing {query}…");
                    vec![AppCommand::Search(query)]
                }
                None => {
                    self.status = "nothing to refresh".into();
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
        self.status = format!("theme: {}", list[self.theme_index]);
    }

    fn set_theme_by_name(&mut self, name: &str) {
        self.theme = Theme::by_name(name);
        self.theme_index = Theme::index_of(name);
        self.status = format!("theme: {}", self.theme.name());
    }

    /// Recompute the visible `view` by fuzzy-filtering results on `search_input`.
    fn refilter(&mut self) {
        let displays: Vec<&str> = self.results.iter().map(|r| r.display.as_str()).collect();
        self.view = crate::tui::fuzzy::rank(&self.search_input, &displays);
        self.selected = 0;
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
        }
    }

    /// The body text for the active detail tab (summary when `detail_tab` is 0).
    pub fn detail_body(&self) -> &str {
        match &self.detail {
            Some(d) if self.detail_tab > 0 => d
                .tabs
                .get(self.detail_tab - 1)
                .map(|t| t.body.as_str())
                .unwrap_or(d.body.as_str()),
            Some(d) => d.body.as_str(),
            None => "loading…",
        }
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
        }
    }

    fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
}

/// Delete the trailing word (and its preceding spaces) from `s`.
fn trim_last_word(s: &mut String) {
    let trimmed = s.trim_end_matches(' ');
    let cut = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
    s.truncate(cut);
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
        let mut a = app();
        a.mode = Mode::Search;
        a.search_input = "edge router".into();
        a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(a.search_input, "edge ");
    }

    #[test]
    fn theme_cycles() {
        let mut a = app();
        let before = a.theme_index;
        a.handle_event(press(KeyCode::Char('t')));
        assert_ne!(a.theme_index, before);
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
}
