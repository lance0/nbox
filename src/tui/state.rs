//! TUI application state and (pure) input handling.
//!
//! `handle_event`/`handle_key` mutate state and return the commands to run —
//! they perform no I/O, so they're unit-testable without a terminal. Network
//! work happens in spawned tasks (see `tui::app`), never in the render loop.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::netbox::client::NetBoxClient;
use crate::netbox::search::SearchResult;
use crate::tui::theme::Theme;

/// Which screen is in the body area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Help,
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
}

/// Side-effecting work the loop should spawn off the render thread.
#[derive(Debug, Clone)]
pub enum AppCommand {
    Search(String),
}

/// The whole TUI application state.
pub struct App {
    pub client: NetBoxClient,
    pub theme: Theme,
    pub theme_index: usize,
    pub profile_name: String,
    pub base_url: String,

    pub mode: Mode,
    pub screen: Screen,
    pub status: String,

    pub search_input: String,
    pub command_input: String,

    pub results: Vec<SearchResult>,
    pub selected: usize,
    pub should_quit: bool,
}

impl App {
    /// Construct a fresh app on the home screen.
    pub fn new(
        client: NetBoxClient,
        theme_name: &str,
        profile_name: String,
        base_url: String,
    ) -> Self {
        Self {
            client,
            theme: Theme::by_name(theme_name),
            theme_index: Theme::index_of(theme_name),
            profile_name,
            base_url,
            mode: Mode::Normal,
            screen: Screen::Home,
            status: String::new(),
            search_input: String::new(),
            command_input: String::new(),
            results: Vec::new(),
            selected: 0,
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
                if self.screen == Screen::Help {
                    self.screen = Screen::Home;
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.screen = if self.screen == Screen::Help {
                    Screen::Home
                } else {
                    Screen::Help
                };
            }
            KeyCode::Esc | KeyCode::Char('b') => {
                if self.screen == Screen::Help {
                    self.screen = Screen::Home;
                }
            }
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
            KeyCode::Char('G') => {
                self.selected = self.results.len().saturating_sub(1);
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
        App::new(client, "default", "test".into(), "http://localhost".into())
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
    fn esc_leaves_search_without_command() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('/')));
        a.handle_event(press(KeyCode::Char('x')));
        let cmds = a.handle_event(press(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Normal);
        assert!(cmds.is_empty());
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
}
