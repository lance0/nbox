//! The in-app Config modal: pure state + key handling for both sections.
//!
//! A single modal with two sections (`Profiles` | `Settings`), `Tab` to switch.
//! The Profiles section lists the configured profiles (active marked) with add /
//! edit / select / delete actions, and an add/edit [`FormInput`] form whose token
//! field is masked (never written to TOML; stored in the OS keyring on save). The
//! Settings section is a small form over the *real* `[ui]` settings — theme (a
//! cycle), `refresh_secs` (numeric), and `open_browser_command` (text); the no-op
//! `wide`/`confirm_writes` knobs are deliberately excluded.
//!
//! Everything here is PURE: key handling mutates the modal's own state and yields
//! a [`ModalOutcome`] describing what the app should *do* (test-connect, save,
//! select, delete, close). The app (`tui::state`) performs the I/O —
//! config-file writes, the keyring, the reconnect/switch path — never this module.
//! That keeps the modal unit-testable without a terminal or a network.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::netbox::auth::AuthScheme;
use crate::tui::cheese::{FormInput, TextInput};
use crate::tui::theme::Theme;

/// Which section of the Config modal is active. `Tab` switches sections at the
/// Profiles list level and from the Settings form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSection {
    Profiles,
    Settings,
}

/// The text fields of the add/edit form, by index — kept in sync with the order
/// the [`FormInput`] is built in [`ProfileForm::new`].
pub mod field {
    pub const NAME: usize = 0;
    pub const URL: usize = 1;
    pub const TOKEN_ENV: usize = 2;
    pub const TOKEN: usize = 3;
}

/// The result of a test-connect attempt, shown in the form before committing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestState {
    /// No test run yet for the current form contents.
    Idle,
    /// A test-connect is in flight (spinner shown).
    Testing,
    /// The last test succeeded, carrying the probed NetBox version.
    Ok(String),
    /// The last test failed, carrying the error to display.
    Failed(String),
}

/// The add/edit profile form: the masked-token-aware text fields plus the two
/// non-text controls (auth-scheme cycle, verify-tls toggle), the edit target (if
/// editing), and the latest test-connect state.
pub struct ProfileForm {
    /// name, url, token_env, token(masked) — see [`field`].
    pub inputs: FormInput,
    /// Cycled with the dedicated control key (auto → bearer → token → auto).
    pub auth_scheme: AuthScheme,
    /// Toggled with the dedicated control key.
    pub verify_tls: bool,
    /// `Some(original_name)` when editing an existing profile (so a rename can be
    /// detected and the old keyring entry migrated); `None` when adding.
    pub editing: Option<String>,
    /// Latest test-connect outcome for the current contents.
    pub test: TestState,
    /// A transient validation / info line shown under the form.
    pub message: Option<String>,
}

impl ProfileForm {
    /// A blank add form (focus on `name`).
    pub fn add() -> Self {
        let inputs = FormInput::new(vec![
            ("name".to_string(), TextInput::new("profile name")),
            (
                "url".to_string(),
                TextInput::new("https://netbox.example.com"),
            ),
            (
                "token_env".to_string(),
                TextInput::new("env var holding the token (optional)"),
            ),
            (
                "token".to_string(),
                TextInput::masked("paste a token to store in the keyring (optional)"),
            ),
        ]);
        Self {
            inputs,
            auth_scheme: AuthScheme::Auto,
            verify_tls: true,
            editing: None,
            test: TestState::Idle,
            message: None,
        }
    }

    /// An edit form prefilled from an existing profile's metadata. The token
    /// field starts empty — the stored secret is never read back into the UI;
    /// leaving it blank keeps the existing keyring entry untouched.
    pub fn edit(
        name: &str,
        url: &str,
        token_env: Option<&str>,
        auth_scheme: AuthScheme,
        verify_tls: bool,
    ) -> Self {
        let mut form = Self::add();
        form.inputs.input_mut(field::NAME).unwrap().set_value(name);
        form.inputs.input_mut(field::URL).unwrap().set_value(url);
        if let Some(env) = token_env {
            form.inputs
                .input_mut(field::TOKEN_ENV)
                .unwrap()
                .set_value(env);
        }
        form.auth_scheme = auth_scheme;
        form.verify_tls = verify_tls;
        form.editing = Some(name.to_string());
        form
    }

    /// The trimmed `name` field.
    pub fn name(&self) -> String {
        self.inputs
            .value(field::NAME)
            .unwrap_or("")
            .trim()
            .to_string()
    }

    /// The trimmed `url` field.
    pub fn url(&self) -> String {
        self.inputs
            .value(field::URL)
            .unwrap_or("")
            .trim()
            .to_string()
    }

    /// The trimmed `token_env` field, `None` when empty.
    pub fn token_env(&self) -> Option<String> {
        let v = self
            .inputs
            .value(field::TOKEN_ENV)
            .unwrap_or("")
            .trim()
            .to_string();
        if v.is_empty() { None } else { Some(v) }
    }

