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
//! [`write_doc`](crate::config::write_doc)), writing a pasted token to
//! `config.toml`, and returns the chosen profile name so `run_tui` continues into
//! the normal `App` with that profile active.
//!
//! No-token path: a profile saved with neither a pasted token nor a `token_env`
//! completes, with guidance to export `NBOX_TOKEN` / set a `token_env` to connect.
//!
//! The state + key handling here are PURE (no terminal, no network): [`handle_key`]
//! is a state transition returning a [`WizardAction`] the driver acts on, and
//! [`persist`] is a pure-as-possible save that returns what it did. Only [`run`]
//! touches the terminal + the test-connect probe (the wizard's lone network call).

use std::path::Path;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use tokio::sync::mpsc;

use crate::netbox::auth::AuthScheme;
use crate::netbox::client::NetBoxClient;
use crate::tui::config_modal::{ProfileForm, TestState, field};
use crate::tui::events::{AbortOnDrop, spawn_terminal_events};
use crate::tui::state::{AppEvent, ConnectRequest};
use crate::tui::theme::Theme;

/// What the driver should do after the wizard handles a key. The wizard stays
/// pure — it returns one of these and the driver performs the I/O (the
/// test-connect probe, the config write, exiting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardAction {
    /// Nothing to do (state changed in place, or the key was inert).
    None,
    /// Test-connect the form's current contents (builds a temp client + probes).
    TestConnect,
    /// Persist the form (config metadata + optional token) and finish.
    Save,
    /// Quit the wizard without writing anything (Esc / Ctrl+C).
    Quit,
}

/// The first-run wizard: Phase B's add-[`ProfileForm`] plus the latest
/// test-connect state. Mirrors the Config modal's form handling so the two share
/// behaviour (Tab field movement, Ctrl+S/Ctrl+L controls, Ctrl+T test, Enter
/// save) — onboarding is just the same form, standalone, before the `App` exists.
pub struct OnboardingWizard {
    /// The reused add-[`ProfileForm`]. The name field starts blank — it's derived
    /// from the url host on save (see [`suggest_name_from_url`]), so onboarding
    /// never plants a stray `default` profile.
    pub form: ProfileForm,
    /// When `true`, the advanced fields (token_env, auth_scheme, verify_tls) show
    /// and Tab reaches them; toggled with `Ctrl+A`. Off by default, so the first
    /// screen is just url / name / token.
    pub advanced: bool,
}

impl Default for OnboardingWizard {
    fn default() -> Self {
        Self::new()
    }
}

