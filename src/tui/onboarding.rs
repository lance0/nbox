//! First-run onboarding wizard.
//!
//! When the TUI launches with no usable config (no file, no profiles, or no
//! resolvable active profile — see [`crate::config::needs_onboarding`]) we run a
//! short guided wizard instead of dying with "run `nbox config init`". It
//! captures one profile (name, url, token source, auth_scheme, verify_tls),
//! reusing Phase B's [`ProfileForm`]/`FormInput` add-form and its
//! test-connect/`verify_compatible` path verbatim — no re-implemented
//! form/validation/probe. On success it writes the profile via the same config
//! setters the editor uses ([`upsert_profile`](crate::config::upsert_profile) +
//! [`set_active_profile`](crate::config::set_active_profile) +
//! [`write_doc`](crate::config::write_doc)), optionally stores the token in the OS
//! keyring, and returns the chosen profile name so `run_tui` continues into the
//! normal `App` with that profile active.
//!
//! Keyring-unavailable path: a pasted token cannot be stored, so save is blocked
//! with guidance to use `token_env`/`NBOX_TOKEN` instead. Metadata-only profiles
//! can still be saved intentionally.
//!
//! The state + key handling here are PURE (no terminal, no network): [`handle_key`]
//! is a state transition returning a [`WizardAction`] the driver acts on, and
//! [`persist`] is a pure-as-possible save that returns what it did. Only [`run`]
//! touches the terminal + the test-connect probe (the wizard's lone network call).

use std::path::Path;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::DefaultTerminal;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use tokio::sync::mpsc;

use crate::netbox::auth::AuthScheme;
use crate::netbox::client::NetBoxClient;
use crate::tui::config_modal::{ProfileForm, TestState, TokenAction, field};
use crate::tui::events::{AbortOnDrop, spawn_terminal_events};
use crate::tui::profile_token::PreparedTokenChange;
use crate::tui::state::{AppEvent, ConnectRequest};
use crate::tui::theme::Theme;

/// What the driver should do after the wizard handles a key. The wizard stays
/// pure — it returns one of these and the driver performs the I/O (the
/// test-connect probe, the config/keyring write, exiting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardAction {
    /// Nothing to do (state changed in place, or the key was inert).
    None,
    /// Test-connect the form's current contents (builds a temp client + probes).
    TestConnect,
    /// Persist the form (config metadata + optional keyring) and finish.
    Save,
    /// Quit the wizard without writing anything (Esc / Ctrl+C).
    Quit,
}

/// The first-run wizard: Phase B's add-[`ProfileForm`] plus the latest
/// test-connect state. Mirrors the Config modal's form handling so the two share
/// behaviour (Tab field movement, Ctrl+S/Ctrl+L controls, Ctrl+T test, Enter
/// save) — onboarding is just the same form, standalone, before the `App` exists.
pub struct OnboardingWizard {
    /// The add-form, prefilled with a sensible default profile name.
    pub form: ProfileForm,
}

impl Default for OnboardingWizard {
    fn default() -> Self {
        Self::new()
    }
}

impl OnboardingWizard {
    /// A fresh wizard with the name field defaulted to `default`, focus on `url`
    /// so the user starts where the real input is.
    #[must_use]
    pub fn new() -> Self {
        let mut form = ProfileForm::add();
        // Seed a sensible default name; the user can change it.
        if let Some(input) = form.form_input_mut().input_mut(field::NAME) {
            input.set_value("default");
        }
        // Start focus on the url field (name already has a default).
        form.form_input_mut().focus_next();
        Self { form }
    }