    /// The token field's raw value, `None` when empty. Used only to hand straight
    /// to the keyring; it is never rendered (the field is masked) or logged.
    pub fn token(&self) -> Option<String> {
        let v = self.inputs.value(field::TOKEN).unwrap_or("").to_string();
        if v.is_empty() { None } else { Some(v) }
    }

    /// Read-only access to the form's text inputs (for the onboarding wizard,
    /// which reuses this form standalone before the `App` exists).
    pub fn form_input(&self) -> &FormInput {
        &self.inputs
    }

    /// Mutable access to the form's text inputs — used by the onboarding wizard to
    /// prefill / route keys through the same `FormInput` the editor uses.
    pub fn form_input_mut(&mut self) -> &mut FormInput {
        &mut self.inputs
    }

    /// Advance the auth-scheme control: auto → bearer → token → auto. Public so
    /// the onboarding wizard can drive the same control.
    pub fn cycle_auth_scheme(&mut self) {
        self.auth_scheme = match self.auth_scheme {
            AuthScheme::Auto => AuthScheme::Bearer,
            AuthScheme::Bearer => AuthScheme::Token,
            AuthScheme::Token => AuthScheme::Auto,
        };
        self.invalidate_test();
    }

    /// Flip the verify-tls toggle. Public so the onboarding wizard can drive it.
    pub fn toggle_verify_tls(&mut self) {
        self.verify_tls = !self.verify_tls;
        self.invalidate_test();
    }

    /// Public wrapper for [`Self::invalidate_test`], for the onboarding wizard
    /// (which routes edits through `FormInput` directly and must invalidate a
    /// prior test result the same way the editor form does).
    pub fn invalidate_test_public(&mut self) {
        self.invalidate_test();
    }

    /// Any edit to the form invalidates a prior test result (it no longer
    /// describes the current contents) so the user can't save a stale OK.
    fn invalidate_test(&mut self) {
        if self.test != TestState::Testing {
            self.test = TestState::Idle;
        }
        self.message = None;
    }

    /// Validate the required fields, returning an error message when invalid.
    pub fn validate(&self) -> Result<(), String> {
        if self.name().is_empty() {
            return Err("name is required".to_string());
        }
        let url = self.url();
        if url.is_empty() {
            return Err("url is required".to_string());
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("url must start with http:// or https://".to_string());
        }
        Ok(())
    }
}

/// What the Profiles section is currently showing: the list, or an add/edit form,
/// or a delete confirmation.
pub enum ProfilesMode {
    /// The list of configured profiles; `selected` indexes into it.
    List { selected: usize },
    /// The add/edit form.
    Form(ProfileForm),
    /// A confirm-delete prompt for the named profile (returns to the list).
    ConfirmDelete { name: String, selected: usize },
}

/// The Profiles section state.
pub struct ProfilesPane {
    pub mode: ProfilesMode,
    /// A transient list-level guidance line (e.g. why a delete was blocked),
    /// shown under the list. Cleared on the next navigation.
    pub message: Option<String>,
}

impl ProfilesPane {
    fn new() -> Self {
        Self {
            mode: ProfilesMode::List { selected: 0 },
            message: None,
        }
    }
}

/// The focusable rows of the Settings form, in display order. `theme` is a cycle
/// (Left/Right or Space), the other two are text fields.
pub mod setting {
    pub const THEME: usize = 0;
    pub const REFRESH: usize = 1;
    pub const BROWSER: usize = 2;
    /// Number of settings rows.
    pub const COUNT: usize = 3;
}

/// The Settings section: an editable form over the *real* `[ui]` settings.
///
/// `theme` is held as an index into [`Theme::list`] and cycled in place (it also
/// hot-applies live to the running app — see [`ModalOutcome::ChangeTheme`]). The
/// other two are free-text [`TextInput`]s: `refresh_secs` (numeric; empty/0 =
/// off) and `open_browser_command` (empty = the OS default). `focus` selects the
/// row Up/Down move between; typing / cycling routes to the focused row. PURE +
/// unit-testable — no I/O; the app persists + re-arms on [`ModalOutcome::SaveSettings`].
pub struct SettingsPane {
    /// Index into [`Theme::list`] of the working theme selection.
    pub theme_index: usize,
    /// `refresh_secs` as typed (digits only; empty = off).
    pub refresh: TextInput,
    /// `open_browser_command` as typed (empty = OS default).
    pub browser: TextInput,
    /// Focused row: one of [`setting::THEME`] / `REFRESH` / `BROWSER`.
    pub focus: usize,
    /// A transient info / validation line shown under the form.
    pub message: Option<String>,
}

impl SettingsPane {
    /// Build the form seeded from the live settings: the current theme name, the
    /// refresh interval (rendered as digits; `None`/`0` = empty = off), and the
    /// browser command.
    fn new(theme_name: &str, refresh_secs: Option<u64>, open_browser_command: &str) -> Self {
        let mut refresh = TextInput::new("seconds (empty = off)");
        if let Some(secs) = refresh_secs.filter(|s| *s > 0) {
            refresh.set_value(secs.to_string());
        }
        let mut browser = TextInput::new("custom open command (empty = OS default)");
        if !open_browser_command.is_empty() {
            browser.set_value(open_browser_command);
        }
        Self {
            theme_index: Theme::index_of(theme_name),
            refresh,
            browser,
            focus: setting::THEME,
            message: None,
        }
    }