impl OnboardingWizard {
    /// A fresh wizard focused on `url` — the first thing the user supplies, and
    /// what the profile name is derived from. The name field is left blank (its
    /// live placeholder shows the suggestion); a still-blank name is filled from
    /// the url on save. Advanced fields start collapsed.
    #[must_use]
    pub fn new() -> Self {
        let mut form = ProfileForm::add();
        form.form_input_mut().set_focus(field::URL);
        Self {
            form,
            advanced: false,
        }
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
            // Ctrl+A reveals/hides the advanced fields (token_env, auth_scheme,
            // verify_tls). Collapsing pulls focus back if it sat on a hidden field.
            KeyCode::Char('a') if ctrl => {
                self.advanced = !self.advanced;
                self.clamp_focus();
                WizardAction::None
            }
            // Tab/Shift-Tab walk the *visible* fields in display order, so a hidden
            // token_env is skipped (the form's raw `focus_next` would land on it).
            KeyCode::Tab => {
                self.focus_step(true);
                WizardAction::None
            }
            KeyCode::BackTab => {
                self.focus_step(false);
                WizardAction::None
            }
            // Ctrl+T tests the connection; Enter saves (and finishes). Both treat
            // the name as optional — a blank name is filled from the url suggestion
            // so the user only has to supply a url (test reverts it, since the probe
            // ignores the name and the live suggestion should stay visible).
            KeyCode::Char('t') if ctrl => match self.validate_optional_name(false) {
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
            KeyCode::Enter => match self.validate_optional_name(true) {
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
    /// The probe token uses the same normalized precedence as the eventual launch
    /// (M15) via the shared helper: the typed (masked) token wins, else the form's
    /// `token_env` (resolved from the environment), else `NBOX_TOKEN`. There's no
    /// saved profile yet, so there's no config token. The token is passed straight
    /// to the temp client, never logged.
    #[must_use]
    pub fn connect_request(&self) -> ConnectRequest {
        let typed = self.form.token();
        let token_env = self.form.token_env();
        let token =
            crate::config::resolve_probe_token(typed.as_deref(), token_env.as_deref(), None);
        ConnectRequest {
            url: self.form.url(),
            auth_scheme: self.form.auth_scheme,
            verify_tls: self.form.verify_tls,
            token,
        }
    }

    /// The text fields shown (in display order) for the current mode. Simple mode
    /// hides `token_env`; both modes hide the numeric tuning fields (`timeout_secs`/
    /// `page_size` — edited in the config file or the Settings modal). `url` leads.
    fn visible_fields(&self) -> &'static [usize] {
        if self.advanced {
            &[field::URL, field::NAME, field::TOKEN, field::TOKEN_ENV]
        } else {
            &[field::URL, field::NAME, field::TOKEN]
        }
    }

    /// Move focus to the next (`forward`) or previous visible field, wrapping at
    /// both ends.
    fn focus_step(&mut self, forward: bool) {
        let order = self.visible_fields();
        let len = order.len();
        if len == 0 {
            return;
        }
        let cur = self.form.form_input().focus();
        let pos = order.iter().position(|&f| f == cur).unwrap_or(0);
        let next = if forward {
            (pos + 1) % len
        } else {
            (pos + len - 1) % len
        };
        self.form.form_input_mut().set_focus(order[next]);
    }

    /// After collapsing the advanced fields, pull focus back to `url` if it sat on
    /// a now-hidden field.
    fn clamp_focus(&mut self) {
        let order = self.visible_fields();
        let cur = self.form.form_input().focus();
        if !order.contains(&cur) {
            self.form.form_input_mut().set_focus(order[0]);
        }
    }

    /// Fill the name from the url-derived suggestion when it's blank; returns
    /// whether it set a value (so the caller can revert it). Never overwrites a
    /// name the user typed.
    fn fill_name_from_url_if_blank(&mut self) -> bool {
        if self.form.name().trim().is_empty() {
            let suggestion = suggest_name_from_url(&self.form.url());
            if let Some(input) = self.form.form_input_mut().input_mut(field::NAME) {
                input.set_value(suggestion);
            }
            true
        } else {
            false
        }
    }

    /// Validate with the name treated as optional: fill it from the url suggestion
    /// when blank, so the shared [`ProfileForm::validate`] (which requires a name)
    /// passes on a url alone. With `commit`, a successful validation keeps the
    /// auto-filled name (Enter → save); otherwise — or on failure — it's reverted
    /// so the live placeholder suggestion stays (test ignores the name, and a
    /// failed save shouldn't strand a half-derived name in the field).
    fn validate_optional_name(&mut self, commit: bool) -> Result<(), String> {
        let auto_filled = self.fill_name_from_url_if_blank();
        let result = self.form.validate();
        if auto_filled
            && (!commit || result.is_err())
            && let Some(input) = self.form.form_input_mut().input_mut(field::NAME)
        {
            input.set_value("");
        }
        result
    }
}

/// Suggest a profile name from a NetBox URL's host: strip the scheme and any
/// port/path, drop a single leading `www.`/`netbox.` label, take the next DNS
/// label, and keep only `[a-z0-9-]` (lowercased). Falls back to `prod` for an
/// empty host or a bare IP address — so the wizard never has to invent `default`.
///
/// `https://netbox.acme.com` → `acme`; `https://acme.example.com:8443/x` → `acme`;
/// `https://10.0.0.5` / `https://[::1]:8080` → `prod`.
fn suggest_name_from_url(url: &str) -> String {
    let fallback = || "prod".to_string();
    // Authority = between the scheme and the first '/', '?' or '#'.
    let after_scheme = url.split("://").nth(1).unwrap_or(url).trim();
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .trim();
    // Bracketed IPv6 (`[::1]`) has no usable name.
    if authority.is_empty() || authority.starts_with('[') {
        return fallback();
    }
    // Strip a `host:port` suffix (host has no colon; IPv6 was handled above).
    let host = authority.split(':').next().unwrap_or("");
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.is_empty() {
        return fallback();
    }
    // A bare IPv4 (all-numeric labels) has no usable name.
    if labels.iter().all(|l| l.chars().all(|c| c.is_ascii_digit())) {
        return fallback();
    }
    // Drop one leading generic label so `netbox.acme.com` → `acme`.
    let start = match labels.first().copied() {
        Some("www" | "netbox") if labels.len() > 1 => 1,
        _ => 0,
    };
    let cleaned: String = labels[start]
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .to_ascii_lowercase();
    if cleaned.is_empty() {
        fallback()
    } else {
        cleaned
    }
}

/// The display label for a form field index (the wizard lays fields out itself).
fn field_label(idx: usize) -> &'static str {
    match idx {
        field::URL => "url",
        field::NAME => "name",
        field::TOKEN => "token",
        field::TOKEN_ENV => "token_env",
        _ => "",
    }
}

/// What [`persist`] did, so the driver can show the right status / steer the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistOutcome {
    /// The profile name that was written + made active.
    pub name: String,
    /// True when a typed token was stored in `config.toml`.
    pub stored_in_config: bool,
    /// `Some(var)` when the user gave a `token_env` (persisted to the file).
    pub token_env: Option<String>,
    /// True when no token landed anywhere (no config token, no token_env): the
    /// driver tells the user to export `NBOX_TOKEN` / set a `token_env`.
    pub needs_env_guidance: bool,
}