    /// Handle a key, returning the [`WizardAction`] the driver should act on.
    /// PURE: mutates only the wizard's own state. Mirrors
    /// [`crate::tui::config_modal::ConfigModal`]'s form key handling.
    pub fn handle_key(&mut self, key: KeyEvent) -> WizardAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // A clean quit writes nothing (Esc, or Ctrl+C).
            KeyCode::Esc => WizardAction::Quit,
            KeyCode::Char('c') if ctrl => WizardAction::Quit,
            // Ctrl+S cycles the auth scheme; Ctrl+L toggles verify-tls — same as
            // the editor form, so the bare letters keep flowing into text fields.
            KeyCode::Char('s') if ctrl => {
                self.form.cycle_auth_scheme();
                WizardAction::None
            }
            KeyCode::Char('l') if ctrl => {
                self.form.toggle_verify_tls();
                WizardAction::None
            }
            KeyCode::Char('k') if ctrl => {
                self.form.toggle_token_store();
                WizardAction::None
            }
            // Ctrl+T tests the connection; Enter saves (and finishes).
            KeyCode::Char('t') if ctrl => match self.form.validate() {
                Ok(()) => {
                    self.form.test = TestState::Testing;
                    self.form.message = None;
                    WizardAction::TestConnect
                }
                Err(e) => {
                    self.form.message = Some(e);
                    WizardAction::None
                }
            },
            KeyCode::Enter => match validate_for_onboarding_save(&self.form) {
                Ok(()) => WizardAction::Save,
                Err(e) => {
                    self.form.message = Some(e);
                    WizardAction::None
                }
            },
            _ => {
                // Everything else is text editing / focus movement; an edit
                // invalidates a prior test result.
                if self.form.form_input_mut().handle_key(key) {
                    self.form.invalidate_test_public();
                }
                WizardAction::None
            }
        }
    }

    /// Build a [`ConnectRequest`] from the current form for a test-connect probe.
    /// The probe token uses the same precedence as the eventual launch (M15): the
    /// typed (masked) token wins, else the form's `token_env` (resolved from the
    /// environment), else `NBOX_TOKEN`. There's no profile yet, so there's no
    /// keyring tier here. The token is passed straight to the temp client, never
    /// logged.
    #[must_use]
    pub fn connect_request(&self) -> ConnectRequest {
        let token = self.form.token().or_else(|| {
            self.form
                .token_env()
                .and_then(|name| std::env::var(&name).ok())
                .filter(|t| !t.is_empty())
                .or_else(|| std::env::var("NBOX_TOKEN").ok().filter(|t| !t.is_empty()))
        });
        ConnectRequest {
            url: self.form.url(),
            auth_scheme: self.form.auth_scheme,
            verify_tls: self.form.verify_tls,
            token,
            // Onboarding has no saved profile yet, so there's no keyring tier.
            keyring_account: None,
        }
    }
}

/// Onboarding-specific save validation.
///
/// The shared profile form only checks syntax. First-run onboarding also has to
/// prevent a misleading "saved" path when the user pasted a token but this build
/// has no persistent keyring backend to store it.
fn validate_for_onboarding_save(form: &ProfileForm) -> Result<(), String> {
    form.validate()?;
    if form.token().is_some()
        && form.token_store == crate::config::TokenStore::Keyring
        && !crate::secret::keyring_available()
    {
        return Err(
            "this build can't store tokens in a keyring; switch token_store to config or use token_env"
                .to_string(),
        );
    }
    Ok(())
}

/// What [`persist`] did, so the driver can show the right status / steer the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistOutcome {
    /// The profile name that was written + made active.
    pub name: String,
    /// True when a typed token was stored in the OS keyring.
    pub stored_in_keyring: bool,
    /// `Some(var)` when the user gave a `token_env` (persisted to the file).
    pub token_env: Option<String>,
    /// True when no token landed anywhere (no keyring entry, no token_env): the
    /// driver tells the user to export `NBOX_TOKEN` / set a `token_env`.
    pub needs_env_guidance: bool,
}

