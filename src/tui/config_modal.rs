//! The in-app Config modal: pure state + key handling for both sections.
//!
//! A single modal with two sections (`Profiles` | `Settings`), `Tab` to switch.
//! The Profiles section lists the configured profiles (active marked) with add /
//! edit / select / delete actions, and an add/edit [`FormInput`] form whose token
//! field is masked (never written to TOML; stored in the OS keyring on save). The
//! Settings section is a small form over the *real* `[ui]` settings — theme (a
//! cycle), `refresh_secs` (numeric), and `open_browser_command` (text); the no-op
//! `confirm_writes` knob is deliberately excluded.
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

/// What the profile-save path should do with the OS-keyring token, derived from
/// the edit form's token field + clear intent (see [`ProfileForm::token_action`]).
/// The token value is never logged; only this intent crosses the pure/IO seam.
#[derive(Clone, PartialEq, Eq)]
pub enum TokenAction {
    /// Store this freshly-typed token under the (possibly renamed) keyring key.
    Set(String),
    /// Delete the stored keyring entry (the explicit `Ctrl+X` clear).
    Clear,
    /// Leave the stored token as-is (blank field, no clear intent). On a rename,
    /// the existing entry is migrated to the new key.
    Keep,
}

impl std::fmt::Debug for TokenAction {
    /// Redacts the token value: a `Set` shows as `Set(<redacted>)`, never the
    /// secret, so a `{:?}` of an outcome carrying it can't leak.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Set(_) => f.write_str("Set(<redacted>)"),
            Self::Clear => f.write_str("Clear"),
            Self::Keep => f.write_str("Keep"),
        }
    }
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
    /// "Clear the stored keyring token" intent, toggled with `Ctrl+X` on an edit
    /// form. The token field starts blank on edit (the secret is never read back),
    /// so blank-means-keep can't express "delete it"; this flag does. When set,
    /// save deletes the keyring entry. Typing a new token clears the flag (a value
    /// to store overrides a clear).
    pub clear_token: bool,
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
            clear_token: false,
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

    /// The token field's value, trimmed of surrounding whitespace, `None` when
    /// empty. Used only to hand straight to the keyring; it is never rendered (the
    /// field is masked) or logged. Trimming (L6) drops a trailing newline/space a
    /// paste can leave behind, which would otherwise break auth.
    pub fn token(&self) -> Option<String> {
        let v = self
            .inputs
            .value(field::TOKEN)
            .unwrap_or("")
            .trim()
            .to_string();
        if v.is_empty() { None } else { Some(v) }
    }

    /// What the save path should do with the keyring token, from the form state:
    /// a typed value ⇒ store it (overrides a pending clear); else the `Ctrl+X`
    /// clear intent ⇒ delete the entry; else ⇒ keep whatever is stored. PURE.
    pub fn token_action(&self) -> TokenAction {
        match self.token() {
            Some(t) => TokenAction::Set(t),
            None if self.clear_token => TokenAction::Clear,
            None => TokenAction::Keep,
        }
    }

    /// Toggle the "clear stored token" intent (`Ctrl+X`). A no-op model change;
    /// the deletion happens on save. Returns the new flag for the caller's hint.
    pub fn toggle_clear_token(&mut self) -> bool {
        self.clear_token = !self.clear_token;
        self.message = Some(if self.clear_token {
            "will clear the stored token on save".to_string()
        } else {
            "keeping the stored token".to_string()
        });
        self.clear_token
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
    /// describes the current contents) so the user can't save a stale OK. This
    /// resets to [`TestState::Idle`] even mid-test (H4): the in-flight probe is
    /// superseded by the edit, so its result must not be shown as if it matched
    /// the new contents. The driver also bumps the test generation id so a probe
    /// that lands after the edit is dropped on arrival.
    fn invalidate_test(&mut self) {
        self.test = TestState::Idle;
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

    /// [`validate`](Self::validate) plus a duplicate-name guard against `existing`
    /// (the live profile names). On *add* a name already in use is rejected; on
    /// *edit* the form's own original name is allowed (renaming to a *different*
    /// existing name is still rejected). Save runs this; test-connect uses the
    /// plain `validate` (a probe doesn't care about name collisions).
    pub fn validate_for_save(&self, existing: &[&str]) -> Result<(), String> {
        self.validate()?;
        let name = self.name();
        let collides = existing
            .iter()
            .any(|n| *n == name && self.editing.as_deref() != Some(*n));
        if collides {
            return Err(format!("a profile named '{name}' already exists"));
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
    /// The list selection to restore when a form/confirm is dismissed with `Esc`,
    /// so cancelling an add/edit returns to the row the user was on instead of
    /// snapping back to 0.
    pub last_selected: usize,
}

impl ProfilesPane {
    fn new() -> Self {
        Self {
            mode: ProfilesMode::List { selected: 0 },
            message: None,
            last_selected: 0,
        }
    }
}

/// A single setting field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingId {
    /// `[ui].theme` — a cycle (Left/Right/Space), hot-applied live.
    Theme,
    /// `[ui].refresh_secs` — numeric text (empty/0 = off).
    RefreshSecs,
    /// `[ui].open_browser_command` — free text (empty = OS default).
    OpenBrowserCommand,
    /// Top-level `log_level` — free text (empty = default; applies next launch).
    LogLevel,
    /// Top-level `log_file` — a path (empty = stderr only; applies next launch).
    LogFile,
    /// `[cache].enabled` — a toggle (Left/Right/Space), hot-applied on save.
    CacheEnabled,
    /// `[cache].ttl_secs` — numeric text (the read-cache de-dupe window).
    CacheTtl,
}

impl SettingId {
    /// The label shown beside the field in the fields pane.
    pub fn label(self) -> &'static str {
        match self {
            SettingId::Theme => "theme",
            SettingId::RefreshSecs => "refresh_secs",
            SettingId::OpenBrowserCommand => "open command",
            SettingId::LogLevel => "log_level",
            SettingId::LogFile => "log_file",
            SettingId::CacheEnabled => "cache",
            SettingId::CacheTtl => "cache_ttl",
        }
    }
}

/// Settings grouped into categories, in display order. Adding a setting is just
/// adding a field to a category here — the surface is data-driven, so it scales
/// without touching the navigation or render code.
pub const SETTINGS_CATEGORIES: &[(&str, &[SettingId])] = &[
    ("Appearance", &[SettingId::Theme]),
    (
        "Behavior",
        &[SettingId::RefreshSecs, SettingId::OpenBrowserCommand],
    ),
    ("Cache", &[SettingId::CacheEnabled, SettingId::CacheTtl]),
    ("Logging", &[SettingId::LogLevel, SettingId::LogFile]),
];

/// Which pane of the two-pane Settings section holds focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFocus {
    /// The left category list: `↑/↓` select, `→` enters the fields.
    Categories,
    /// The right field list: `↑/↓` move, edit in place, `Esc` back to categories.
    Fields,
}