/// Persist the wizard's profile. A pasted token is written to `config.toml`
/// (`token = "…"`, `0600` on Unix, redacted in display) via the same setters the
/// editor uses; metadata-only and `token_env`-backed profiles just skip it.
/// Returns a [`PersistOutcome`] describing what happened so the driver can guide
/// the user when no token source was given. Pure but for the file write
/// (mirroring the editor's profile save).
pub fn persist(form: &ProfileForm, path: &Path) -> Result<PersistOutcome> {
    // The wizard leaves the name blank to mean "derive it from the url"; resolve
    // it here too (not only in the Enter handler) so a direct save still names the
    // profile sensibly instead of writing an empty name.
    let name = {
        let typed = form.name();
        let typed = typed.trim();
        if typed.is_empty() {
            suggest_name_from_url(&form.url())
        } else {
            typed.to_string()
        }
    };
    let url = form.url();
    let token_env = form.token_env();
    let auth_scheme = form.auth_scheme;
    let verify_tls = form.verify_tls;
    let token = form.token();

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
    crate::config::set_profile_token(&mut doc, &name, token.as_deref())?;
    crate::config::set_active_profile(&mut doc, &name);
    crate::config::write_doc(path, &doc)?;

    let stored_in_config = token.is_some();
    // If nothing authenticatable landed — no config token and no token_env — the
    // user must export NBOX_TOKEN or set a token_env to connect.
    let needs_env_guidance = !stored_in_config && token_env.is_none();

    Ok(PersistOutcome {
        name,
        stored_in_config,
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
    // Keep the name field's placeholder in sync with the live url-derived
    // suggestion, so an empty name shows what it will become on save.
    let suggestion = suggest_name_from_url(&wizard.form.url());
    if let Some(input) = wizard.form.form_input_mut().input_mut(field::NAME) {
        input.set_placeholder(format!("{suggestion}   (suggested)"));
    }

    // The panel grows when the advanced fields are showing.
    let visible = wizard.visible_fields();
    let advanced_rows = if wizard.advanced { 2 } else { 0 };
    // intro + blank + fields + [auth + verify] + hint + blank + test + message + footer
    let content_h = 1 + 1 + visible.len() as u16 + advanced_rows + 1 + 1 + 1 + 1 + 1;
    let popup_w = 64.min(area.width);
    let popup_h = (content_h + 2).min(area.height);
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

    let mut y = inner.y;

    // Intro (one line), then a blank row.
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Set up your first NetBox profile.",
            Style::default().fg(theme.header),
        )),
        Rect::new(inner.x, y, inner.width, 1),
    );
    y = y.saturating_add(2);

    // Label column width: the widest visible label, plus the advanced labels when
    // they're showing, so the value cells line up.
    let mut label_w = visible
        .iter()
        .map(|&i| field_label(i).len())
        .max()
        .unwrap_or(0);
    if wizard.advanced {
        label_w = label_w.max("auth_scheme".len());
    }

    // The text fields, in display order: a label cell + the field's value cell.
    let mut cursor = None;
    for &idx in visible {
        if y >= inner.bottom() {
            break;
        }
        let label_cell = format!("{:<label_w$}  ", field_label(idx));
        let lw = label_cell.chars().count() as u16;
        frame.render_widget(
            Paragraph::new(Span::styled(
                label_cell,
                Style::default().fg(theme.text_dim),
            )),
            Rect::new(inner.x, y, lw.min(inner.width), 1),
        );
        let value_x = inner.x.saturating_add(lw);
        let value_w = inner.width.saturating_sub(lw);
        if value_w > 0 {
            let focused = wizard.form.form_input().focus() == idx;
            if let Some(input) = wizard.form.form_input_mut().input_mut(idx)
                && let Some(pos) =
                    input.render_value(frame, Rect::new(value_x, y, value_w, 1), theme, focused)
            {
                cursor = Some(pos);
            }
        }
        y = y.saturating_add(1);
    }

    // Advanced controls (auth_scheme, verify_tls) — only when expanded.
    if wizard.advanced {
        let scheme = match wizard.form.auth_scheme {
            AuthScheme::Auto => "auto",
            AuthScheme::Bearer => "bearer",
            AuthScheme::Token => "token",
        };
        if y < inner.bottom() {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("{:<label_w$}  ", "auth_scheme"),
                        Style::default().fg(theme.text_dim),
                    ),
                    Span::styled(scheme, Style::default().fg(theme.accent)),
                    Span::styled("  (Ctrl+S cycles)", Style::default().fg(theme.text_dim)),
                ])),
                Rect::new(inner.x, y, inner.width, 1),
            );
            y = y.saturating_add(1);
        }
        if y < inner.bottom() {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("{:<label_w$}  ", "verify_tls"),
                        Style::default().fg(theme.text_dim),
                    ),
                    Span::styled(
                        if wizard.form.verify_tls { "on" } else { "off" },
                        Style::default().fg(theme.accent),
                    ),
                    Span::styled("  (Ctrl+L toggles)", Style::default().fg(theme.text_dim)),
                ])),
                Rect::new(inner.x, y, inner.width, 1),
            );
            y = y.saturating_add(1);
        }
    }

    // Advanced toggle hint, then a blank row.
    if y < inner.bottom() {
        let hint = if wizard.advanced {
            "▾ Ctrl+A  hide advanced"
        } else {
            "▸ Ctrl+A  advanced (token_env, auth_scheme, verify_tls)"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(theme.text_dim))),
            Rect::new(inner.x, y, inner.width, 1),
        );
        y = y.saturating_add(2);
    }

    // Test state.
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
    if y < inner.bottom() {
        frame.render_widget(
            Paragraph::new(Span::styled(test_text, test_style)),
            Rect::new(inner.x, y, inner.width, 1),
        );
        y = y.saturating_add(1);
    }

    // Optional message line.
    if y < inner.bottom()
        && let Some(msg) = &wizard.form.message
    {
        frame.render_widget(
            Paragraph::new(Span::styled(msg.clone(), Style::default().fg(theme.error))),
            Rect::new(inner.x, y, inner.width, 1),
        );
    }

    // Footer help, pinned to the last inner row.
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Tab: field  Ctrl+A: advanced  Ctrl+T: test  Enter: save",
            Style::default().fg(theme.text_dim),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );

    if let Some(pos) = cursor {
        frame.set_cursor_position(pos);
    }
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
    fn new_wizard_starts_blank_name_focused_on_url() {
        let w = OnboardingWizard::new();
        // The name starts blank — it's derived from the url host on save, not
        // pre-seeded to `default`.
        assert!(w.form.name().is_empty());
        assert_eq!(w.form.form_input().focus(), field::URL);
        assert!(!w.advanced, "advanced fields start collapsed");
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
        // Name derived from the url host (`nb.example` → `nb`), not `default`.
        assert_eq!(outcome.name, "nb");

        // The on-disk config has the profile and names it active.
        let text = std::fs::read_to_string(&path).unwrap();
        let cfg: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg.active_profile.as_deref(), Some("nb"));
        let prof = &cfg.profiles["nb"];
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

        assert!(outcome.stored_in_config);
        assert!(!outcome.needs_env_guidance);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("token = \"nbt_secret.value\""), "{text}");
        let cfg: Config = toml::from_str(&text).unwrap();
        assert_eq!(
            cfg.profiles["nb"]
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
        let prof = &cfg.profiles["nb"];
        assert_eq!(prof.timeout_secs, Some(30), "typed timeout persisted");
        assert_eq!(prof.page_size, Some(250), "typed page_size persisted");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persist_token_env_completes_and_persists_env() {
        // A token_env (no typed token) is persisted; the outcome doesn't ask for env
        // guidance (the token_env is the guidance) and nothing lands in config.
        let path = temp_config("tokenenv");
        let _ = std::fs::remove_file(&path);
        let mut w = OnboardingWizard::new();
        // Set url + token_env directly (token_env is an advanced field now); this
        // test exercises persist, not the wizard's field navigation.
        w.form
            .form_input_mut()
            .input_mut(field::URL)
            .unwrap()
            .set_value("https://nb.example");
        w.form
            .form_input_mut()
            .input_mut(field::TOKEN_ENV)
            .unwrap()
            .set_value("NETBOX_TOKEN");
        let outcome = persist(&w.form, &path).unwrap();
        assert!(!outcome.stored_in_config);
        assert_eq!(outcome.token_env.as_deref(), Some("NETBOX_TOKEN"));
        assert!(
            !outcome.needs_env_guidance,
            "a token_env counts as guidance already given"
        );

        let cfg: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            cfg.profiles["nb"].token_env.as_deref(),
            Some("NETBOX_TOKEN")
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persist_no_token_anywhere_requests_env_guidance() {
        // No typed token and no token_env: the outcome flags that the user must
        // export NBOX_TOKEN / set a token_env to connect.
        let path = temp_config("noenv");
        let _ = std::fs::remove_file(&path);
        let mut w = OnboardingWizard::new();
        type_into(&mut w, "https://nb.example");
        let outcome = persist(&w.form, &path).unwrap();
        assert!(!outcome.stored_in_config);
        assert!(outcome.token_env.is_none());
        assert!(outcome.needs_env_guidance);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn suggest_name_strips_a_generic_host_label() {
        assert_eq!(suggest_name_from_url("https://netbox.acme.com"), "acme");
        assert_eq!(suggest_name_from_url("https://www.acme.com"), "acme");
        assert_eq!(
            suggest_name_from_url("https://acme.example.com:8443/api"),
            "acme"
        );
        assert_eq!(suggest_name_from_url("https://corp.net"), "corp");
        // No scheme still resolves a host.
        assert_eq!(suggest_name_from_url("netbox.lab.internal"), "lab");
    }

    #[test]
    fn suggest_name_falls_back_to_prod_for_ip_or_empty() {
        assert_eq!(suggest_name_from_url("https://10.0.0.5"), "prod");
        assert_eq!(suggest_name_from_url("https://10.0.0.5:8000/"), "prod");
        assert_eq!(suggest_name_from_url("https://[::1]:8080"), "prod");
        assert_eq!(suggest_name_from_url(""), "prod");
        assert_eq!(suggest_name_from_url("https://"), "prod");
    }

    #[test]
    fn ctrl_a_toggles_the_advanced_field_set() {
        let mut w = OnboardingWizard::new();
        assert!(!w.advanced);
        assert_eq!(
            w.visible_fields().to_vec(),
            vec![field::URL, field::NAME, field::TOKEN]
        );
        w.handle_key(ctrl('a'));
        assert!(w.advanced);
        assert_eq!(
            w.visible_fields().to_vec(),
            vec![field::URL, field::NAME, field::TOKEN, field::TOKEN_ENV]
        );
        w.handle_key(ctrl('a'));
        assert!(!w.advanced);
    }

    #[test]
    fn tab_walks_visible_fields_and_skips_hidden_token_env() {
        let mut w = OnboardingWizard::new();
        // Simple mode: url → name → token → wrap to url; token_env is never focused.
        assert_eq!(w.form.form_input().focus(), field::URL);
        w.handle_key(key(KeyCode::Tab));
        assert_eq!(w.form.form_input().focus(), field::NAME);
        w.handle_key(key(KeyCode::Tab));
        assert_eq!(w.form.form_input().focus(), field::TOKEN);
        w.handle_key(key(KeyCode::Tab));
        assert_eq!(
            w.form.form_input().focus(),
            field::URL,
            "wraps past token, skipping the hidden token_env"
        );
        w.handle_key(key(KeyCode::BackTab));
        assert_eq!(w.form.form_input().focus(), field::TOKEN);
    }

    #[test]
    fn collapsing_advanced_pulls_focus_off_a_hidden_field() {
        let mut w = OnboardingWizard::new();
        w.handle_key(ctrl('a')); // expand
        for _ in 0..3 {
            w.handle_key(key(KeyCode::Tab)); // url → name → token → token_env
        }
        assert_eq!(w.form.form_input().focus(), field::TOKEN_ENV);
        w.handle_key(ctrl('a')); // collapse — token_env is now hidden
        assert_eq!(
            w.form.form_input().focus(),
            field::URL,
            "focus falls back to url when its field is hidden"
        );
    }

    #[test]
    fn blank_name_is_filled_from_the_url_on_save() {
        let mut w = OnboardingWizard::new();
        type_into(&mut w, "https://netbox.acme.com");
        assert_eq!(w.handle_key(key(KeyCode::Enter)), WizardAction::Save);
        assert_eq!(
            w.form.name(),
            "acme",
            "blank name derived from the url host"
        );
    }

    #[test]
    fn a_failed_save_reverts_the_auto_filled_name() {
        let mut w = OnboardingWizard::new();
        // No url ⇒ validation fails on the url; the name auto-filled for the
        // validation attempt is reverted so the live suggestion placeholder returns.
        assert_eq!(w.handle_key(key(KeyCode::Enter)), WizardAction::None);
        assert!(w.form.message.as_deref().unwrap().contains("url"));
        assert!(
            w.form.name().is_empty(),
            "auto-filled name reverted after a failed save"
        );
    }

    #[test]
    fn a_typed_name_is_not_overwritten_by_the_suggestion() {
        let mut w = OnboardingWizard::new();
        type_into(&mut w, "https://netbox.acme.com");
        w.handle_key(key(KeyCode::Tab)); // url → name
        assert_eq!(w.form.form_input().focus(), field::NAME);
        type_into(&mut w, "edge");
        assert_eq!(w.handle_key(key(KeyCode::Enter)), WizardAction::Save);
        assert_eq!(w.form.name(), "edge");
    }

    /// Render the wizard onto a [`TestBackend`] and return the flattened buffer
    /// text, so a test can assert what's on screen (and that drawing never panics).
    fn rendered_text(w: &mut OnboardingWizard) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::default();
        terminal.draw(|f| render(f, f.area(), w, &theme)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn render_simple_mode_draws_core_fields_without_advanced_controls() {
        let mut w = OnboardingWizard::new();
        let text = rendered_text(&mut w);
        assert!(text.contains("Welcome to nbox"));
        assert!(text.contains("url"));
        assert!(text.contains("name"));
        assert!(text.contains("Ctrl+A")); // the advanced toggle hint
        // The auth/tls controls are advanced-only — hidden on the first screen.
        assert!(!text.contains("Ctrl+S"));
        assert!(!text.contains("Ctrl+L"));
    }

    #[test]
    fn render_advanced_mode_reveals_the_auth_and_tls_controls() {
        let mut w = OnboardingWizard::new();
        w.handle_key(ctrl('a')); // expand the advanced fields
        let text = rendered_text(&mut w);
        assert!(text.contains("Ctrl+S"), "auth_scheme control shown");
        assert!(text.contains("Ctrl+L"), "verify_tls control shown");
        assert!(text.contains("hide advanced"));
    }
}