/// Persist the wizard's profile. Pasted tokens are stored in config by default;
/// if the user explicitly selected keyring, prepare that write first, then write
/// metadata (format-preserving) via the same setters the editor uses. If the
/// metadata write fails, the keyring change is rolled back. The interactive
/// wizard validates the no-keyring case before calling this, and any runtime
/// keyring write failure keeps the user in the wizard instead of creating a
/// profile without the pasted token. Returns a [`PersistOutcome`] describing what
/// happened so the driver can guide the user when no token source was given.
/// Pure but for the file/keyring writes (mirroring the editor's profile save).
/// Metadata-only, config-token, and `token_env`-backed profiles save without
/// touching the keyring.
pub fn persist(form: &ProfileForm, path: &Path) -> Result<PersistOutcome> {
    let name = form.name();
    let url = form.url();
    let token_env = form.token_env();
    let auth_scheme = form.auth_scheme;
    let verify_tls = form.verify_tls;
    let token = form.token();
    let token_store = form.token_store;

    // Write the metadata (format-preserving), the same way `ProfileCommand::Add`
    // and the editor do: upsert + the field setters + activate.
    let mut doc = crate::config::load_doc_or_new(path)?;
    crate::config::upsert_profile(&mut doc, &name, &url, None)?;
    crate::config::set_profile_token_env(&mut doc, &name, token_env.as_deref())?;
    crate::config::set_profile_auth_scheme(&mut doc, &name, Some(auth_scheme))?;
    crate::config::set_profile_verify_tls(&mut doc, &name, Some(verify_tls))?;
    // The wizard renders the shared add-form, so its numeric fields (timeout_secs /
    // page_size) accept input — persist them too, the same way the editor does, so a
    // value typed during onboarding isn't silently dropped. The Ctrl-toggled knobs
    // (exclude / api) aren't reachable in the wizard, so they write their defaults
    // (exclude = the runtime default; REST ⇒ no `[api]` table).
    crate::config::set_profile_timeout_secs(&mut doc, &name, form.timeout_secs())?;
    crate::config::set_profile_page_size(&mut doc, &name, form.page_size())?;
    crate::config::set_profile_exclude_config_context(
        &mut doc,
        &name,
        Some(form.exclude_config_context),
    )?;
    crate::config::set_profile_api_backend(
        &mut doc,
        &name,
        crate::config::ApiSurface::Vrf,
        form.api_vrf,
    )?;
    crate::config::set_profile_api_backend(
        &mut doc,
        &name,
        crate::config::ApiSurface::RouteTarget,
        form.api_route_target,
    )?;
    crate::config::set_profile_token_store(&mut doc, &name, token_store)?;
    if token_store == crate::config::TokenStore::Config {
        crate::config::set_profile_token(&mut doc, &name, token.as_deref())?;
    } else {
        crate::config::set_profile_token(&mut doc, &name, None)?;
    }
    crate::config::set_active_profile(&mut doc, &name);

    let token_action = if token_store == crate::config::TokenStore::Keyring {
        token
            .as_ref()
            .map_or(TokenAction::Keep, |token| TokenAction::Set(token.clone()))
    } else {
        TokenAction::Keep
    };
    let token_change = PreparedTokenChange::prepare(path, None, &name, &token_action)?;
    if let Err(e) = crate::config::write_doc(path, &doc) {
        token_change.rollback();
        return Err(e);
    }
    let stored_in_keyring = token_change.stored_token();
    let _ = token_change.commit();
    let stored_in_config = token_store == crate::config::TokenStore::Config && token.is_some();

    // If nothing authenticatable landed — no keyring entry and no token_env — the
    // user must export NBOX_TOKEN or set a token_env to connect.
    let needs_env_guidance = !stored_in_keyring && !stored_in_config && token_env.is_none();

    Ok(PersistOutcome {
        name,
        stored_in_keyring,
        token_env,
        needs_env_guidance,
    })
}