    /// The currently selected theme name (from the working index).
    pub fn theme_name(&self) -> &'static str {
        Theme::list()[self.theme_index.min(Theme::list().len() - 1)]
    }

    /// The typed `refresh_secs` parsed to an optional interval: empty or `0` ⇒
    /// `None` (off); non-numeric text ⇒ `None` too (validated on save).
    pub fn refresh_secs(&self) -> Option<u64> {
        let t = self.refresh.value().trim();
        if t.is_empty() {
            return None;
        }
        t.parse::<u64>().ok().filter(|s| *s > 0)
    }

    /// The typed `open_browser_command` (trimmed of surrounding whitespace).
    pub fn browser_command(&self) -> String {
        self.browser.value().trim().to_string()
    }

    /// Move focus to the next row (wrapping).
    fn focus_next(&mut self) {
        self.focus = (self.focus + 1) % setting::COUNT;
    }

    /// Move focus to the previous row (wrapping).
    fn focus_prev(&mut self) {
        self.focus = (self.focus + setting::COUNT - 1) % setting::COUNT;
    }

    /// Cycle the theme selection by `+1`/`-1`, wrapping. The change hot-applies
    /// live; persistence happens on save.
    fn cycle_theme(&mut self, forward: bool) {
        let len = Theme::list().len();
        self.theme_index = if forward {
            (self.theme_index + 1) % len
        } else {
            (self.theme_index + len - 1) % len
        };
        self.message = None;
    }

    /// Validate the form for save: `refresh_secs`, when non-empty, must be a
    /// non-negative integer (0 = off is allowed). Returns an error message to show.
    fn validate(&self) -> Result<(), String> {
        let t = self.refresh.value().trim();
        if !t.is_empty() && t.parse::<u64>().is_err() {
            return Err("refresh_secs must be a whole number of seconds".to_string());
        }
        Ok(())
    }
}

/// The whole Config modal: the active section plus both section states.
pub struct ConfigModal {
    pub section: ConfigSection,
    pub profiles: ProfilesPane,
    pub settings: SettingsPane,
}

impl ConfigModal {
    /// Open the modal on the Profiles section, seeding the Settings form from the
    /// live UI settings (so a switch to Settings shows the current values).
    pub fn new(theme_name: &str, refresh_secs: Option<u64>, open_browser_command: &str) -> Self {
        Self {
            section: ConfigSection::Profiles,
            profiles: ProfilesPane::new(),
            settings: SettingsPane::new(theme_name, refresh_secs, open_browser_command),
        }
    }
}

impl Default for ConfigModal {
    /// A modal seeded with default settings (the `default` theme, no auto-refresh,
    /// the OS default opener) — used by tests / when no live settings are handy.
    fn default() -> Self {
        Self::new("default", None, "")
    }
}

/// What the app should do after the modal handles a key. The modal stays pure —
/// it returns one of these, and `tui::state::App` performs the I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalOutcome {
    /// Nothing to do (state changed in place, or the key was inert).
    None,
    /// Close the Config modal.
    Close,
    /// Test-connect the form's current contents (builds a temp client + probes).
    TestConnect,
    /// Persist the form (upsert + keyring) and, when `use_it`, switch to it.
    Save { use_it: bool },
    /// Switch the live session to (and persist as active) the named profile.
    Select(String),
    /// Open the edit form for the named profile. The app fills it in via
    /// [`ConfigModal::open_edit_form`] (it owns the [`ProfileConfig`]).
    Edit(String),
    /// Remove the named profile (config + keyring).
    Delete(String),
    /// Hot-apply the named theme to the running app (the Settings theme cycle).
    /// The app routes this through the same theme path as the `t` cycle / palette
    /// `:theme` — including the `NO_COLOR` guard — so it applies live; the value is
    /// persisted on [`Self::SaveSettings`].
    ChangeTheme(String),
    /// Persist the Settings form and hot-apply: write each changed `[ui]` field,
    /// re-arm the auto-refresh ticker at the new interval, and adopt the live
    /// browser command. The app reads the values off the modal's [`SettingsPane`].
    SaveSettings,
}

impl ConfigModal {
    /// Handle a key while the modal is open. Returns the [`ModalOutcome`] the app
    /// should act on. PURE: mutates only the modal's own state.
    ///
    /// `profiles` is the live profile-name list (for list movement/selection) and
    /// `active` the active profile name (so the list can mark it and guard the
    /// delete of the active/last profile).
    pub fn handle_key(&mut self, key: KeyEvent, profiles: &[String], active: &str) -> ModalOutcome {
        // Tab switches sections only at the list level (never mid-form, where Tab
        // moves field focus). Esc/`q` close from the list; the form/confirm
        // handle their own Esc (back to the list).
        match self.section {
            ConfigSection::Profiles => self.handle_profiles_key(key, profiles, active),
            ConfigSection::Settings => self.handle_settings_key(key),
        }
    }