/// The Settings section: a two-pane editor — categories on the left, the selected
/// category's fields on the right.
///
/// `theme` is a cycle (Left/Right/Space, hot-applied live — see
/// [`ModalOutcome::ChangeTheme`]); the rest are free-text [`TextInput`]s. In the
/// Categories pane `↑/↓` pick a category and `→` enters its fields; in the Fields
/// pane `↑/↓` move between fields and `Esc` returns to the categories. PURE +
/// unit-testable — no I/O; the app persists on [`ModalOutcome::SaveSettings`].
pub struct SettingsPane {
    /// Which pane has focus.
    pub focus: SettingsFocus,
    /// Selected category (index into [`SETTINGS_CATEGORIES`]).
    pub category: usize,
    /// Focused field within the selected category.
    pub field: usize,
    /// Index into [`Theme::list`] of the working theme selection.
    pub theme_index: usize,
    /// `refresh_secs` as typed (digits only; empty = off).
    pub refresh: TextInput,
    /// `open_browser_command` as typed (empty = OS default).
    pub browser: TextInput,
    /// `log_level` as typed (empty = default; a tracing filter like `nbox=debug`).
    pub log_level: TextInput,
    /// `log_file` as typed (empty = stderr only; a path).
    pub log_file: TextInput,
    /// `[cache].enabled` — the read-cache on/off toggle.
    pub cache_enabled: bool,
    /// `[cache].ttl_secs` as typed (digits; the engine clamps to 5–300s).
    pub cache_ttl: TextInput,
    /// A transient info / validation line shown under the form.
    pub message: Option<String>,
}