/// Run the first-run wizard to completion, returning the [`PersistOutcome`] on a
/// successful save, or `None` when the user quit cleanly (nothing written).
///
/// Owns a minimal terminal event loop: it renders the form, routes keys through
/// the pure [`OnboardingWizard::handle_key`], and — for the one network action,
/// test-connect — spawns the same `verify_compatible` probe the editor's
/// test-connect uses (off the render thread, id-guarded so a superseded test is
/// dropped). On `Save` it [`persist`]s and returns the outcome so the caller can
/// surface env-var guidance when no token landed anywhere.
pub async fn run(
    terminal: &mut DefaultTerminal,
    path: &Path,
    theme: &Theme,
) -> Result<Option<PersistOutcome>> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(64);
    let _terminal_events = AbortOnDrop::new(spawn_terminal_events(tx.clone()));

    let mut wizard = OnboardingWizard::new();
    // Latest-test-wins guard: each test bumps `test_seq`; a `ConnectTested` with
    // an older id is from a superseded test (the form changed + re-tested).
    let mut test_seq: u64 = 0;

    loop {
        terminal.draw(|frame| render(frame, frame.area(), &mut wizard, theme))?;

        let Some(event) = rx.recv().await else {
            // The terminal event source ended — treat as a clean quit.
            return Ok(None);
        };
        match event {
            AppEvent::Key(key) => {
                // Snapshot whether a probe is in flight, so a probe-relevant edit
                // that supersedes it (the form drops back to Idle) bumps the test
                // id — dropping the now-stale in-flight result on arrival (H4).
                let was_testing = wizard.form.test == TestState::Testing;
                let action = wizard.handle_key(key);
                if was_testing && wizard.form.test == TestState::Idle {
                    test_seq += 1;
                }
                match action {
                    WizardAction::None => {}
                    WizardAction::Quit => return Ok(None),
                    WizardAction::TestConnect => {
                        test_seq += 1;
                        let id = test_seq;
                        let req = wizard.connect_request();
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            let result = probe(&req).await;
                            let _ = tx.send(AppEvent::ConnectTested { id, result }).await;
                        });
                    }
                    WizardAction::Save => {
                        let outcome = persist(&wizard.form, path)?;
                        // L8: when nothing authenticatable landed, surface the
                        // env-var guidance *in the wizard* (its final frame) rather
                        // than only after it exits, so the message is part of the
                        // onboarding flow the user is still looking at.
                        if outcome.needs_env_guidance {
                            wizard.form.message = Some(
                                "profile saved — set NBOX_TOKEN or a token_env to authenticate"
                                    .to_string(),
                            );
                            terminal.draw(|frame| {
                                render(frame, frame.area(), &mut wizard, theme);
                            })?;
                        }
                        return Ok(Some(outcome));
                    }
                }
            }
            AppEvent::ConnectTested { id, result } => {
                // Drop a superseded probe (the user edited + re-tested).
                if id < test_seq {
                    continue;
                }
                wizard.form.test = match result {
                    Ok(version) => TestState::Ok(version),
                    Err(e) => TestState::Failed(format!("{e:#}")),
                };
            }
            // Other events (ticks/resizes/etc.) just redraw on the next loop.
            _ => {}
        }
    }
}

/// Build a temporary client for `req` and probe the instance, returning its
/// NetBox version. Reuses [`NetBoxClient::new`] + [`NetBoxClient::verify_compatible`]
/// — the exact pair the editor's test-connect and a normal launch use — so the
/// wizard enforces the same reachability + version floor. The token is moved into
/// the client; it is never logged.
async fn probe(req: &ConnectRequest) -> Result<String> {
    let profile = req.to_profile();
    let client = NetBoxClient::new(&profile, req.resolved_token())?;
    let status = client.verify_compatible().await?;
    Ok(status.netbox_version)
}