    /// Drive the Settings form. `Tab`/`Shift-Tab` switch back to Profiles (the
    /// section toggle); `Up`/`Down` move row focus; the theme row cycles with
    /// `Left`/`Right`/`Space`; the two text rows edit in place. `Enter`/`Ctrl+S`
    /// save; `Esc` closes. (`q` is a valid command char, so it does *not* close
    /// from here — only `Esc` does, unlike the Profiles list.)
    fn handle_settings_key(&mut self, key: KeyEvent) -> ModalOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // The section toggle and close are handled before the `settings` borrow so
        // the Tab arm can touch `self.section` cleanly.
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.section = ConfigSection::Profiles;
                return ModalOutcome::None;
            }
            KeyCode::Esc => return ModalOutcome::Close,
            _ => {}
        }
        let s = &mut self.settings;
        match key.code {
            KeyCode::Up => {
                s.focus_prev();
                ModalOutcome::None
            }
            KeyCode::Down => {
                s.focus_next();
                ModalOutcome::None
            }
            // Enter always saves; Ctrl+S also saves (so a save is reachable while a
            // text row is focused without leaving it). A bare `s` falls through to
            // text editing below.
            KeyCode::Enter => match s.validate() {
                Ok(()) => ModalOutcome::SaveSettings,
                Err(e) => {
                    s.message = Some(e);
                    ModalOutcome::None
                }
            },
            KeyCode::Char('s') if ctrl => match s.validate() {
                Ok(()) => ModalOutcome::SaveSettings,
                Err(e) => {
                    s.message = Some(e);
                    ModalOutcome::None
                }
            },
            // The theme row cycles with Left/Right or Space; the change hot-applies.
            KeyCode::Left if s.focus == setting::THEME => {
                s.cycle_theme(false);
                ModalOutcome::ChangeTheme(s.theme_name().to_string())
            }
            KeyCode::Right | KeyCode::Char(' ') if s.focus == setting::THEME => {
                s.cycle_theme(true);
                ModalOutcome::ChangeTheme(s.theme_name().to_string())
            }
            // Otherwise: text editing on the focused text row. The theme row has no
            // text field, so non-cycle keys there are inert.
            _ => {
                let field = match s.focus {
                    setting::REFRESH => Some(&mut s.refresh),
                    setting::BROWSER => Some(&mut s.browser),
                    _ => None, // the theme row: no text field
                };
                if let Some(input) = field
                    && input.handle_key(key)
                {
                    s.message = None;
                }
                ModalOutcome::None
            }
        }
    }

    fn handle_profiles_key(
        &mut self,
        key: KeyEvent,
        profiles: &[String],
        active: &str,
    ) -> ModalOutcome {
        // Take the mode out so we can match it by value and rebuild it; this keeps
        // each arm a clean state transition.
        match &mut self.profiles.mode {
            ProfilesMode::List { .. } => self.handle_list_key(key, profiles, active),
            ProfilesMode::Form(_) => self.handle_form_key(key),
            ProfilesMode::ConfirmDelete { .. } => self.handle_confirm_key(key, active),
        }
    }

    fn handle_list_key(
        &mut self,
        key: KeyEvent,
        profiles: &[String],
        active: &str,
    ) -> ModalOutcome {
        let len = profiles.len();
        // Clear the transient guidance line on any list key; specific arms re-set
        // it (a blocked delete) below.
        self.profiles.message = None;
        let ProfilesMode::List { selected } = &mut self.profiles.mode else {
            return ModalOutcome::None;
        };
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.section = ConfigSection::Settings;
                ModalOutcome::None
            }
            KeyCode::Esc | KeyCode::Char('q') => ModalOutcome::Close,
            KeyCode::Char('j') | KeyCode::Down => {
                if len > 0 {
                    *selected = (*selected + 1).min(len - 1);
                }
                ModalOutcome::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                *selected = selected.saturating_sub(1);
                ModalOutcome::None
            }
            KeyCode::Char('a') => {
                self.profiles.mode = ProfilesMode::Form(ProfileForm::add());
                ModalOutcome::None
            }
            KeyCode::Char('e') => {
                // Edit the selected profile: surface the request; the app opens the
                // prefilled form via `open_edit_form` (it owns the ProfileConfig),
                // keeping this module pure.
                profiles
                    .get(*selected)
                    .map_or(ModalOutcome::None, |name| ModalOutcome::Edit(name.clone()))
            }
            KeyCode::Enter | KeyCode::Char('s') => {
                profiles.get(*selected).map_or(ModalOutcome::None, |name| {
                    ModalOutcome::Select(name.clone())
                })
            }
            KeyCode::Char('d') => {
                let sel = *selected;
                let Some(name) = profiles.get(sel).cloned() else {
                    return ModalOutcome::None;
                };
                // Guard: don't delete the last remaining profile, nor the active
                // one (the session is connected to it).
                if len <= 1 {
                    self.profiles.message = Some("can't delete the only profile".to_string());
                    return ModalOutcome::None;
                }
                if name == active {
                    self.profiles.message =
                        Some("can't delete the active profile — switch away first".to_string());
                    return ModalOutcome::None;
                }
                self.profiles.mode = ProfilesMode::ConfirmDelete {
                    name,
                    selected: sel,
                };
                ModalOutcome::None
            }
            _ => ModalOutcome::None,
        }
    }

    fn handle_form_key(&mut self, key: KeyEvent) -> ModalOutcome {
        let ProfilesMode::Form(form) = &mut self.profiles.mode else {
            return ModalOutcome::None;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                // Back to the list, discarding the form.
                self.profiles.mode = ProfilesMode::List { selected: 0 };
                ModalOutcome::None
            }
            // Ctrl+S cycles the auth scheme; Ctrl+L toggles verify-tls. These use
            // Ctrl so the bare letters keep flowing into the focused text field.
            KeyCode::Char('s') if ctrl => {
                form.cycle_auth_scheme();
                ModalOutcome::None
            }
            KeyCode::Char('l') if ctrl => {
                form.toggle_verify_tls();
                ModalOutcome::None
            }
            // Ctrl+T tests the connection; Enter saves; Ctrl+U saves + uses it.
            KeyCode::Char('t') if ctrl => match form.validate() {
                Ok(()) => {
                    form.test = TestState::Testing;
                    form.message = None;
                    ModalOutcome::TestConnect
                }
                Err(e) => {
                    form.message = Some(e);
                    ModalOutcome::None
                }
            },
            KeyCode::Enter => match form.validate() {
                Ok(()) => ModalOutcome::Save { use_it: false },
                Err(e) => {
                    form.message = Some(e);
                    ModalOutcome::None
                }
            },
            KeyCode::Char('u') if ctrl => match form.validate() {
                Ok(()) => ModalOutcome::Save { use_it: true },
                Err(e) => {
                    form.message = Some(e);
                    ModalOutcome::None
                }
            },
            _ => {
                // Everything else is text editing / focus movement; an edit
                // invalidates a prior test result.
                if form.inputs.handle_key(key) {
                    form.invalidate_test();
                }
                ModalOutcome::None
            }
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent, active: &str) -> ModalOutcome {
        let ProfilesMode::ConfirmDelete { name, selected } = &mut self.profiles.mode else {
            return ModalOutcome::None;
        };
        let name = name.clone();
        let selected = *selected;
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                // Re-check the active guard at confirm time (it can't have changed
                // mid-modal, but cheap and safe).
                if name == active {
                    self.profiles.mode = ProfilesMode::List { selected };
                    ModalOutcome::None
                } else {
                    self.profiles.mode = ProfilesMode::List { selected };
                    ModalOutcome::Delete(name)
                }
            }
            // Anything else cancels back to the list.
            _ => {
                self.profiles.mode = ProfilesMode::List { selected };
                ModalOutcome::None
            }
        }
    }

    /// Open the edit form for `name`, prefilled from its metadata. Called by the
    /// app (which owns the [`ProfileConfig`]) in response to the edit request.
    pub fn open_edit_form(
        &mut self,
        name: &str,
        url: &str,
        token_env: Option<&str>,
        auth_scheme: AuthScheme,
        verify_tls: bool,
    ) {
        self.profiles.mode = ProfilesMode::Form(ProfileForm::edit(
            name,
            url,
            token_env,
            auth_scheme,
            verify_tls,
        ));
    }

    /// Return to the list view (after a save/delete settles), selecting `idx`.
    pub fn show_list(&mut self, idx: usize) {
        self.profiles.mode = ProfilesMode::List { selected: idx };
    }

    /// The form currently being edited, if any (for the app to read on save / to
    /// post a test result back into).
    pub fn form_mut(&mut self) -> Option<&mut ProfileForm> {
        match &mut self.profiles.mode {
            ProfilesMode::Form(f) => Some(f),
            _ => None,
        }
    }

    /// The form currently being edited, if any (read-only).
    pub fn form(&self) -> Option<&ProfileForm> {
        match &self.profiles.mode {
            ProfilesMode::Form(f) => Some(f),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_into(m: &mut ConfigModal, s: &str) {
        for c in s.chars() {
            m.handle_key(key(KeyCode::Char(c)), &[], "");
        }
    }

    #[test]
    fn opens_on_profiles_list() {
        let m = ConfigModal::default();
        assert_eq!(m.section, ConfigSection::Profiles);
        assert!(matches!(
            m.profiles.mode,
            ProfilesMode::List { selected: 0 }
        ));
    }

    #[test]
    fn tab_switches_sections_at_list_level() {
        let mut m = ConfigModal::default();
        m.handle_key(key(KeyCode::Tab), &[], "");
        assert_eq!(m.section, ConfigSection::Settings);
        m.handle_key(key(KeyCode::Tab), &[], "");
        assert_eq!(m.section, ConfigSection::Profiles);
    }

    #[test]
    fn list_movement_clamps_to_bounds() {
        let mut m = ConfigModal::default();
        let names = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        m.handle_key(key(KeyCode::Char('j')), &names, "a");
        assert!(matches!(
            m.profiles.mode,
            ProfilesMode::List { selected: 1 }
        ));
        m.handle_key(key(KeyCode::Char('j')), &names, "a");
        m.handle_key(key(KeyCode::Char('j')), &names, "a"); // clamps at last
        assert!(matches!(
            m.profiles.mode,
            ProfilesMode::List { selected: 2 }
        ));
        m.handle_key(key(KeyCode::Char('k')), &names, "a");
        m.handle_key(key(KeyCode::Char('k')), &names, "a");
        m.handle_key(key(KeyCode::Char('k')), &names, "a"); // clamps at first
        assert!(matches!(
            m.profiles.mode,
            ProfilesMode::List { selected: 0 }
        ));
    }

    #[test]
    fn esc_and_q_close_from_list() {
        let mut m = ConfigModal::default();
        assert_eq!(
            m.handle_key(key(KeyCode::Esc), &[], ""),
            ModalOutcome::Close
        );
        let mut m2 = ConfigModal::default();
        assert_eq!(
            m2.handle_key(key(KeyCode::Char('q')), &[], ""),
            ModalOutcome::Close
        );
    }

    #[test]
    fn a_opens_add_form_and_esc_returns_to_list() {
        let mut m = ConfigModal::default();
        m.handle_key(key(KeyCode::Char('a')), &[], "");
        assert!(matches!(m.profiles.mode, ProfilesMode::Form(_)));
        assert!(m.form().unwrap().editing.is_none(), "add form, not edit");
        m.handle_key(key(KeyCode::Esc), &[], "");
        assert!(matches!(m.profiles.mode, ProfilesMode::List { .. }));
    }

    #[test]
    fn enter_on_list_selects_the_highlighted_profile() {
        let mut m = ConfigModal::default();
        let names = vec!["a".to_string(), "b".to_string()];
        m.handle_key(key(KeyCode::Char('j')), &names, "a"); // → b
        let out = m.handle_key(key(KeyCode::Enter), &names, "a");
        assert_eq!(out, ModalOutcome::Select("b".to_string()));
    }

    #[test]
    fn form_typing_routes_to_focused_field_and_validates() {
        let mut m = ConfigModal::default();
        m.handle_key(key(KeyCode::Char('a')), &[], ""); // open add form
        // name field
        type_into(&mut m, "lab");
        // Tab to url, type a bare (invalid) url
        m.handle_key(key(KeyCode::Tab), &[], "");
        type_into(&mut m, "nb.lab");
        // Enter → validation error (url scheme), no save.
        let out = m.handle_key(key(KeyCode::Enter), &[], "");
        assert_eq!(out, ModalOutcome::None);
        assert!(
            m.form()
                .unwrap()
                .message
                .as_deref()
                .unwrap()
                .contains("http")
        );
    }

    #[test]
    fn form_enter_saves_when_valid() {
        let mut m = ConfigModal::default();
        m.handle_key(key(KeyCode::Char('a')), &[], "");
        type_into(&mut m, "lab");
        m.handle_key(key(KeyCode::Tab), &[], "");
        type_into(&mut m, "https://nb.lab");
        let out = m.handle_key(key(KeyCode::Enter), &[], "");
        assert_eq!(out, ModalOutcome::Save { use_it: false });
        // Ctrl+U saves + uses it.
        let out = m.handle_key(ctrl('u'), &[], "");
        assert_eq!(out, ModalOutcome::Save { use_it: true });
    }

    #[test]
    fn ctrl_t_requests_a_test_when_valid() {
        let mut m = ConfigModal::default();
        m.handle_key(key(KeyCode::Char('a')), &[], "");
        type_into(&mut m, "lab");
        m.handle_key(key(KeyCode::Tab), &[], "");
        type_into(&mut m, "https://nb.lab");
        let out = m.handle_key(ctrl('t'), &[], "");
        assert_eq!(out, ModalOutcome::TestConnect);
        assert_eq!(m.form().unwrap().test, TestState::Testing);
    }

    #[test]
    fn ctrl_s_cycles_auth_scheme_and_ctrl_l_toggles_tls() {
        let mut m = ConfigModal::default();
        m.handle_key(key(KeyCode::Char('a')), &[], "");
        assert_eq!(m.form().unwrap().auth_scheme, AuthScheme::Auto);
        m.handle_key(ctrl('s'), &[], "");
        assert_eq!(m.form().unwrap().auth_scheme, AuthScheme::Bearer);
        m.handle_key(ctrl('s'), &[], "");
        assert_eq!(m.form().unwrap().auth_scheme, AuthScheme::Token);
        m.handle_key(ctrl('s'), &[], "");
        assert_eq!(m.form().unwrap().auth_scheme, AuthScheme::Auto);
        assert!(m.form().unwrap().verify_tls);
        m.handle_key(ctrl('l'), &[], "");
        assert!(!m.form().unwrap().verify_tls);
    }

    #[test]
    fn editing_form_keeps_token_blank_and_records_original_name() {
        let mut m = ConfigModal::default();
        m.open_edit_form(
            "work",
            "https://w",
            Some("W_TOKEN"),
            AuthScheme::Bearer,
            false,
        );
        let f = m.form().unwrap();
        assert_eq!(f.name(), "work");
        assert_eq!(f.url(), "https://w");
        assert_eq!(f.token_env().as_deref(), Some("W_TOKEN"));
        assert_eq!(f.auth_scheme, AuthScheme::Bearer);
        assert!(!f.verify_tls);
        assert_eq!(f.editing.as_deref(), Some("work"));
        // The token field starts empty — the stored secret is never read back.
        assert!(f.token().is_none());
    }

    #[test]
    fn token_field_is_masked_and_never_exposes_its_value() {
        let mut m = ConfigModal::default();
        m.handle_key(key(KeyCode::Char('a')), &[], "");
        // Tab to the token field (index 3): name→url→token_env→token.
        for _ in 0..field::TOKEN {
            m.handle_key(key(KeyCode::Tab), &[], "");
        }
        type_into(&mut m, "nbt_secret");
        let f = m.form().unwrap();
        // The raw value is available for the keyring…
        assert_eq!(f.token().as_deref(), Some("nbt_secret"));
        // …but the rendered token line is all bullets.
        let lines = f.inputs.rendered_lines();
        let token_line = &lines[field::TOKEN].1;
        assert!(!token_line.contains("nbt_secret"));
        assert!(!token_line.contains("secret"));
    }

    #[test]
    fn delete_confirm_then_y_deletes_a_non_active_profile() {
        let mut m = ConfigModal::default();
        let names = vec!["a".to_string(), "b".to_string()];
        // Highlight b (not active), press d → confirm.
        m.handle_key(key(KeyCode::Char('j')), &names, "a"); // → b
        m.handle_key(key(KeyCode::Char('d')), &names, "a");
        assert!(matches!(
            m.profiles.mode,
            ProfilesMode::ConfirmDelete { ref name, .. } if name == "b"
        ));
        let out = m.handle_key(key(KeyCode::Char('y')), &names, "a");
        assert_eq!(out, ModalOutcome::Delete("b".to_string()));
        assert!(matches!(m.profiles.mode, ProfilesMode::List { .. }));
    }

    #[test]
    fn delete_is_blocked_for_active_and_last_profile() {
        // Active profile: d is a no-op (no confirm).
        let mut m = ConfigModal::default();
        let names = vec!["a".to_string(), "b".to_string()];
        m.handle_key(key(KeyCode::Char('d')), &names, "a"); // a is active
        assert!(
            matches!(m.profiles.mode, ProfilesMode::List { .. }),
            "deleting the active profile does not open a confirm"
        );
        // Last/only profile: d is a no-op too.
        let mut m2 = ConfigModal::default();
        let one = vec!["only".to_string()];
        m2.handle_key(key(KeyCode::Char('d')), &one, "other");
        assert!(matches!(m2.profiles.mode, ProfilesMode::List { .. }));
    }

    #[test]
    fn confirm_delete_n_cancels_back_to_list() {
        let mut m = ConfigModal::default();
        let names = vec!["a".to_string(), "b".to_string()];
        m.handle_key(key(KeyCode::Char('j')), &names, "a");
        m.handle_key(key(KeyCode::Char('d')), &names, "a");
        let out = m.handle_key(key(KeyCode::Char('n')), &names, "a");
        assert_eq!(out, ModalOutcome::None);
        assert!(matches!(
            m.profiles.mode,
            ProfilesMode::List { selected: 1 }
        ));
    }

    #[test]
    fn edit_request_carries_the_selected_name() {
        // `e` surfaces the edit request encoded for the app, which then opens the
        // prefilled form (it owns the ProfileConfig).
        let mut m = ConfigModal::default();
        let names = vec!["a".to_string(), "b".to_string()];
        m.handle_key(key(KeyCode::Char('j')), &names, "a"); // → b
        let out = m.handle_key(key(KeyCode::Char('e')), &names, "a");
        assert_eq!(out, ModalOutcome::Edit("b".to_string()));
    }

    // ---- Settings section ----------------------------------------------------

    /// Open a modal and switch it to the Settings section.
    fn on_settings(theme: &str, refresh: Option<u64>, browser: &str) -> ConfigModal {
        let mut m = ConfigModal::new(theme, refresh, browser);
        m.handle_key(key(KeyCode::Tab), &[], ""); // Profiles list → Settings
        assert_eq!(m.section, ConfigSection::Settings);
        m
    }

    #[test]
    fn settings_form_seeds_from_the_live_values() {
        let m = on_settings("nord", Some(30), "firefox");
        assert_eq!(m.settings.theme_name(), "nord");
        assert_eq!(m.settings.refresh_secs(), Some(30));
        assert_eq!(m.settings.browser_command(), "firefox");
        // refresh_secs renders as digits; an absent interval seeds an empty field.
        let blank = on_settings("default", None, "");
        assert_eq!(blank.settings.refresh.value(), "");
        assert_eq!(blank.settings.refresh_secs(), None);
        assert_eq!(blank.settings.browser_command(), "");
    }

    #[test]
    fn settings_tab_switches_back_to_profiles_and_esc_closes() {
        let mut m = on_settings("default", None, "");
        m.handle_key(key(KeyCode::Tab), &[], "");
        assert_eq!(m.section, ConfigSection::Profiles);
        // Esc from Settings closes the whole modal.
        let mut m2 = on_settings("default", None, "");
        assert_eq!(
            m2.handle_key(key(KeyCode::Esc), &[], ""),
            ModalOutcome::Close
        );
    }

    #[test]
    fn settings_up_down_move_row_focus_and_wrap() {
        let mut m = on_settings("default", None, "");
        assert_eq!(m.settings.focus, setting::THEME);
        m.handle_key(key(KeyCode::Down), &[], "");
        assert_eq!(m.settings.focus, setting::REFRESH);
        m.handle_key(key(KeyCode::Down), &[], "");
        assert_eq!(m.settings.focus, setting::BROWSER);
        m.handle_key(key(KeyCode::Down), &[], ""); // wraps to THEME
        assert_eq!(m.settings.focus, setting::THEME);
        m.handle_key(key(KeyCode::Up), &[], ""); // wraps to BROWSER
        assert_eq!(m.settings.focus, setting::BROWSER);
    }

    #[test]
    fn settings_theme_cycles_and_emits_change_theme() {
        let mut m = on_settings("default", None, "");
        // Right cycles forward to the next theme and emits a hot-apply outcome.
        let out = m.handle_key(key(KeyCode::Right), &[], "");
        assert_eq!(m.settings.theme_name(), Theme::list()[1]);
        assert_eq!(out, ModalOutcome::ChangeTheme(Theme::list()[1].to_string()));
        // Left cycles back to the first.
        let out = m.handle_key(key(KeyCode::Left), &[], "");
        assert_eq!(m.settings.theme_name(), Theme::list()[0]);
        assert_eq!(out, ModalOutcome::ChangeTheme(Theme::list()[0].to_string()));
        // Space also cycles forward.
        let out = m.handle_key(key(KeyCode::Char(' ')), &[], "");
        assert_eq!(out, ModalOutcome::ChangeTheme(Theme::list()[1].to_string()));
    }

    #[test]
    fn settings_typing_routes_to_the_focused_text_row() {
        let mut m = on_settings("default", None, "");
        // Move to refresh_secs and type digits.
        m.handle_key(key(KeyCode::Down), &[], "");
        for c in "45".chars() {
            m.handle_key(key(KeyCode::Char(c)), &[], "");
        }
        assert_eq!(m.settings.refresh_secs(), Some(45));
        // The theme row took no text (it's a cycle, not a field).
        assert_eq!(m.settings.theme_name(), "default");
        // Move to the browser row and type a command (incl. an 's', which only
        // means "save" with Ctrl).
        m.handle_key(key(KeyCode::Down), &[], "");
        for c in "open -a Safari".chars() {
            m.handle_key(key(KeyCode::Char(c)), &[], "");
        }
        assert_eq!(m.settings.browser_command(), "open -a Safari");
    }

    #[test]
    fn settings_enter_and_ctrl_s_save_when_valid() {
        let mut m = on_settings("default", None, "");
        assert_eq!(
            m.handle_key(key(KeyCode::Enter), &[], ""),
            ModalOutcome::SaveSettings
        );
        let mut m2 = on_settings("default", None, "");
        assert_eq!(
            m2.handle_key(ctrl('s'), &[], ""),
            ModalOutcome::SaveSettings
        );
    }

    #[test]
    fn settings_save_rejects_non_numeric_refresh() {
        let mut m = on_settings("default", None, "");
        m.handle_key(key(KeyCode::Down), &[], ""); // → refresh row
        for c in "abc".chars() {
            m.handle_key(key(KeyCode::Char(c)), &[], "");
        }
        let out = m.handle_key(key(KeyCode::Enter), &[], "");
        assert_eq!(out, ModalOutcome::None, "invalid refresh blocks save");
        assert!(
            m.settings
                .message
                .as_deref()
                .unwrap()
                .contains("whole number"),
            "a validation message is shown"
        );
    }

    #[test]
    fn settings_refresh_secs_zero_and_empty_are_off() {
        let mut m = on_settings("default", Some(10), "");
        // Clear the field (Ctrl+U) → empty → off.
        m.handle_key(key(KeyCode::Down), &[], "");
        m.handle_key(ctrl('u'), &[], "");
        assert_eq!(m.settings.refresh_secs(), None);
        // An explicit 0 is also off.
        for c in "0".chars() {
            m.handle_key(key(KeyCode::Char(c)), &[], "");
        }
        assert_eq!(m.settings.refresh_secs(), None);
    }
}