impl SettingsPane {
    /// Build the form seeded from the live settings.
    fn new(
        theme_name: &str,
        refresh_secs: Option<u64>,
        open_browser_command: &str,
        log_level: &str,
        log_file: &str,
        cache_enabled: bool,
        cache_ttl_secs: u64,
    ) -> Self {
        let mut refresh = TextInput::new("seconds (empty = off)");
        if let Some(secs) = refresh_secs.filter(|s| *s > 0) {
            refresh.set_value(secs.to_string());
        }
        let mut browser = TextInput::new("custom open command (empty = OS default)");
        if !open_browser_command.is_empty() {
            browser.set_value(open_browser_command);
        }
        let mut log_level_in = TextInput::new("e.g. info, nbox=debug (empty = default)");
        if !log_level.is_empty() {
            log_level_in.set_value(log_level);
        }
        let mut log_file_in = TextInput::new("path (empty = stderr only)");
        if !log_file.is_empty() {
            log_file_in.set_value(log_file);
        }
        let mut cache_ttl = TextInput::new("seconds (5–300)");
        cache_ttl.set_value(cache_ttl_secs.to_string());
        Self {
            focus: SettingsFocus::Categories,
            category: 0,
            field: 0,
            theme_index: Theme::index_of(theme_name),
            refresh,
            browser,
            log_level: log_level_in,
            log_file: log_file_in,
            cache_enabled,
            cache_ttl,
            message: None,
        }
    }