/// Render the wizard: a centered, bordered panel with an intro line, the reused
/// profile form, the auth/tls controls, the test state, an optional message, and
/// the key hints. Mirrors the Config modal's profile-form rendering.
fn render(frame: &mut ratatui::Frame, area: Rect, wizard: &mut OnboardingWizard, theme: &Theme) {
    // L9: a too-small terminal can't fit the wizard's fixed row layout; show a
    // compact resize hint instead of a collapsed, garbled panel.
    if area.width < 24 || area.height < 12 {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "terminal too small — resize to set up nbox",
                Style::default().fg(theme.text_dim),
            )),
            area,
        );
        return;
    }
    // A roomy centered panel; clamp to the available area.
    let popup_w = 64.min(area.width);
    let popup_h = 17.min(area.height);
    let popup_x = area.x + area.width.saturating_sub(popup_w) / 2;
    let popup_y = area.y + area.height.saturating_sub(popup_h) / 2;
    let popup = Rect::new(popup_x, popup_y, popup_w, popup_h);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Welcome to nbox {} ", crate::VERSION))
        .title(
            Line::from(" Esc: quit ")
                .right_aligned()
                .style(theme.text_dim),
        )
        .border_style(Style::default().fg(theme.border_focused));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::vertical([
        Constraint::Length(2), // intro
        Constraint::Length(4), // the FormInput rows
        Constraint::Length(1), // auth_scheme
        Constraint::Length(1), // verify_tls
        Constraint::Length(1), // token_store
        Constraint::Length(1), // blank
        Constraint::Length(1), // test state
        Constraint::Length(1), // message
        Constraint::Min(1),    // help
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Let's set up your first NetBox profile.",
                Style::default().fg(theme.header),
            )),
            Line::from(Span::styled(
                "Paste a token to save it, name a token_env, or Ctrl+K for Keychain.",
                Style::default().fg(theme.text_dim),
            )),
        ]),
        rows[0],
    );

    if let Some(pos) = wizard.form.form_input_mut().render(frame, rows[1], theme) {
        frame.set_cursor_position(pos);
    }

    let scheme = match wizard.form.auth_scheme {
        AuthScheme::Auto => "auto",
        AuthScheme::Bearer => "bearer",
        AuthScheme::Token => "token",
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("auth_scheme  ", Style::default().fg(theme.header)),
            Span::styled(scheme, Style::default().fg(theme.accent)),
            Span::styled("  (Ctrl+S cycles)", Style::default().fg(theme.text_dim)),
        ])),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("verify_tls   ", Style::default().fg(theme.header)),
            Span::styled(
                if wizard.form.verify_tls { "on" } else { "off" },
                Style::default().fg(theme.accent),
            ),
            Span::styled("  (Ctrl+L toggles)", Style::default().fg(theme.text_dim)),
        ])),
        rows[3],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("token_store ", Style::default().fg(theme.header)),
            Span::styled(
                match wizard.form.token_store {
                    crate::config::TokenStore::Config => "config",
                    crate::config::TokenStore::Keyring => "keyring",
                },
                Style::default().fg(theme.accent),
            ),
            Span::styled("  (Ctrl+K toggles)", Style::default().fg(theme.text_dim)),
        ])),
        rows[4],
    );

    let (test_text, test_style) = match &wizard.form.test {
        TestState::Idle => (String::new(), Style::default().fg(theme.text_dim)),
        TestState::Testing => (
            "testing connection…".to_string(),
            Style::default().fg(theme.text_dim),
        ),
        TestState::Ok(v) => (
            format!("✓ connected (NetBox v{v})"),
            Style::default().fg(theme.success),
        ),
        TestState::Failed(e) => (format!("✗ {e}"), Style::default().fg(theme.error)),
    };
    frame.render_widget(Paragraph::new(Span::styled(test_text, test_style)), rows[6]);

    if let Some(msg) = &wizard.form.message {
        frame.render_widget(
            Paragraph::new(Span::styled(msg.clone(), Style::default().fg(theme.error))),
            rows[7],
        );
    }

    frame.render_widget(
        Paragraph::new(Span::styled(
            "Tab: field  Ctrl+T: test  Ctrl+K: token store  Enter: save & continue  Esc: quit",
            Style::default().fg(theme.text_dim),
        )),
        rows[8],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_into(w: &mut OnboardingWizard, s: &str) {
        for c in s.chars() {
            w.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn new_wizard_defaults_name_and_focuses_url() {
        let w = OnboardingWizard::new();
        assert_eq!(w.form.name(), "default");
        // Focus starts on the url field (name is pre-seeded).
        assert_eq!(w.form.form_input().focus(), field::URL);
    }

    #[test]
    fn esc_and_ctrl_c_quit_without_saving() {
        let mut w = OnboardingWizard::new();
        assert_eq!(w.handle_key(key(KeyCode::Esc)), WizardAction::Quit);
        let mut w2 = OnboardingWizard::new();
        assert_eq!(w2.handle_key(ctrl('c')), WizardAction::Quit);
    }

    #[test]
    fn enter_validates_then_saves() {
        let mut w = OnboardingWizard::new();
        // No url yet ⇒ validation error, no save.
        assert_eq!(w.handle_key(key(KeyCode::Enter)), WizardAction::None);
        assert!(w.form.message.as_deref().unwrap().contains("url"));
        // Type a valid url ⇒ Enter saves.
        type_into(&mut w, "https://nb.example");
        assert_eq!(w.handle_key(key(KeyCode::Enter)), WizardAction::Save);
    }

    #[test]
    fn enter_saves_pasted_token_with_default_config_storage() {
        let mut w = OnboardingWizard::new();
        type_into(&mut w, "https://nb.example");
        for _ in field::URL..field::TOKEN {
            w.handle_key(key(KeyCode::Tab));
        }
        type_into(&mut w, "nbt_secret");

        assert_eq!(w.handle_key(key(KeyCode::Enter)), WizardAction::Save);
    }

    #[test]
    fn enter_blocks_keyring_token_when_keyring_is_unavailable() {
        if crate::secret::keyring_available() {
            return;
        }
        let mut w = OnboardingWizard::new();
        type_into(&mut w, "https://nb.example");
        w.handle_key(ctrl('k')); // opt into keyring
        for _ in field::URL..field::TOKEN {
            w.handle_key(key(KeyCode::Tab));
        }
        type_into(&mut w, "nbt_secret");

        assert_eq!(w.handle_key(key(KeyCode::Enter)), WizardAction::None);
        assert!(
            w.form
                .message
                .as_deref()
                .is_some_and(|m| m.contains("can't store tokens in a keyring")),
            "message: {:?}",
            w.form.message
        );
    }

    #[test]
    fn ctrl_t_tests_when_valid_and_marks_testing() {
        let mut w = OnboardingWizard::new();
        type_into(&mut w, "https://nb.example");
        assert_eq!(w.handle_key(ctrl('t')), WizardAction::TestConnect);
        assert_eq!(w.form.test, TestState::Testing);
    }

    #[test]
    fn ctrl_s_cycles_auth_and_ctrl_l_toggles_tls() {
        let mut w = OnboardingWizard::new();
        assert_eq!(w.form.auth_scheme, AuthScheme::Auto);
        w.handle_key(ctrl('s'));
        assert_eq!(w.form.auth_scheme, AuthScheme::Bearer);
        assert!(w.form.verify_tls);
        w.handle_key(ctrl('l'));
        assert!(!w.form.verify_tls);
    }

    #[test]
    fn connect_request_carries_form_fields_and_typed_token() {
        let mut w = OnboardingWizard::new();
        type_into(&mut w, "https://nb.example");
        // Tab to the token field and type a token.
        for _ in field::URL..field::TOKEN {
            w.handle_key(key(KeyCode::Tab));
        }
        type_into(&mut w, "nbt_secret");
        let req = w.connect_request();
        assert_eq!(req.url, "https://nb.example");
        assert_eq!(req.auth_scheme, AuthScheme::Auto);
        assert!(req.verify_tls);
        assert_eq!(req.token.as_deref(), Some("nbt_secret"));
    }

    /// A temp config path under the OS temp dir, unique per test process + name.
    fn temp_config(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nbox-onboard-{}-{}", std::process::id(), tag));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    #[test]
    fn persist_writes_a_valid_active_profile() {
        let path = temp_config("valid");
        let _ = std::fs::remove_file(&path);
        let mut w = OnboardingWizard::new();
        type_into(&mut w, "https://nb.example");
        let outcome = persist(&w.form, &path).unwrap();
        assert_eq!(outcome.name, "default");

        // The on-disk config has the profile and names it active.
        let text = std::fs::read_to_string(&path).unwrap();
        let cfg: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg.active_profile.as_deref(), Some("default"));
        let prof = &cfg.profiles["default"];
        assert_eq!(prof.url, "https://nb.example");
        // No token was typed, so the file carries no token.
        assert!(!text.contains("token ="), "no token should be invented");
        // A freshly written config is no longer first-run.
        assert!(!crate::config::needs_onboarding(&path, None));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persist_writes_pasted_token_to_config_by_default() {
        let path = temp_config("token");
        let _ = std::fs::remove_file(&path);
        let mut w = OnboardingWizard::new();
        type_into(&mut w, "https://nb.example");
        for _ in field::URL..field::TOKEN {
            w.handle_key(key(KeyCode::Tab));
        }
        type_into(&mut w, "nbt_secret.value");

        let outcome = persist(&w.form, &path).unwrap();

        assert!(!outcome.stored_in_keyring);
        assert!(!outcome.needs_env_guidance);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("token = \"nbt_secret.value\""), "{text}");
        let cfg: Config = toml::from_str(&text).unwrap();
        assert_eq!(
            cfg.profiles["default"]
                .token
                .as_ref()
                .map(crate::config::ConfigToken::expose),
            Some("nbt_secret.value")
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persist_writes_typed_timeout_and_page_size() {
        // Regression: the wizard renders the shared add-form, so the timeout_secs /
        // page_size fields are visible + editable — a value typed there must persist,
        // not be silently dropped (it was, before the persist() setters were added).
        let path = temp_config("tuning");
        let _ = std::fs::remove_file(&path);
        let mut w = OnboardingWizard::new();
        for (idx, val) in [
            (field::URL, "https://nb.example"),
            (field::TIMEOUT_SECS, "30"),
            (field::PAGE_SIZE, "250"),
        ] {
            w.form
                .form_input_mut()
                .input_mut(idx)
                .unwrap()
                .set_value(val);
        }
        persist(&w.form, &path).unwrap();

        let cfg: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let prof = &cfg.profiles["default"];
        assert_eq!(prof.timeout_secs, Some(30), "typed timeout persisted");
        assert_eq!(prof.page_size, Some(250), "typed page_size persisted");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persist_no_keyring_with_token_env_completes_and_persists_env() {
        // A token_env (no typed token) is persisted; with no keyring entry the
        // outcome doesn't ask for env guidance (the token_env is the guidance).
        let path = temp_config("tokenenv");
        let _ = std::fs::remove_file(&path);
        let mut w = OnboardingWizard::new();
        type_into(&mut w, "https://nb.example");
        // Tab to token_env and set it.
        for _ in field::URL..field::TOKEN_ENV {
            w.handle_key(key(KeyCode::Tab));
        }
        type_into(&mut w, "NETBOX_TOKEN");
        let outcome = persist(&w.form, &path).unwrap();
        assert!(!outcome.stored_in_keyring);
        assert_eq!(outcome.token_env.as_deref(), Some("NETBOX_TOKEN"));
        assert!(
            !outcome.needs_env_guidance,
            "a token_env counts as guidance already given"
        );

        let cfg: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            cfg.profiles["default"].token_env.as_deref(),
            Some("NETBOX_TOKEN")
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persist_no_token_anywhere_requests_env_guidance() {
        // No typed token and no token_env: when the keyring is unavailable the
        // outcome flags that the user must export NBOX_TOKEN / set a token_env.
        // (Where a real keystore IS available, a typed token would store there;
        // here we type none, so guidance is requested regardless of backend.)
        let path = temp_config("noenv");
        let _ = std::fs::remove_file(&path);
        let mut w = OnboardingWizard::new();
        type_into(&mut w, "https://nb.example");
        let outcome = persist(&w.form, &path).unwrap();
        assert!(!outcome.stored_in_keyring);
        assert!(outcome.token_env.is_none());
        assert!(outcome.needs_env_guidance);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
