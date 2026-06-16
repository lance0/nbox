//! TUI application state and (pure) input handling.
//!
//! `handle_event`/`handle_key` mutate state and return the commands to run —
//! they perform no I/O, so they're unit-testable without a terminal. Network
//! work happens in spawned tasks (see `tui::app`), never in the render loop.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::domain::detail::DetailView;
use crate::netbox::client::NetBoxClient;
use crate::netbox::search::{ObjectKind, SearchResult};
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
    SearchComplete(anyhow::Result<Vec<SearchResult>>),
    DetailLoaded(anyhow::Result<DetailView>),
    Status(String),
}

/// Side-effecting work the loop should spawn off the render thread.
#[derive(Debug, Clone)]
pub enum AppCommand {
    Search(String),
    LoadDetail { kind: ObjectKind, id: u64 },
    OpenBrowser(String),
    Copy(String),
}

/// The whole TUI application state.
pub struct App {
    pub client: NetBoxClient,
    pub theme: Theme,
    pub theme_index: usize,
    pub profile_name: String,
    pub base_url: String,
    pub netbox_version: String,

    pub mode: Mode,
    pub screen: Screen,
    pub history: Vec<Screen>,
    pub status: String,

    pub search_input: String,
    pub command_input: String,

    pub results: Vec<SearchResult>,
    pub selected: usize,
    pub detail: Option<DetailView>,
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
    ) -> Self {
        Self {
            client,
            theme: Theme::by_name(theme_name),
            theme_index: Theme::index_of(theme_name),
            profile_name,
            base_url,
            netbox_version,
            mode: Mode::Normal,
            screen: Screen::Home,
            history: Vec::new(),
            status: String::new(),
            search_input: String::new(),
            command_input: String::new(),
            results: Vec::new(),
            selected: 0,
            detail: None,
            should_quit: false,
        }
    }

    /// Apply an event, returning any commands to dispatch.
    pub fn handle_event(&mut self, event: AppEvent) -> Vec<AppCommand> {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::Resize(_, _) | AppEvent::Tick => Vec::new(),
            AppEvent::SearchComplete(result) => {
                match result {
                    Ok(items) => {
                        self.status = format!("{} result(s)", items.len());
                        self.results = items;
                        self.selected = 0;
                    }
                    Err(e) => self.status = format!("error: {e:#}"),
                }
                Vec::new()
            }
            AppEvent::DetailLoaded(result) => {
                match result {
                    Ok(view) => {
                        self.navigate_to(Screen::Detail);
                        self.detail = Some(view);
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
            }
            KeyCode::Char(':') => {
                self.mode = Mode::Command;
                self.command_input.clear();
            }
            KeyCode::Char('t') => self.cycle_theme(),
            KeyCode::Char('j') | KeyCode::Down => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.select_prev(),
            KeyCode::Char('g') => self.selected = 0,
            KeyCode::Char('G') => self.selected = self.results.len().saturating_sub(1),
            KeyCode::Enter => {
                if self.screen == Screen::Home
                    && let Some(r) = self.results.get(self.selected)
                {
                    let (kind, id) = (r.kind, r.id);
                    self.status = "loading…".into();
                    return vec![AppCommand::LoadDetail { kind, id }];
                }
            }
            KeyCode::Char('o') => {
                if let Some(r) = self.results.get(self.selected) {
                    return vec![AppCommand::OpenBrowser(r.url.clone())];
                }
            }
            KeyCode::Char('y') => {
                if let Some(r) = self.results.get(self.selected) {
                    return vec![AppCommand::Copy(r.display.clone())];
                }
            }
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
                    self.status = format!("searching {query}…");
                    return vec![AppCommand::Search(query)];
                }
            }
            KeyCode::Char('u') if ctrl => self.search_input.clear(),
            KeyCode::Char('w') if ctrl => trim_last_word(&mut self.search_input),
            KeyCode::Backspace => {
                self.search_input.pop();
            }
            KeyCode::Char(c) => self.search_input.push(c),
            _ => {}
        }
        Vec::new()
    }

    fn handle_command_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                self.command_input.pop();
            }
            KeyCode::Char(c) => self.command_input.push(c),
            _ => {}
        }
        Vec::new()
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

    /// Advance to the next built-in theme.
    pub fn cycle_theme(&mut self) {
        let list = Theme::list();
        self.theme_index = (self.theme_index + 1) % list.len();
        self.theme = Theme::by_name(list[self.theme_index]);
        self.status = format!("theme: {}", list[self.theme_index]);
    }

    /// The currently selected result, if any.
    pub fn selected_result(&self) -> Option<&SearchResult> {
        self.results.get(self.selected)
    }

    fn select_next(&mut self) {
        if self.selected + 1 < self.results.len() {
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
        )
    }

    fn result() -> SearchResult {
        SearchResult {
            kind: ObjectKind::Device,
            id: 1,
            display: "edge01".into(),
            subtitle: Some("iad1".into()),
            url: "http://nb/dcim/devices/1/".into(),
            score: 100,
        }
    }

    fn press(code: KeyCode) -> AppEvent {
        AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
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
    fn enter_on_result_loads_detail_and_back_returns_home() {
        let mut a = app();
        a.results = vec![result()];
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadDetail {
                kind: ObjectKind::Device,
                id: 1
            }]
        ));

        // Simulate the detail load completing.
        a.handle_event(AppEvent::DetailLoaded(Ok(DetailView {
            title: "device edge01".into(),
            body: "name: edge01".into(),
        })));
        assert_eq!(a.screen, Screen::Detail);

        a.handle_event(press(KeyCode::Char('b')));
        assert_eq!(a.screen, Screen::Home);
    }

    #[test]
    fn o_and_y_emit_open_and_copy() {
        let mut a = app();
        a.results = vec![result()];
        let open = a.handle_event(press(KeyCode::Char('o')));
        assert!(
            matches!(open.as_slice(), [AppCommand::OpenBrowser(u)] if u == "http://nb/dcim/devices/1/")
        );
        let copy = a.handle_event(press(KeyCode::Char('y')));
        assert!(matches!(copy.as_slice(), [AppCommand::Copy(v)] if v == "edge01"));
    }

    #[test]
    fn theme_cycles() {
        let mut a = app();
        let before = a.theme_index;
        a.handle_event(press(KeyCode::Char('t')));
        assert_ne!(a.theme_index, before);
    }
}