    /// The fields of the selected category.
    pub fn current_fields(&self) -> &'static [SettingId] {
        SETTINGS_CATEGORIES[self.category.min(SETTINGS_CATEGORIES.len() - 1)].1
    }

    /// The focused field id, when the Fields pane is active.
    pub fn focused_field(&self) -> Option<SettingId> {
        if self.focus == SettingsFocus::Fields {
            self.current_fields().get(self.field).copied()
        } else {
            None
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

    /// The typed `log_level`, trimmed; empty ⇒ `None` (unset).
    pub fn log_level_value(&self) -> Option<String> {
        let t = self.log_level.value().trim();
        (!t.is_empty()).then(|| t.to_string())
    }

    /// The typed `log_file`, trimmed; empty ⇒ `None` (unset).
    pub fn log_file_value(&self) -> Option<String> {
        let t = self.log_file.value().trim();
        (!t.is_empty()).then(|| t.to_string())
    }

    /// The working cache on/off state.
    pub fn cache_enabled(&self) -> bool {
        self.cache_enabled
    }

    /// The typed cache TTL in seconds; empty/invalid ⇒ the 30s default (the engine
    /// then clamps to 5–300). Validation on save rejects non-numeric text.
    pub fn cache_ttl_secs(&self) -> u64 {
        let t = self.cache_ttl.value().trim();
        t.parse::<u64>().unwrap_or(30)
    }

    /// Flip the cache on/off toggle (Left/Right/Space on the `cache` row).
    fn toggle_cache_enabled(&mut self) {
        self.cache_enabled = !self.cache_enabled;
        self.message = None;
    }

    /// The `TextInput` backing a field, if it has one (theme and the cache toggle
    /// are non-text controls).
    pub fn input_mut(&mut self, id: SettingId) -> Option<&mut TextInput> {
        match id {
            SettingId::Theme | SettingId::CacheEnabled => None,
            SettingId::RefreshSecs => Some(&mut self.refresh),
            SettingId::OpenBrowserCommand => Some(&mut self.browser),
            SettingId::LogLevel => Some(&mut self.log_level),
            SettingId::LogFile => Some(&mut self.log_file),
            SettingId::CacheTtl => Some(&mut self.cache_ttl),
        }
    }

    /// Select the next category (wrapping), resetting the field cursor.
    fn next_category(&mut self) {
        self.category = (self.category + 1) % SETTINGS_CATEGORIES.len();
        self.field = 0;
        self.message = None;
    }

    /// Select the previous category (wrapping), resetting the field cursor.
    fn prev_category(&mut self) {
        self.category = (self.category + SETTINGS_CATEGORIES.len() - 1) % SETTINGS_CATEGORIES.len();
        self.field = 0;
        self.message = None;
    }

    /// Move focus into the selected category's fields (no-op for an empty category).
    fn enter_fields(&mut self) {
        if !self.current_fields().is_empty() {
            self.focus = SettingsFocus::Fields;
            self.field = 0;
        }
    }

    /// Return focus to the category list.
    fn back_to_categories(&mut self) {
        self.focus = SettingsFocus::Categories;
    }

    /// Move to the next field within the category (wrapping).
    fn next_field(&mut self) {
        let n = self.current_fields().len();
        if n > 0 {
            self.field = (self.field + 1) % n;
        }
    }

    /// Move to the previous field within the category (wrapping).
    fn prev_field(&mut self) {
        let n = self.current_fields().len();
        if n > 0 {
            self.field = (self.field + n - 1) % n;
        }
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
        let c = self.cache_ttl.value().trim();
        if !c.is_empty() && c.parse::<u64>().is_err() {
            return Err("cache_ttl must be a whole number of seconds".to_string());
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
    pub fn new(
        theme_name: &str,
        refresh_secs: Option<u64>,
        open_browser_command: &str,
        log_level: &str,
        log_file: &str,
        cache_enabled: bool,
        cache_ttl_secs: u64,
    ) -> Self {
        Self {
            section: ConfigSection::Profiles,
            profiles: ProfilesPane::new(),
            settings: SettingsPane::new(
                theme_name,
                refresh_secs,
                open_browser_command,
                log_level,
                log_file,
                cache_enabled,
                cache_ttl_secs,
            ),
        }
    }
}

impl Default for ConfigModal {
    /// A modal seeded with default settings (the `default` theme, no auto-refresh,
    /// the OS default opener, no log overrides, cache on at 30s) — used by tests /
    /// when no live settings are handy.
    fn default() -> Self {
        Self::new("default", None, "", "", "", true, 30)
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
    /// delete of the active/last profile). Borrowed (`&[&str]`) so the caller can
    /// pass names that point straight into its own state without cloning each one
    /// per keystroke (M11).
    pub fn handle_key(&mut self, key: KeyEvent, profiles: &[&str], active: &str) -> ModalOutcome {
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
        // Section toggle is handled before the `settings` borrow so the Tab arm can
        // touch `self.section` cleanly. (Esc is context-sensitive — handled below.)
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.section = ConfigSection::Profiles;
            return ModalOutcome::None;
        }
        // Enter / Ctrl+S save the whole form from either pane (validate first).
        if key.code == KeyCode::Enter || (ctrl && key.code == KeyCode::Char('s')) {
            return match self.settings.validate() {
                Ok(()) => ModalOutcome::SaveSettings,
                Err(e) => {
                    self.settings.message = Some(e);
                    ModalOutcome::None
                }
            };
        }
        let s = &mut self.settings;
        match s.focus {
            // Left pane: pick a category, `→` enters its fields, `Esc` closes.
            SettingsFocus::Categories => match key.code {
                KeyCode::Esc => ModalOutcome::Close,
                KeyCode::Up => {
                    s.prev_category();
                    ModalOutcome::None
                }
                KeyCode::Down => {
                    s.next_category();
                    ModalOutcome::None
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    s.enter_fields();
                    ModalOutcome::None
                }
                _ => ModalOutcome::None,
            },
            // Right pane: `↑/↓` move fields, `Esc` steps back to the categories,
            // the theme field cycles with `←/→/Space`, text fields edit in place.
            SettingsFocus::Fields => {
                if key.code == KeyCode::Esc {
                    s.back_to_categories();
                    return ModalOutcome::None;
                }
                let is_cycle_key = matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                );
                if s.focused_field() == Some(SettingId::Theme) && is_cycle_key {
                    s.cycle_theme(!matches!(key.code, KeyCode::Left));
                    return ModalOutcome::ChangeTheme(s.theme_name().to_string());
                }
                // The cache on/off toggle flips with the same keys; it's applied on
                // save, so no live outcome (unlike the theme cycle).
                if s.focused_field() == Some(SettingId::CacheEnabled) && is_cycle_key {
                    s.toggle_cache_enabled();
                    return ModalOutcome::None;
                }
                match key.code {
                    KeyCode::Up => {
                        s.prev_field();
                        ModalOutcome::None
                    }
                    KeyCode::Down => {
                        s.next_field();
                        ModalOutcome::None
                    }
                    // Text editing on the focused text field (theme has none).
                    _ => {
                        if let Some(id) = s.focused_field()
                            && let Some(input) = s.input_mut(id)
                            && input.handle_key(key)
                        {
                            s.message = None;
                        }
                        ModalOutcome::None
                    }
                }
            }
        }
    }

    fn handle_profiles_key(
        &mut self,
        key: KeyEvent,
        profiles: &[&str],
        active: &str,
    ) -> ModalOutcome {
        // Take the mode out so we can match it by value and rebuild it; this keeps
        // each arm a clean state transition.
        match &mut self.profiles.mode {
            ProfilesMode::List { .. } => self.handle_list_key(key, profiles, active),
            ProfilesMode::Form(_) => self.handle_form_key(key, profiles),
            ProfilesMode::ConfirmDelete { .. } => self.handle_confirm_key(key, active),
        }
    }

    fn handle_list_key(&mut self, key: KeyEvent, profiles: &[&str], active: &str) -> ModalOutcome {
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
                // Remember the row to come back to if the add is cancelled.
                self.profiles.last_selected = *selected;
                self.profiles.mode = ProfilesMode::Form(ProfileForm::add());
                ModalOutcome::None
            }
            KeyCode::Char('e') => {
                // Edit the selected profile: surface the request; the app opens the
                // prefilled form via `open_edit_form` (it owns the ProfileConfig),
                // keeping this module pure. Remember the row to restore on cancel.
                let sel = *selected;
                profiles.get(sel).map_or(ModalOutcome::None, |name| {
                    self.profiles.last_selected = sel;
                    ModalOutcome::Edit((*name).to_string())
                })
            }
            KeyCode::Enter | KeyCode::Char('s') => {
                profiles.get(*selected).map_or(ModalOutcome::None, |name| {
                    ModalOutcome::Select((*name).to_string())
                })
            }
            KeyCode::Char('d') => {
                let sel = *selected;
                let Some(name) = profiles.get(sel).map(|n| (*n).to_string()) else {
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

    fn handle_form_key(&mut self, key: KeyEvent, profiles: &[&str]) -> ModalOutcome {
        let ProfilesMode::Form(form) = &mut self.profiles.mode else {
            return ModalOutcome::None;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                // Back to the list, discarding the form — restoring the row the form
                // was opened from (not snapping back to 0).
                self.profiles.mode = ProfilesMode::List {
                    selected: self.profiles.last_selected,
                };
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
            // Ctrl+T tests the connection; Enter saves; Ctrl+G saves + uses it.
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
            KeyCode::Enter => match form.validate_for_save(profiles) {
                Ok(()) => ModalOutcome::Save { use_it: false },
                Err(e) => {
                    form.message = Some(e);
                    ModalOutcome::None
                }
            },
            // Ctrl+G saves + uses it (Ctrl+U is the field clear-line in the text
            // inputs, so it can't double as save+use).
            KeyCode::Char('g') if ctrl => match form.validate_for_save(profiles) {
                Ok(()) => ModalOutcome::Save { use_it: true },
                Err(e) => {
                    form.message = Some(e);
                    ModalOutcome::None
                }
            },
            // Ctrl+X toggles "clear the stored keyring token on save". Only
            // meaningful while editing (a fresh add has no stored token to clear),
            // but harmless on add — token_action ignores Clear when a value is typed.
            KeyCode::Char('x') if ctrl => {
                form.toggle_clear_token();
                ModalOutcome::None
            }
            _ => {
                // Everything else is text editing / focus movement; an edit
                // invalidates a prior test result.
                if form.inputs.handle_key(key) {
                    form.invalidate_test();
                    // Typing into the token field overrides a pending clear: a value
                    // to store wins. (Other fields don't touch the token intent.)
                    if form.inputs.focus() == field::TOKEN && form.token().is_some() {
                        form.clear_token = false;
                    }
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
        let names = vec!["a", "b", "c"];
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
        let names = vec!["a", "b"];
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
        // Ctrl+G saves + uses it (Ctrl+U is the field clear-line, not save+use).
        let out = m.handle_key(ctrl('g'), &[], "");
        assert_eq!(out, ModalOutcome::Save { use_it: true });
    }

    #[test]
    fn ctrl_u_clears_the_focused_field_not_save_and_use() {
        // M4: Ctrl+U must clear the focused text line (cheese TextInput behavior),
        // not trigger save+use — that's now Ctrl+G.
        let mut m = ConfigModal::default();
        m.handle_key(key(KeyCode::Char('a')), &[], "");
        type_into(&mut m, "lab");
        assert_eq!(m.form().unwrap().name(), "lab");
        let out = m.handle_key(ctrl('u'), &[], "");
        assert_eq!(out, ModalOutcome::None, "Ctrl+U is not save+use");
        assert_eq!(
            m.form().unwrap().name(),
            "",
            "Ctrl+U cleared the name field"
        );
    }

    #[test]
    fn token_field_is_trimmed_so_a_pasted_newline_does_not_break_auth() {
        // L6: a trailing space/newline from a paste is trimmed off the token.
        let mut m = ConfigModal::default();
        m.handle_key(key(KeyCode::Char('a')), &[], "");
        for _ in 0..field::TOKEN {
            m.handle_key(key(KeyCode::Tab), &[], "");
        }
        type_into(&mut m, "  nbt_secret  ");
        assert_eq!(m.form().unwrap().token().as_deref(), Some("nbt_secret"));
    }

    #[test]
    fn token_action_models_set_clear_and_keep() {
        // Edit form: token blank, no clear intent ⇒ Keep.
        let mut m = ConfigModal::default();
        m.open_edit_form("work", "https://w", None, AuthScheme::Auto, true);
        assert_eq!(m.form().unwrap().token_action(), TokenAction::Keep);
        // Ctrl+X toggles the clear intent ⇒ Clear.
        m.handle_key(ctrl('x'), &[], "");
        assert_eq!(m.form().unwrap().token_action(), TokenAction::Clear);
        // Tab to the token field and type a value ⇒ Set wins over a pending clear.
        for _ in 0..field::TOKEN {
            m.handle_key(key(KeyCode::Tab), &[], "");
        }
        type_into(&mut m, "nbt_new");
        assert_eq!(
            m.form().unwrap().token_action(),
            TokenAction::Set("nbt_new".to_string())
        );
        assert!(
            !m.form().unwrap().clear_token,
            "typing a token clears the flag"
        );
        // The Debug of a Set never leaks the value.
        assert_eq!(
            format!("{:?}", m.form().unwrap().token_action()),
            "Set(<redacted>)"
        );
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
    fn add_rejects_a_duplicate_name_but_edit_keeps_its_own() {
        // M5: adding a profile whose name already exists is rejected on save.
        let names = vec!["work", "lab"];
        let mut m = ConfigModal::default();
        m.handle_key(key(KeyCode::Char('a')), &names, "work"); // add form
        type_into(&mut m, "work"); // name collides with an existing profile
        m.handle_key(key(KeyCode::Tab), &names, "work");
        type_into(&mut m, "https://nb"); // valid url
        let out = m.handle_key(key(KeyCode::Enter), &names, "work");
        assert_eq!(out, ModalOutcome::None, "duplicate name blocks save");
        assert!(
            m.form()
                .unwrap()
                .message
                .as_deref()
                .unwrap()
                .contains("already exists")
        );
        // Editing 'work' and saving under its own name is allowed.
        let mut m2 = ConfigModal::default();
        m2.open_edit_form("work", "https://w", None, AuthScheme::Auto, true);
        let out = m2.handle_key(key(KeyCode::Enter), &names, "work");
        assert_eq!(out, ModalOutcome::Save { use_it: false });
        // …but renaming 'work' to the *other* existing name 'lab' is rejected.
        let mut m3 = ConfigModal::default();
        m3.open_edit_form("work", "https://w", None, AuthScheme::Auto, true);
        // Clear the name field and retype 'lab'.
        m3.handle_key(ctrl('u'), &names, "work");
        type_into(&mut m3, "lab");
        let out = m3.handle_key(key(KeyCode::Enter), &names, "work");
        assert_eq!(
            out,
            ModalOutcome::None,
            "rename onto another profile is blocked"
        );
    }

    #[test]
    fn esc_from_form_restores_the_prior_list_selection() {
        // M6: cancelling an add/edit returns to the row the form was opened from.
        let names = vec!["a", "b", "c"];
        let mut m = ConfigModal::default();
        m.handle_key(key(KeyCode::Char('j')), &names, "a"); // → 1
        m.handle_key(key(KeyCode::Char('j')), &names, "a"); // → 2
        m.handle_key(key(KeyCode::Char('a')), &names, "a"); // open add form
        assert!(matches!(m.profiles.mode, ProfilesMode::Form(_)));
        m.handle_key(key(KeyCode::Esc), &names, "a"); // cancel
        assert!(
            matches!(m.profiles.mode, ProfilesMode::List { selected: 2 }),
            "selection restored to row 2, not reset to 0"
        );
    }

    #[test]
    fn delete_confirm_then_y_deletes_a_non_active_profile() {
        let mut m = ConfigModal::default();
        let names = vec!["a", "b"];
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
        let names = vec!["a", "b"];
        m.handle_key(key(KeyCode::Char('d')), &names, "a"); // a is active
        assert!(
            matches!(m.profiles.mode, ProfilesMode::List { .. }),
            "deleting the active profile does not open a confirm"
        );
        // Last/only profile: d is a no-op too.
        let mut m2 = ConfigModal::default();
        let one = vec!["only"];
        m2.handle_key(key(KeyCode::Char('d')), &one, "other");
        assert!(matches!(m2.profiles.mode, ProfilesMode::List { .. }));
    }

    #[test]
    fn confirm_delete_n_cancels_back_to_list() {
        let mut m = ConfigModal::default();
        let names = vec!["a", "b"];
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
        let names = vec!["a", "b"];
        m.handle_key(key(KeyCode::Char('j')), &names, "a"); // → b
        let out = m.handle_key(key(KeyCode::Char('e')), &names, "a");
        assert_eq!(out, ModalOutcome::Edit("b".to_string()));
    }

    // ---- Settings section ----------------------------------------------------

    /// Open a modal and switch it to the Settings section (Categories pane).
    fn on_settings(theme: &str, refresh: Option<u64>, browser: &str) -> ConfigModal {
        let mut m = ConfigModal::new(theme, refresh, browser, "", "", true, 30);
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
    fn settings_seeds_and_reads_log_fields() {
        let mut m = ConfigModal::new("default", None, "", "nbox=debug", "/tmp/x.log", true, 30);
        m.handle_key(key(KeyCode::Tab), &[], "");
        assert_eq!(m.settings.log_level_value().as_deref(), Some("nbox=debug"));
        assert_eq!(m.settings.log_file_value().as_deref(), Some("/tmp/x.log"));
        // Empty seeds read back as None.
        let blank = on_settings("default", None, "");
        assert_eq!(blank.settings.log_level_value(), None);
        assert_eq!(blank.settings.log_file_value(), None);
    }

    #[test]
    fn settings_tab_switches_back_to_profiles_and_esc_closes() {
        let mut m = on_settings("default", None, "");
        m.handle_key(key(KeyCode::Tab), &[], "");
        assert_eq!(m.section, ConfigSection::Profiles);
        // Esc from the Categories pane closes the whole modal.
        let mut m2 = on_settings("default", None, "");
        assert_eq!(
            m2.handle_key(key(KeyCode::Esc), &[], ""),
            ModalOutcome::Close
        );
    }

    #[test]
    fn settings_categories_and_fields_navigate() {
        let mut m = on_settings("default", None, "");
        // Starts in the Categories pane on the first category (Appearance).
        assert_eq!(m.settings.focus, SettingsFocus::Categories);
        assert_eq!(m.settings.category, 0);
        // Down cycles through the categories (wrapping at the last).
        m.handle_key(key(KeyCode::Down), &[], "");
        assert_eq!(m.settings.category, 1); // Behavior
        m.handle_key(key(KeyCode::Down), &[], "");
        assert_eq!(m.settings.category, 2); // Cache
        m.handle_key(key(KeyCode::Down), &[], "");
        assert_eq!(m.settings.category, 3); // Logging
        m.handle_key(key(KeyCode::Down), &[], ""); // wraps
        assert_eq!(m.settings.category, 0);
        m.handle_key(key(KeyCode::Up), &[], ""); // wraps back to Logging
        assert_eq!(m.settings.category, 3);
        // Back to Behavior, then → enters its fields; Down moves; Esc returns.
        m.handle_key(key(KeyCode::Up), &[], "");
        assert_eq!(m.settings.category, 2); // Cache
        m.handle_key(key(KeyCode::Up), &[], "");
        assert_eq!(m.settings.category, 1); // Behavior
        m.handle_key(key(KeyCode::Right), &[], "");
        assert_eq!(m.settings.focus, SettingsFocus::Fields);
        assert_eq!(m.settings.focused_field(), Some(SettingId::RefreshSecs));
        m.handle_key(key(KeyCode::Down), &[], "");
        assert_eq!(
            m.settings.focused_field(),
            Some(SettingId::OpenBrowserCommand)
        );
        m.handle_key(key(KeyCode::Esc), &[], "");
        assert_eq!(m.settings.focus, SettingsFocus::Categories);
    }

    #[test]
    fn settings_theme_cycles_and_emits_change_theme() {
        let mut m = on_settings("default", None, "");
        // Enter the Appearance fields (theme) before cycling.
        assert_eq!(
            m.handle_key(key(KeyCode::Right), &[], ""),
            ModalOutcome::None
        );
        assert_eq!(m.settings.focused_field(), Some(SettingId::Theme));
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
    fn settings_typing_routes_to_the_focused_field() {
        let mut m = on_settings("default", None, "");
        // Behavior → refresh_secs: enter the fields and type digits.
        m.handle_key(key(KeyCode::Down), &[], ""); // → Behavior
        m.handle_key(key(KeyCode::Right), &[], ""); // enter fields (refresh_secs)
        for c in "45".chars() {
            m.handle_key(key(KeyCode::Char(c)), &[], "");
        }
        assert_eq!(m.settings.refresh_secs(), Some(45));
        // Down → open command; type a command (incl. an 's', which only saves with Ctrl).
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
        m.handle_key(key(KeyCode::Down), &[], ""); // → Behavior
        m.handle_key(key(KeyCode::Right), &[], ""); // enter fields (refresh_secs)
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
        m.handle_key(key(KeyCode::Down), &[], ""); // → Behavior
        m.handle_key(key(KeyCode::Right), &[], ""); // enter fields (refresh_secs)
        // Clear the field (Ctrl+U) → empty → off.
        m.handle_key(ctrl('u'), &[], "");
        assert_eq!(m.settings.refresh_secs(), None);
        // An explicit 0 is also off.
        for c in "0".chars() {
            m.handle_key(key(KeyCode::Char(c)), &[], "");
        }
        assert_eq!(m.settings.refresh_secs(), None);
    }
}
