//! Configuration: profiles, UI preferences, and token resolution.
//!
//! Config lives at `~/.config/nbox/config.toml` (Linux/macOS) or
//! `%APPDATA%\nbox\config.toml` (Windows). We read with `toml` and mutate with
//! `toml_edit` so user comments and formatting survive `profile add`/`use`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::cli::{ConfigCommand, ProfileCommand, TokenCommand};
use crate::netbox::auth::AuthScheme;

/// Starter config written by `nbox config init`.
const INIT_TEMPLATE: &str = r#"# nbox configuration
# Tokens can be stored in this user-only config file (`token = "..."`) or kept
# outside it with `token_env` / NBOX_TOKEN. Env vars override config tokens.

config_version = 1

active_profile = "default"

[ui]
theme = "default"
confirm_writes = true
open_browser_command = ""

[profiles.default]
url = "https://netbox.example.com"
token_env = "NETBOX_TOKEN"
auth_scheme = "auto"        # auto | bearer | token
verify_tls = true
timeout_secs = 15
page_size = 100
exclude_config_context = true

# REST is the canonical backend. GraphQL is an opt-in per-surface accelerator;
# uncomment to route a read surface through it (falls back to REST if the
# instance's GraphQL schema doesn't support that surface). Search is always REST.
# [profiles.default.api]
# vrf = "graphql"           # rest | graphql
# route_target = "graphql"  # rest | graphql
"#;

/// The config schema version this build writes and understands. Bump when the
/// shape changes incompatibly; older binaries warn on a newer file.
pub const CONFIG_VERSION: u32 = 1;

/// Top-level configuration document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Schema version; absent means pre-versioning (treated as v1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_version: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,

    /// Path to a log file. When set, logs are written here (and still mirrored
    /// to stderr); when absent, logs go to stderr only. Overridden by the
    /// `--log-file` flag. stdout is never used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_file: Option<String>,

    /// Logging level / `tracing` filter (e.g. `info`, `debug`, `nbox=debug`).
    /// Overridden by `--log-level`, then `NBOX_LOG`, then `RUST_LOG`; the
    /// fallback is `warn`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_level: Option<String>,

    #[serde(default)]
    pub ui: UiConfig,

    /// Local read-cache settings. Absent ⇒ defaults (in-memory cache enabled).
    #[serde(default)]
    pub cache: CacheSettings,

    /// MCP server (`nbox serve`) settings. Absent ⇒ all defaults (stdio).
    #[serde(default)]
    pub serve: ServeConfig,

    /// An order-preserving map so the profiles keep their TOML document order
    /// (the TUI profile switcher cycles in config-file order, not alphabetical).
    #[serde(default)]
    pub profiles: IndexMap<String, ProfileConfig>,
}

/// `nbox serve` (MCP server) settings. The CLI flags (`--http`, `--http-token`)
/// take precedence over these; everything is optional and absent ⇒ stdio.
///
/// `Debug` is hand-written (not derived) so `http_token` — a secret — is never
/// printed: it renders as `<redacted>`/`None`, so a `{:?}`/log of a `Config` can't
/// leak it. Keep in sync with [`redact_secrets`].
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ServeConfig {
    /// Loopback address to serve HTTP on, e.g. `127.0.0.1:8080`. Absent ⇒ stdio.
    /// Overridden by `--http`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<String>,

    /// Static bearer token required on the HTTP `/mcp` endpoint. Overridden by
    /// `--http-token` / `NBOX_SERVE_TOKEN`. Prefer the env var over storing a
    /// secret in the config file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_token: Option<String>,

    /// OIDC issuer URL. Its presence switches the HTTP transport into OAuth 2.1
    /// resource-server mode: inbound IdP JWTs are validated on `/mcp` and
    /// Protected Resource Metadata is advertised. Overridden by `--oidc-issuer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc_issuer: Option<String>,

    /// Expected token audience — nbox's canonical resource URI (RFC 8707).
    /// Required when `oidc_issuer` is set. Overridden by `--audience`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,

    /// Optional JWKS URL override. Absent ⇒ discover it from the issuer's
    /// `/.well-known/openid-configuration` (then `oauth-authorization-server`).
    /// Overridden by `--oidc-jwks-url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jwks_url: Option<String>,

    /// Extra hostnames to accept in the DNS-rebinding allow-list, on top of the
    /// `--audience` host and loopback. Only applies in OIDC/routable mode (a
    /// loopback bind stays loopback-only). Merged with any `--allowed-host`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_hosts: Vec<String>,

    /// Per-caller request cap on the HTTP `/mcp` endpoint, in requests per
    /// minute. Absent / `0` ⇒ disabled (default). Overridden by `--rate-limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<u32>,
}

impl std::fmt::Debug for ServeConfig {
    /// Redacts `http_token` so the secret never lands in a `{:?}`/log line: a set
    /// token shows as `Some("<redacted>")`, an unset one as `None`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServeConfig")
            .field("http", &self.http)
            .field(
                "http_token",
                &self.http_token.as_ref().map(|_| "<redacted>"),
            )
            .field("oidc_issuer", &self.oidc_issuer)
            .field("audience", &self.audience)
            .field("jwks_url", &self.jwks_url)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("rate_limit", &self.rate_limit)
            .finish()
    }
}

/// UI / TUI preferences.
///
/// (The former `wide` knob was removed — nothing read it, so shipping it was a
/// no-op. An existing `wide = …` in a user's file is harmlessly ignored, since
/// unknown keys aren't rejected.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_true")]
    pub confirm_writes: bool,
    #[serde(default)]
    pub open_browser_command: String,
    /// TUI auto-refresh interval in seconds (0/absent = disabled).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_secs: Option<u64>,
    /// The kind slug the TUI Nav rail last browsed (e.g. `device`, `vrf`),
    /// restored on the next launch. Absent = none (launch on Recent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_browsed: Option<String>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            confirm_writes: true,
            open_browser_command: String::new(),
            refresh_secs: None,
            last_browsed: None,
        }
    }
}

/// Local read-cache settings (the `[cache]` section). The cache is a small,
/// in-memory store of assembled view models per profile, so a burst of identical
/// reads (TUI back-navigation, a chatty MCP agent) doesn't re-hit NetBox. Pure
/// data — the runtime engine lives in `crate::cache`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CacheSettings {
    /// Master switch. When off, every read goes straight to NetBox.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How long (seconds) a cached value is reused before a fresh fetch. A short
    /// de-dupe window, not a freshness window — clamped to 5–300s by the engine.
    #[serde(default = "default_cache_ttl")]
    pub ttl_secs: u64,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl_secs: default_cache_ttl(),
        }
    }
}

fn default_cache_ttl() -> u64 {
    30
}

/// A single NetBox connection profile.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub url: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<ConfigToken>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_scheme: Option<AuthScheme>,

    /// Per-surface backend preferences (`[profiles.<name>.api]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiConfig>,

    /// Capture for the removed `backend` key so [`load`] can reject it with a
    /// clear pointer to `[profiles.<name>.api]` instead of silently ignoring it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<toml::Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_tls: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_config_context: Option<bool>,
}

/// A NetBox API token stored in config.
///
/// This intentionally serializes as a normal TOML string, but `Debug` and
/// `config show` redact it. The config file is written with user-only
/// permissions on Unix.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConfigToken(String);

impl ConfigToken {
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn redacted() -> Self {
        Self("<redacted>".to_string())
    }
}

impl std::fmt::Debug for ConfigToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("\"<redacted>\"")
    }
}

/// Which NetBox read backend a profile should prefer.
///
/// REST remains the default and full-coverage backend. GraphQL is opt-in and may
/// fall back to REST for operations NetBox does not expose through GraphQL.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BackendPreference {
    #[default]
    Rest,
    Graphql,
}

impl BackendPreference {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rest => "rest",
            Self::Graphql => "graphql",
        }
    }
}

/// A read surface whose backend can be chosen independently. REST is canonical;
/// GraphQL is an opt-in per-surface accelerator (see [`ApiConfig`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiSurface {
    /// The multi-kind `search` fan-out.
    Search,
    /// The VRF routing-context view (its prefixes/addresses bundle).
    Vrf,
    /// The route-target relation graph (its importing/exporting VRFs).
    RouteTarget,
}

impl ApiSurface {
    /// The `[profiles.<name>.api]` key this surface is configured under.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Vrf => "vrf",
            Self::RouteTarget => "route_target",
        }
    }
}

/// Per-surface backend preferences (`[profiles.<name>.api]`). A missing table, or
/// a missing key within it, means REST for that surface. Unknown keys (e.g. the
/// not-yet-implemented `detail`) are rejected so typos surface as config errors.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ApiConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<BackendPreference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vrf: Option<BackendPreference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_target: Option<BackendPreference>,
}

impl ProfileConfig {
    /// The configured backend preference for `surface` (REST when unset).
    #[must_use]
    pub fn api_preference(&self, surface: ApiSurface) -> BackendPreference {
        self.api
            .as_ref()
            .and_then(|a| match surface {
                ApiSurface::Search => a.search,
                ApiSurface::Vrf => a.vrf,
                ApiSurface::RouteTarget => a.route_target,
            })
            .unwrap_or_default()
    }
}

fn default_theme() -> String {
    "default".to_string()
}

fn default_true() -> bool {
    true
}

/// The default config file path for this platform.
pub fn default_path() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("could not determine the user config directory")?;
    Ok(dir.join("nbox").join("config.toml"))
}

/// Resolve an explicit `--config` path, falling back to [`default_path`].
fn resolve_path(explicit: Option<&Path>) -> Result<PathBuf> {
    match explicit {
        Some(p) => Ok(p.to_path_buf()),
        None => default_path(),
    }
}

/// Load and deserialize the typed config at `path`.
///
/// Forward-compatible: a `config_version` newer than this build is warned about
/// (some settings may be ignored) but still loads — we never hard-fail on it.
pub fn load(path: &Path) -> Result<Config> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("no config at {} — run `nbox config init`", path.display()))?;
    let cfg: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if let Some(v) = cfg.config_version
        && v > CONFIG_VERSION
    {
        tracing::warn!(
            "config_version {v} is newer than this nbox understands ({CONFIG_VERSION}); \
             some settings may be ignored — consider upgrading nbox"
        );
    }
    // The coarse `backend = "rest"|"graphql"` profile key was removed in favor of
    // per-surface `[profiles.<name>.api]` preferences. Reject it loudly rather
    // than silently ignore it, pointing at the replacement.
    for (name, profile) in &cfg.profiles {
        if profile.backend.is_some() {
            anyhow::bail!(
                "profile `{name}`: the `backend` key was removed — set the backend per surface under \
                 `[profiles.{name}.api]` instead, e.g. `search = \"graphql\"` (and/or `vrf = \"graphql\"`)"
            );
        }
    }
    Ok(cfg)
}

/// Whether the TUI should run first-run onboarding instead of connecting.
///
/// PURE + unit-testable: it decides purely from the on-disk state and the
/// `--profile` flag, doing no network or keyring I/O. Onboarding is needed when:
/// - there is **no config file** at `path` (a brand-new install), OR
/// - the config parses but has **no profiles**, OR
/// - **no active profile is resolvable** — neither a `--profile` override nor a
///   configured `active_profile` that names an existing profile.
///
/// A `--profile X` that names an existing profile (or a configured, existing
/// `active_profile`) means a normal launch — onboarding is skipped. An
/// unparseable config is *not* treated as first-run (the user has a file to fix;
/// `load` surfaces the parse error), so a typo never silently triggers the wizard.
#[must_use]
pub fn needs_onboarding(path: &Path, explicit_profile: Option<&str>) -> bool {
    // No file ⇒ brand-new install ⇒ onboard.
    let Ok(text) = fs::read_to_string(path) else {
        return true;
    };
    // A file that doesn't parse is the user's to fix — `load` reports the error;
    // don't mask it behind the wizard.
    let Ok(cfg) = toml::from_str::<Config>(&text) else {
        return false;
    };
    needs_onboarding_for(&cfg, explicit_profile)
}

/// The pure core of [`needs_onboarding`], over an already-parsed [`Config`]: true
/// when there are no profiles, or when no resolvable active profile exists.
#[must_use]
pub fn needs_onboarding_for(cfg: &Config, explicit_profile: Option<&str>) -> bool {
    if cfg.profiles.is_empty() {
        return true;
    }
    // A `--profile` that names a real profile is a normal launch.
    if let Some(name) = explicit_profile {
        return !cfg.profiles.contains_key(name);
    }
    // Otherwise the configured active profile must exist.
    match &cfg.active_profile {
        Some(name) => !cfg.profiles.contains_key(name),
        None => true,
    }
}

/// The logging-relevant config fields (`log_file`, `log_level`), read
/// best-effort so logging can be set up before — and independently of —
/// the command's own config handling.
#[derive(Debug, Clone, Default)]
pub struct LoggingConfig {
    pub log_file: Option<String>,
    pub log_level: Option<String>,
}

/// Read just the logging fields from the config at the resolved path, never
/// failing: a missing or unparseable config yields the empty default, so we
/// fall back to flags/env (and ultimately stderr at `warn`). This runs before
/// the command, which loads + validates the config properly on its own.
#[must_use]
pub fn load_logging(explicit: Option<&Path>) -> LoggingConfig {
    let Ok(path) = resolve_path(explicit) else {
        return LoggingConfig::default();
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return LoggingConfig::default();
    };
    let Ok(cfg) = toml::from_str::<Config>(&text) else {
        return LoggingConfig::default();
    };
    LoggingConfig {
        log_file: cfg.log_file,
        log_level: cfg.log_level,
    }
}

/// Where a resolved API token came from, in precedence order. Reported by
/// `nbox config token status` — the token *value* is never exposed, only its
/// source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenSource {
    /// The profile's `token_env` variable (highest precedence).
    TokenEnv(String),
    /// The `NBOX_TOKEN` environment variable.
    NboxToken,
    /// The token value stored in the profile config file.
    Config,
    /// No token from any source.
    None,
}

/// Resolve the API token for `profile`.
///
/// Precedence (highest first): the profile's `token_env` (if set & present), then
/// `NBOX_TOKEN`, then the profile's config token, then `None`. Each candidate is
/// normalized (a pasted `Bearer `/`Token ` prefix or stray whitespace stripped)
/// *before* it competes, so a source that's set but normalizes to nothing (e.g.
/// `NBOX_TOKEN="Bearer "`) falls through instead of masking a valid lower one.
///
/// (`config_path`/`profile_name` are unused — token storage is `config.toml` or an
/// env var, never an OS keyring — but kept for call-site signature stability.)
pub fn resolve_token(
    profile: &ProfileConfig,
    _config_path: &Path,
    _profile_name: &str,
) -> Option<String> {
    let token_env = profile
        .token_env
        .as_ref()
        .and_then(|name| std::env::var(name).ok());
    let nbox = std::env::var("NBOX_TOKEN").ok();
    select_env_token(token_env, nbox).or_else(|| {
        profile
            .token
            .as_ref()
            .and_then(|t| normalize_token(t.expose()))
    })
}

/// Strip surrounding whitespace and an accidental `Bearer `/`Token ` scheme prefix
/// from a token value. NetBox's UI copies the full `Authorization` header value
/// (`Bearer nbt_…`), so pasting that verbatim — into the token field, a `token_env`
/// var, or `NBOX_TOKEN` — still works: nbox adds the scheme itself from
/// `auth_scheme`. Returns `None` when nothing usable remains.
pub(crate) fn normalize_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let mut value = trimmed;
    for scheme in ["Bearer", "Token"] {
        if let Some(rest) = trimmed
            .get(..scheme.len())
            .filter(|p| p.eq_ignore_ascii_case(scheme))
            .map(|_| &trimmed[scheme.len()..])
            && (rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            // A scheme *word* followed by whitespace (or nothing) — so a real token
            // like "Tokenxyz" is never mistaken for a "Token " prefix.
            value = rest.trim();
            break;
        }
    }
    (!value.is_empty()).then(|| value.to_string())
}

/// Report the *source* of the resolved token for `profile` without exposing the
/// value, for `nbox config token status`. Mirrors [`resolve_token`]'s precedence.
pub fn resolve_token_source(
    profile: &ProfileConfig,
    _config_path: &Path,
    _profile_name: &str,
) -> TokenSource {
    if let Some(name) = &profile.token_env
        && std::env::var(name)
            .ok()
            .and_then(|t| normalize_token(&t))
            .is_some()
    {
        return TokenSource::TokenEnv(name.clone());
    }
    if std::env::var("NBOX_TOKEN")
        .ok()
        .and_then(|t| normalize_token(&t))
        .is_some()
    {
        return TokenSource::NboxToken;
    }
    if profile
        .token
        .as_ref()
        .and_then(|t| normalize_token(t.expose()))
        .is_some()
    {
        return TokenSource::Config;
    }
    TokenSource::None
}

/// Pure env-token precedence: the profile's `token_env` value wins over
/// `NBOX_TOKEN`. Each candidate is normalized (a pasted `Bearer `/`Token ` prefix
/// or whitespace stripped); one that normalizes to nothing is skipped, so it can't
/// mask a lower-precedence source.
fn select_env_token(token_env: Option<String>, nbox_token: Option<String>) -> Option<String> {
    token_env
        .and_then(|t| normalize_token(&t))
        .or_else(|| nbox_token.and_then(|t| normalize_token(&t)))
}

/// Resolve the token a Config-modal / onboarding **test-connect** probe should
/// use, so a `Ctrl+T` test sees exactly what a real launch/reconnect would send.
/// Mirrors [`resolve_token`]'s per-candidate normalization but adds the form's
/// freshly-typed token at the top (it's what the user just entered and wants to
/// test). Order: typed → the `token_env` variable's value → `NBOX_TOKEN` → the
/// profile's config token. Each candidate is normalized (a pasted `Bearer `/`Token `
/// prefix or whitespace stripped) before it competes, so one that's set but blank
/// can't mask a valid lower-precedence source.
pub(crate) fn resolve_probe_token(
    typed: Option<&str>,
    token_env: Option<&str>,
    config_token: Option<&str>,
) -> Option<String> {
    typed
        .and_then(normalize_token)
        .or_else(|| {
            token_env
                .and_then(|name| std::env::var(name).ok())
                .and_then(|t| normalize_token(&t))
        })
        .or_else(|| {
            std::env::var("NBOX_TOKEN")
                .ok()
                .and_then(|t| normalize_token(&t))
        })
        .or_else(|| config_token.and_then(normalize_token))
}

/// Insert or update `profiles.<name>` in a format-preserving document.
pub fn upsert_profile(
    doc: &mut DocumentMut,
    name: &str,
    url: &str,
    token_env: Option<&str>,
) -> Result<()> {
    let profiles = doc.entry("profiles").or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    let profiles = profiles
        .as_table_mut()
        .context("`profiles` is not a table")?;

    let prof = profiles
        .entry(name)
        .or_insert_with(|| Item::Table(Table::new()));
    let prof = prof
        .as_table_mut()
        .with_context(|| format!("`profiles.{name}` is not a table"))?;

    prof["url"] = value(url);
    if let Some(env) = token_env {
        prof["token_env"] = value(env);
    }
    Ok(())
}

/// Set (or clear) `profiles.<name>.token_env` in a format-preserving document.
/// `None` (or an empty name) removes the key. The in-app editor uses this so
/// clearing the `token_env` field actually drops it from the file, rather than
/// leaving a stale variable name behind. (The CLI `profile add` keeps the
/// additive [`upsert_profile`] behavior — it never clears an existing key.)
pub fn set_profile_token_env(
    doc: &mut DocumentMut,
    name: &str,
    token_env: Option<&str>,
) -> Result<()> {
    let prof = profile_table_mut(doc, name)?;
    match token_env.filter(|s| !s.is_empty()) {
        Some(env) => prof["token_env"] = value(env),
        None => {
            prof.remove("token_env");
        }
    }
    Ok(())
}

/// Set (or clear) `profiles.<name>.token` in a format-preserving document.
/// `None` (or an empty value) removes the key. Display paths must redact this
/// field; `write_doc` makes the file user-only on Unix.
pub fn set_profile_token(doc: &mut DocumentMut, name: &str, token: Option<&str>) -> Result<()> {
    let prof = profile_table_mut(doc, name)?;
    match token.filter(|t| !t.is_empty()) {
        Some(token) => prof["token"] = value(token),
        None => {
            prof.remove("token");
        }
    }
    Ok(())
}

/// Set (or clear) `profiles.<name>.auth_scheme` in a format-preserving document.
/// `None` removes the key (falls back to the `auto` default). The profile table
/// is created if absent. Mirrors [`upsert_profile`]'s toml_edit pattern.
pub fn set_profile_auth_scheme(
    doc: &mut DocumentMut,
    name: &str,
    scheme: Option<AuthScheme>,
) -> Result<()> {
    let prof = profile_table_mut(doc, name)?;
    match scheme {
        // `Auto` is the implicit default — write it out only when explicitly
        // bearer/token, and clear the key for auto so the file stays minimal.
        Some(AuthScheme::Bearer) => prof["auth_scheme"] = value("bearer"),
        Some(AuthScheme::Token) => prof["auth_scheme"] = value("token"),
        Some(AuthScheme::Auto) | None => {
            prof.remove("auth_scheme");
        }
    }
    Ok(())
}

/// Set (or clear) `profiles.<name>.verify_tls` in a format-preserving document.
/// `None` removes the key (falls back to the `true` default).
pub fn set_profile_verify_tls(
    doc: &mut DocumentMut,
    name: &str,
    verify: Option<bool>,
) -> Result<()> {
    let prof = profile_table_mut(doc, name)?;
    match verify {
        Some(v) => prof["verify_tls"] = value(v),
        None => {
            prof.remove("verify_tls");
        }
    }
    Ok(())
}

/// Set (or clear) `profiles.<name>.timeout_secs` in a format-preserving document.
/// `None` removes the key (falls back to the built-in default). Mirrors
/// [`set_profile_verify_tls`]'s pattern; the in-app editor writes an empty field
/// as `None` so a default-timeout profile stays clean.
pub fn set_profile_timeout_secs(
    doc: &mut DocumentMut,
    name: &str,
    timeout_secs: Option<u64>,
) -> Result<()> {
    let prof = profile_table_mut(doc, name)?;
    match timeout_secs {
        // toml stores integers as i64; clamp the (tiny) interval into range.
        Some(s) => prof["timeout_secs"] = value(i64::try_from(s).unwrap_or(i64::MAX)),
        None => {
            prof.remove("timeout_secs");
        }
    }
    Ok(())
}

/// Set (or clear) `profiles.<name>.page_size` in a format-preserving document.
/// `None` removes the key (falls back to the built-in default).
pub fn set_profile_page_size(
    doc: &mut DocumentMut,
    name: &str,
    page_size: Option<usize>,
) -> Result<()> {
    let prof = profile_table_mut(doc, name)?;
    match page_size {
        Some(s) => prof["page_size"] = value(i64::try_from(s).unwrap_or(i64::MAX)),
        None => {
            prof.remove("page_size");
        }
    }
    Ok(())
}

/// Set (or clear) `profiles.<name>.exclude_config_context` in a format-preserving
/// document. `None` removes the key (falls back to the built-in default).
pub fn set_profile_exclude_config_context(
    doc: &mut DocumentMut,
    name: &str,
    exclude: Option<bool>,
) -> Result<()> {
    let prof = profile_table_mut(doc, name)?;
    match exclude {
        Some(v) => prof["exclude_config_context"] = value(v),
        None => {
            prof.remove("exclude_config_context");
        }
    }
    Ok(())
}

/// Set `profiles.<name>.api.<surface>` in a format-preserving document. REST is
/// the implicit default, so a `Rest` preference REMOVES the key (and the
/// `[profiles.<name>.api]` table if it becomes empty) to keep REST profiles
/// clean; only `Graphql` is written out. The `[api]` sub-table and its parent
/// profile are created on demand. Comments and other keys/surfaces survive.
pub fn set_profile_api_backend(
    doc: &mut DocumentMut,
    name: &str,
    surface: ApiSurface,
    pref: BackendPreference,
) -> Result<()> {
    let profile = profile_table_mut(doc, name)?;
    let key = surface.key();
    match pref {
        BackendPreference::Graphql => {
            let api = profile
                .entry("api")
                .or_insert_with(|| Item::Table(Table::new()))
                .as_table_mut()
                .with_context(|| format!("`profiles.{name}.api` is not a table"))?;
            api[key] = value(pref.as_str());
        }
        // REST is the default: drop the key, and the whole `[api]` table once it
        // holds nothing, so a REST-everywhere profile carries no `[api]` section.
        BackendPreference::Rest => {
            if let Some(api) = profile.get_mut("api").and_then(Item::as_table_mut) {
                api.remove(key);
                if api.is_empty() {
                    profile.remove("api");
                }
            }
        }
    }
    Ok(())
}

/// Remove `profiles.<name>` from a format-preserving document. Returns
/// `Ok(false)` when there was no such profile (idempotent), `Ok(true)` when one
/// was removed. Comments and other keys are preserved.
pub fn remove_profile(doc: &mut DocumentMut, name: &str) -> Result<bool> {
    let Some(profiles) = doc.get_mut("profiles") else {
        return Ok(false);
    };
    let profiles = profiles
        .as_table_mut()
        .context("`profiles` is not a table")?;
    Ok(profiles.remove(name).is_some())
}

/// Get a mutable handle to `profiles.<name>` as a table, creating the `profiles`
/// table and the profile entry as needed. Shared by the profile-field setters.
fn profile_table_mut<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut Table> {
    let profiles = doc.entry("profiles").or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    let profiles = profiles
        .as_table_mut()
        .context("`profiles` is not a table")?;
    let prof = profiles
        .entry(name)
        .or_insert_with(|| Item::Table(Table::new()));
    prof.as_table_mut()
        .with_context(|| format!("`profiles.{name}` is not a table"))
}

/// Set the active profile in a format-preserving document.
pub fn set_active_profile(doc: &mut DocumentMut, name: &str) {
    doc["active_profile"] = value(name);
}

/// Set `[ui].theme` in a format-preserving document.
pub fn set_ui_theme(doc: &mut DocumentMut, theme: &str) {
    let ui = doc.entry("ui").or_insert_with(|| Item::Table(Table::new()));
    if let Some(table) = ui.as_table_mut() {
        table["theme"] = value(theme);
    }
}

/// A `[ui]` field to set, with its new value. The Settings section persists each
/// changed field through [`set_ui_field`]; this enum carries both the kind and
/// the value so one setter handles the string and the numeric/optional cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiField {
    /// `[ui].theme` — a string.
    Theme(String),
    /// `[ui].refresh_secs` — an optional `u64`. `None` (or `0`) clears the key
    /// (auto-refresh off), so the file stays minimal rather than holding a `0`.
    RefreshSecs(Option<u64>),
    /// `[ui].open_browser_command` — a string. An empty string is written as `""`
    /// (the explicit "use the OS default" value), matching the init template.
    OpenBrowserCommand(String),
    /// `[ui].last_browsed` — the kind slug the TUI Nav rail last browsed (e.g.
    /// `device`, `vrf`), restored on the next launch. `None` removes the key.
    LastBrowsed(Option<String>),
}

/// Set a single `[ui]` field in a format-preserving document, creating the `[ui]`
/// table if absent. Mirrors [`set_ui_theme`]/[`upsert_profile`]'s toml_edit
/// pattern: comments and other keys/sections survive. `refresh_secs` of `None`/`0`
/// removes the key (auto-refresh off); the string fields are written verbatim.
pub fn set_ui_field(doc: &mut DocumentMut, field: &UiField) {
    let ui = doc.entry("ui").or_insert_with(|| Item::Table(Table::new()));
    let Some(table) = ui.as_table_mut() else {
        return;
    };
    match field {
        UiField::Theme(theme) => table["theme"] = value(theme),
        UiField::OpenBrowserCommand(command) => {
            table["open_browser_command"] = value(command);
        }
        UiField::RefreshSecs(secs) => match secs.filter(|s| *s > 0) {
            // toml stores integers as i64; the in-app value is a small interval.
            Some(s) => table["refresh_secs"] = value(i64::try_from(s).unwrap_or(i64::MAX)),
            None => {
                table.remove("refresh_secs");
            }
        },
        UiField::LastBrowsed(slug) => match slug {
            Some(s) => table["last_browsed"] = value(s),
            None => {
                table.remove("last_browsed");
            }
        },
    }
}

/// Persist a single `[ui]` field to the config file (format-preserving). The
/// Settings section calls this for each changed field on save, mirroring
/// [`save_ui_theme`].
pub fn save_ui_field(path: &Path, field: &UiField) -> Result<()> {
    let mut doc = load_doc_or_new(path)?;
    set_ui_field(&mut doc, field);
    write_doc(path, &doc)?;
    Ok(())
}

/// Persist several `[ui]` fields in ONE format-preserving write (M8): all the
/// given changes are applied to a single [`DocumentMut`] and written once, so a
/// failure can't leave the file with the first field updated and the rest stale.
/// Order within `fields` is the write order; comments/other keys survive.
pub fn save_ui_fields(path: &Path, fields: &[UiField]) -> Result<()> {
    let mut doc = load_doc_or_new(path)?;
    for field in fields {
        set_ui_field(&mut doc, field);
    }
    write_doc(path, &doc)?;
    Ok(())
}

/// Set or clear a single top-level (non-`[ui]`) string key in a format-preserving
/// document: a present, non-empty value writes `key = "value"`; `None`/empty
/// removes the key. Used for the global `log_level` / `log_file` settings, which
/// live at the document root rather than under `[ui]`.
pub fn set_top_string(doc: &mut DocumentMut, key: &str, val: Option<&str>) {
    match val.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => doc[key] = value(v),
        None => {
            doc.as_table_mut().remove(key);
        }
    }
}

/// A single setting the Settings section can persist — either a `[ui]` field or a
/// top-level key (`log_level` / `log_file`). One enum so the whole form saves in
/// ONE format-preserving write (see [`save_setting_fields`]): comments and every
/// unrelated key/section stay intact across the save — and across `cargo install`
/// upgrades, since the file is edited in place, never rewritten wholesale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingField {
    /// A `[ui]` field (theme / refresh_secs / open_browser_command).
    Ui(UiField),
    /// Top-level `log_level`; `None`/empty clears it.
    LogLevel(Option<String>),
    /// Top-level `log_file`; `None`/empty clears it.
    LogFile(Option<String>),
    /// `[cache].enabled` — the read-cache on/off switch.
    CacheEnabled(bool),
    /// `[cache].ttl_secs` — the read-cache de-dupe TTL in seconds.
    CacheTtl(u64),
}

/// Apply one [`SettingField`] to a format-preserving document.
pub fn set_setting_field(doc: &mut DocumentMut, field: &SettingField) {
    match field {
        SettingField::Ui(f) => set_ui_field(doc, f),
        SettingField::LogLevel(v) => set_top_string(doc, "log_level", v.as_deref()),
        SettingField::LogFile(v) => set_top_string(doc, "log_file", v.as_deref()),
        SettingField::CacheEnabled(on) => {
            if let Some(table) = cache_table(doc) {
                table["enabled"] = value(*on);
            }
        }
        SettingField::CacheTtl(secs) => {
            if let Some(table) = cache_table(doc) {
                table["ttl_secs"] = value(i64::try_from(*secs).unwrap_or(i64::MAX));
            }
        }
    }
}

/// The `[cache]` table in a format-preserving document, creating it if absent.
/// Mirrors `set_ui_field`'s table handling so comments and other keys survive.
fn cache_table(doc: &mut DocumentMut) -> Option<&mut Table> {
    doc.entry("cache")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
}

/// Persist several settings in ONE format-preserving write — like
/// [`save_ui_fields`] but spanning `[ui]` and the top-level log keys, so the
/// Settings form's whole save is atomic and comment/key-preserving.
pub fn save_setting_fields(path: &Path, fields: &[SettingField]) -> Result<()> {
    let mut doc = load_doc_or_new(path)?;
    for field in fields {
        set_setting_field(&mut doc, field);
    }
    write_doc(path, &doc)?;
    Ok(())
}

/// Build the argv for a custom browser-open command, or `None` to fall back to the
/// OS default. PURE + testable: when `command` is blank, returns `None` (the
/// caller uses `open::that`); otherwise splits the command on whitespace into
/// program + args and **appends the URL as a single final argument** — the URL is
/// never spliced into the string or shell-interpolated, so a URL with shell
/// metacharacters can't inject anything. The first token is the program.
#[must_use]
pub fn build_open_argv(command: &str, url: &str) -> Option<Vec<String>> {
    let mut parts = command.split_whitespace().map(str::to_string);
    let program = parts.next()?; // blank command ⇒ no program ⇒ None (OS default)
    let mut argv = vec![program];
    argv.extend(parts);
    argv.push(url.to_string());
    Some(argv)
}

/// Open `url` in the browser, honoring a custom `open_browser_command`.
///
/// When `command` is set, it's split into program + args (see [`build_open_argv`])
/// and run with the URL appended as a final, non-interpolated argument. When it's
/// blank, falls back to the OS default via `open::that`. Errors propagate so the
/// caller can surface them. Never logs or interpolates the URL into a shell.
///
/// A non-zero exit from the custom command is treated as an error (L1): otherwise
/// `nbox open` would exit `0` and the TUI would say "opened" for a command that
/// actually failed (e.g. `false`, or a misconfigured opener).
pub fn open_url(command: &str, url: &str) -> std::io::Result<()> {
    match build_open_argv(command, url) {
        Some(argv) => {
            let (program, rest) = argv.split_first().expect("argv has the program");
            let status = std::process::Command::new(program).args(rest).status()?;
            if status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other(match status.code() {
                    Some(code) => format!("open command `{program}` exited with status {code}"),
                    None => format!("open command `{program}` terminated by a signal"),
                }))
            }
        }
        None => open::that(url),
    }
}

/// Persist the active UI theme to the config file (format-preserving).
pub fn save_ui_theme(path: &Path, theme: &str) -> Result<()> {
    let mut doc = load_doc_or_new(path)?;
    set_ui_theme(&mut doc, theme);
    write_doc(path, &doc)?;
    Ok(())
}

/// Persist the active profile to the config file (format-preserving). The
/// in-app editor's explicit "use it"/select calls this so `active_profile`
/// survives a restart (unlike the session-only `P`/`Ctrl+P` quick cycle).
pub fn save_active_profile(path: &Path, name: &str) -> Result<()> {
    let mut doc = load_doc_or_new(path)?;
    set_active_profile(&mut doc, name);
    write_doc(path, &doc)?;
    Ok(())
}

/// Load the editable document at `path`, or start a fresh one if absent.
pub fn load_doc_or_new(path: &Path) -> Result<DocumentMut> {
    if path.exists() {
        let text = fs::read_to_string(path)?;
        text.parse::<DocumentMut>()
            .with_context(|| format!("parsing {}", path.display()))
    } else {
        Ok(DocumentMut::new())
    }
}

/// Restrict an existing config file to owner-only (`0600`) on Unix; a no-op
/// elsewhere. The config can hold a `token = "..."`, so it must never be group/
/// world-readable.
fn restrict_to_owner(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Write `contents` to a config file, keeping it owner-only (`0600` on Unix) for
/// the *entire* write — the file can hold a `token = "..."`, so it must never be
/// group/world-readable, even transiently. An existing file is restricted *before*
/// it's truncated and rewritten; a *new* file is created at `0600` before any bytes
/// land (so the secret never touches disk under the default umask); and the mode is
/// reasserted afterward.
fn write_config_file(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Lock down an existing file (e.g. one left 0644 by an older nbox or hand
    // creation) before truncate+rewrite, so the secret is never exposed mid-write.
    if path.exists() {
        restrict_to_owner(path)?;
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Creation mode for a *new* file: 0600 applied before the first write.
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(contents)?;
    // Reassert 0600 (open() leaves an existing file's mode untouched; belt-and-
    // suspenders against a permissive umask on the create path).
    restrict_to_owner(path)?;
    Ok(())
}

/// Write a document to `path`, creating parent directories as needed. The file is
/// kept owner-only throughout (see [`write_config_file`]).
pub fn write_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    write_config_file(path, doc.to_string().as_bytes())
}

/// Handle the `nbox config` subcommands.
pub fn run_config(
    cmd: ConfigCommand,
    config_path: Option<&Path>,
    profile: Option<&str>,
    format: crate::output::Format,
    json_opts: &crate::output::json::JsonOptions,
) -> Result<()> {
    let path = resolve_path(config_path)?;
    match cmd {
        ConfigCommand::Token { command } => {
            run_config_token(command, &path, profile, format, json_opts)
        }
        ConfigCommand::Init => {
            if path.exists() {
                eprintln!("config already exists at {}", path.display());
            } else {
                // The token now lives in this file by default, so it's created
                // owner-only up front — before a user uncomments/adds `token = …`.
                write_config_file(&path, INIT_TEMPLATE.as_bytes())?;
                eprintln!("created config at {}", path.display());
            }
            Ok(())
        }
        ConfigCommand::Path => {
            let report = serde_json::json!({ "path": path.display().to_string() });
            crate::output::emit(format, json_opts, &report, || {
                println!("{}", path.display());
            })
        }
        ConfigCommand::Show => {
            let mut cfg = load(&path)?;
            // Never expose secrets. `serve.http_token` is the one secret that can
            // live in the file; redact it (a placeholder, not the value) in BOTH
            // the human TOML and the `--json` output before emitting.
            redact_secrets(&mut cfg);
            crate::output::emit(format, json_opts, &cfg, || {
                print!("{}", toml::to_string_pretty(&cfg).unwrap_or_default());
            })
        }
    }
}

/// Replace any secret value in `cfg` with a redaction placeholder, in place, so
/// it can be safely printed. Today the only file-stored secret is
/// `serve.http_token`; a present token becomes `"<redacted>"` (an absent one stays
/// `None`, so `config show` still tells you whether one is configured without ever
/// revealing it). Keep this in sync with [`ServeConfig`]'s `Debug` redaction.
pub(crate) fn redact_secrets(cfg: &mut Config) {
    if cfg.serve.http_token.is_some() {
        cfg.serve.http_token = Some("<redacted>".to_string());
    }
    for profile in cfg.profiles.values_mut() {
        if profile.token.is_some() {
            profile.token = Some(ConfigToken::redacted());
        }
    }
}

/// Resolve the active (or `--profile`) profile name for a token command. The
/// profile need not exist in the config yet — `token status` reports env-var
/// sources too — so this only requires *a* name, falling back to the config's
/// `active_profile`, then `"default"`.
fn token_profile_name(path: &Path, profile: Option<&str>) -> String {
    if let Some(name) = profile {
        return name.to_string();
    }
    load(path)
        .ok()
        .and_then(|cfg| cfg.active_profile)
        .unwrap_or_else(|| "default".to_string())
}

/// Handle `nbox config token {set,clear,status}`. The token value is never
/// printed, echoed, or logged.
fn run_config_token(
    cmd: TokenCommand,
    path: &Path,
    profile: Option<&str>,
    format: crate::output::Format,
    json_opts: &crate::output::json::JsonOptions,
) -> Result<()> {
    let name = token_profile_name(path, profile);
    match cmd {
        TokenCommand::Status => {
            // Only the *source* is reported, never the token value. An unknown
            // profile is fine: token_env/NBOX_TOKEN are env-only.
            let prof = load(path)
                .ok()
                .and_then(|cfg| cfg.profiles.get(&name).cloned())
                .unwrap_or_default();
            let source = resolve_token_source(&prof, path, &name);
            let label = match &source {
                TokenSource::TokenEnv(var) => format!("token_env {var}"),
                TokenSource::NboxToken => "NBOX_TOKEN".to_string(),
                TokenSource::Config => "config".to_string(),
                TokenSource::None => "none".to_string(),
            };
            let report = serde_json::json!({
                "profile": name,
                "source": match &source {
                    TokenSource::TokenEnv(_) => "token_env",
                    TokenSource::NboxToken => "NBOX_TOKEN",
                    TokenSource::Config => "config",
                    TokenSource::None => "none",
                },
                "token_env": match &source {
                    TokenSource::TokenEnv(var) => Some(var.clone()),
                    _ => None,
                },
            });
            crate::output::emit(format, json_opts, &report, || {
                println!("{label}");
            })
        }
    }
}

/// Handle the `nbox profile` subcommands.
pub fn run_profile(
    cmd: ProfileCommand,
    config_path: Option<&Path>,
    format: crate::output::Format,
    json_opts: &crate::output::json::JsonOptions,
) -> Result<()> {
    let path = resolve_path(config_path)?;
    match cmd {
        ProfileCommand::Add {
            name,
            url,
            token_env,
        } => {
            let mut doc = load_doc_or_new(&path)?;
            upsert_profile(&mut doc, &name, &url, token_env.as_deref())?;
            if doc.get("active_profile").is_none() {
                set_active_profile(&mut doc, &name);
            }
            write_doc(&path, &doc)?;
            eprintln!("added profile '{name}' ({url})");
            Ok(())
        }
        ProfileCommand::Use { name } => {
            let mut doc = load_doc_or_new(&path)?;
            let exists = doc
                .get("profiles")
                .and_then(|p| p.as_table())
                .is_some_and(|t| t.contains_key(&name));
            if !exists {
                bail!("no profile named '{name}'");
            }
            set_active_profile(&mut doc, &name);
            write_doc(&path, &doc)?;
            eprintln!("active profile set to '{name}'");
            Ok(())
        }
        ProfileCommand::Remove { name } => {
            let mut doc = load_doc_or_new(&path)?;
            // Mirror the TUI's delete guards: don't strand the config by removing
            // the only profile, and don't remove the active one out from under the
            // user (they'd be left with a dangling `active_profile`).
            let (exists, only) = {
                let profiles = doc.get("profiles").and_then(|p| p.as_table());
                (
                    profiles.is_some_and(|t| t.contains_key(&name)),
                    profiles.is_some_and(toml_edit::Table::is_empty)
                        || profiles.is_some_and(|t| t.len() == 1),
                )
            };
            if !exists {
                bail!("no profile named '{name}'");
            }
            if only {
                bail!("can't remove the only profile '{name}'");
            }
            let active_is_target = doc
                .get("active_profile")
                .and_then(|v| v.as_str())
                .is_some_and(|a| a == name);
            if active_is_target {
                bail!(
                    "can't remove the active profile '{name}' — switch with `nbox profile use <other>` first"
                );
            }
            remove_profile(&mut doc, &name)?;
            write_doc(&path, &doc)?;
            eprintln!("removed profile '{name}'");
            Ok(())
        }
        ProfileCommand::List => {
            let cfg = load(&path)?;
            let names: Vec<&String> = cfg.profiles.keys().collect();
            crate::output::emit(format, json_opts, &names, || {
                for name in cfg.profiles.keys() {
                    let marker = if Some(name) == cfg.active_profile.as_ref() {
                        "*"
                    } else {
                        " "
                    };
                    println!("{marker} {name}");
                }
            })
        }
        ProfileCommand::Show { name } => {
            let cfg = load(&path)?;
            let name = name
                .or_else(|| cfg.active_profile.clone())
                .context("no profile specified and no active profile set")?;
            let profile = cfg
                .profiles
                .get(&name)
                .with_context(|| format!("no profile named '{name}'"))?;
            crate::output::emit(format, json_opts, profile, || {
                print!("{}", toml::to_string_pretty(profile).unwrap_or_default());
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
active_profile = "work"

[ui]
theme = "nord"

[profiles.work]
url = "https://netbox.example.com"
token_env = "NETBOX_TOKEN"
auth_scheme = "bearer"
page_size = 250

[profiles.work.api]
search = "graphql"
"#;

    #[test]
    fn deserializes_sample_config() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.active_profile.as_deref(), Some("work"));
        assert_eq!(cfg.ui.theme, "nord");
        // Defaulted UI field.
        assert!(cfg.ui.confirm_writes);
        let work = &cfg.profiles["work"];
        assert_eq!(work.url, "https://netbox.example.com");
        assert_eq!(work.auth_scheme, Some(AuthScheme::Bearer));
        assert_eq!(
            work.api_preference(ApiSurface::Search),
            BackendPreference::Graphql
        );
        // An unset surface defaults to REST.
        assert_eq!(
            work.api_preference(ApiSurface::Vrf),
            BackendPreference::Rest
        );
        assert_eq!(
            work.api_preference(ApiSurface::RouteTarget),
            BackendPreference::Rest
        );
        assert_eq!(work.page_size, Some(250));
        assert_eq!(work.verify_tls, None);
    }

    #[test]
    fn profiles_preserve_config_file_order_not_alphabetical() {
        // Declared out of alphabetical order on purpose: the `IndexMap` must keep
        // TOML document order so the TUI switcher (`P`/`Ctrl+P`) cycles in file
        // order. A `BTreeMap` would re-sort these to alpha and break that.
        let cfg: Config = toml::from_str(
            "[profiles.zebra]\nurl = \"https://z\"\n\
             [profiles.alpha]\nurl = \"https://a\"\n\
             [profiles.mike]\nurl = \"https://m\"\n",
        )
        .unwrap();
        let order: Vec<&str> = cfg.profiles.keys().map(String::as_str).collect();
        assert_eq!(order, ["zebra", "alpha", "mike"]);
    }

    #[test]
    fn api_preference_defaults_to_rest() {
        let profile: ProfileConfig = toml::from_str("url = \"https://nb.example\"").unwrap();
        assert_eq!(
            profile.api_preference(ApiSurface::Search),
            BackendPreference::Rest
        );
        assert_eq!(
            profile.api_preference(ApiSurface::Vrf),
            BackendPreference::Rest
        );
        assert_eq!(
            profile.api_preference(ApiSurface::RouteTarget),
            BackendPreference::Rest
        );
    }

    #[test]
    fn api_surface_preferences_parse() {
        let profile: ProfileConfig = toml::from_str(
            "url = \"https://nb.example\"\n[api]\nsearch = \"graphql\"\nvrf = \"rest\"\nroute_target = \"graphql\"\n",
        )
        .unwrap();
        assert_eq!(
            profile.api_preference(ApiSurface::Search),
            BackendPreference::Graphql
        );
        assert_eq!(
            profile.api_preference(ApiSurface::Vrf),
            BackendPreference::Rest
        );
        assert_eq!(
            profile.api_preference(ApiSurface::RouteTarget),
            BackendPreference::Graphql
        );
    }

    #[test]
    fn invalid_api_value_is_a_config_error() {
        let err = toml::from_str::<ProfileConfig>(
            "url = \"https://nb.example\"\n[api]\nsearch = \"grpc\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("grpc") || err.to_string().contains("search"));
    }

    #[test]
    fn unknown_api_surface_is_rejected() {
        // `detail` is intentionally not implemented; a typo'd/unsupported surface
        // must error rather than be silently ignored.
        let err = toml::from_str::<ProfileConfig>(
            "url = \"https://nb.example\"\n[api]\ndetail = \"graphql\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("detail"));
    }

    #[test]
    fn removed_backend_key_is_rejected_with_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[profiles.work]\nurl = \"https://nb.example\"\nbackend = \"graphql\"\n",
        )
        .unwrap();
        let err = load(&path).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("`backend`"), "got: {msg}");
        assert!(msg.contains("[profiles.work.api]"), "got: {msg}");
    }

    #[test]
    fn config_show_redacts_http_token_in_toml_and_json() {
        // H7: `config show` must never print the http_token value. Redact in both
        // the human TOML and the serialized (JSON) form.
        let mut cfg: Config = toml::from_str(
            "[serve]\nhttp = \"127.0.0.1:8080\"\nhttp_token = \"super-secret-value\"\n",
        )
        .unwrap();
        redact_secrets(&mut cfg);
        assert_eq!(cfg.serve.http_token.as_deref(), Some("<redacted>"));
        let toml_out = toml::to_string_pretty(&cfg).unwrap();
        assert!(
            !toml_out.contains("super-secret-value"),
            "TOML must not leak"
        );
        assert!(toml_out.contains("<redacted>"));
        let json_out = serde_json::to_string(&cfg).unwrap();
        assert!(
            !json_out.contains("super-secret-value"),
            "JSON must not leak"
        );
        // An absent token stays None (config show still says "no token configured").
        let mut none: Config = toml::from_str("[serve]\nhttp = \"127.0.0.1:8080\"\n").unwrap();
        redact_secrets(&mut none);
        assert_eq!(none.serve.http_token, None);
    }

    #[test]
    fn serve_config_debug_redacts_the_http_token() {
        // M2: a `{:?}` of a ServeConfig (or any Config) must not print the token.
        let cfg: Config = toml::from_str(
            "[serve]\nhttp = \"127.0.0.1:8080\"\nhttp_token = \"super-secret-value\"\n",
        )
        .unwrap();
        let dbg = format!("{cfg:?}");
        assert!(
            !dbg.contains("super-secret-value"),
            "Debug must not leak token"
        );
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn serve_section_is_optional_and_parses() {
        // Absent `[serve]` ⇒ defaults (no HTTP, no token, no OIDC) — stdio.
        let bare: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(bare.serve.http, None);
        assert_eq!(bare.serve.http_token, None);
        assert_eq!(bare.serve.oidc_issuer, None);
        assert_eq!(bare.serve.audience, None);
        assert_eq!(bare.serve.jwks_url, None);
        assert!(bare.serve.allowed_hosts.is_empty());
        assert_eq!(bare.serve.rate_limit, None);

        // A present `[serve]` populates the fields.
        let with: Config = toml::from_str(
            "active_profile = \"work\"\n\
             \n\
             [serve]\n\
             http = \"127.0.0.1:8080\"\n\
             http_token = \"local-secret\"\n\
             rate_limit = 120\n\
             \n\
             [profiles.work]\n\
             url = \"https://netbox.example.com\"\n",
        )
        .unwrap();
        assert_eq!(with.serve.http.as_deref(), Some("127.0.0.1:8080"));
        assert_eq!(with.serve.http_token.as_deref(), Some("local-secret"));
        assert_eq!(with.serve.rate_limit, Some(120));

        // The OIDC resource-server fields parse onto the same section.
        let oidc: Config = toml::from_str(
            "active_profile = \"work\"\n\
             \n\
             [serve]\n\
             http = \"0.0.0.0:8080\"\n\
             oidc_issuer = \"https://idp.example.com\"\n\
             audience = \"https://nbox.example.com\"\n\
             jwks_url = \"https://idp.example.com/keys\"\n\
             allowed_hosts = [\"nbox.example.com\", \"alt.example.com\"]\n\
             \n\
             [profiles.work]\n\
             url = \"https://netbox.example.com\"\n",
        )
        .unwrap();
        assert_eq!(
            oidc.serve.oidc_issuer.as_deref(),
            Some("https://idp.example.com")
        );
        assert_eq!(
            oidc.serve.audience.as_deref(),
            Some("https://nbox.example.com")
        );
        assert_eq!(
            oidc.serve.jwks_url.as_deref(),
            Some("https://idp.example.com/keys")
        );
        assert_eq!(
            oidc.serve.allowed_hosts,
            vec![
                "nbox.example.com".to_string(),
                "alt.example.com".to_string()
            ]
        );
    }

    #[test]
    fn config_version_is_optional_and_round_trips() {
        let with: Config = toml::from_str("config_version = 2\n").unwrap();
        assert_eq!(with.config_version, Some(2));
        // Pre-versioning configs (no field) parse as None, treated as v1.
        let without: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(without.config_version, None);
        // A future field nbox doesn't know is ignored, not an error.
        let future: Config = toml::from_str("config_version = 9\nfuture_knob = true\n").unwrap();
        assert_eq!(future.config_version, Some(9));
    }

    #[test]
    fn env_token_prefers_token_env_over_nbox_token() {
        // Reversed precedence (Phase A): the profile's `token_env` wins over
        // `NBOX_TOKEN`. Env always still beats the config token (layered on later).
        assert_eq!(
            select_env_token(Some("from-token-env".into()), Some("from-nbox".into())),
            Some("from-token-env".into())
        );
        // Falls back to NBOX_TOKEN when token_env is absent.
        assert_eq!(
            select_env_token(None, Some("from-nbox".into())),
            Some("from-nbox".into())
        );
        // An empty token_env value is skipped, falling through to NBOX_TOKEN.
        assert_eq!(
            select_env_token(Some(String::new()), Some("from-nbox".into())),
            Some("from-nbox".into())
        );
        // An empty NBOX_TOKEN with no token_env yields None (→ config tier).
        assert_eq!(select_env_token(None, Some(String::new())), None);
        // Neither env source set → None (→ config tier, then no token).
        assert_eq!(select_env_token(None, None), None);
    }

    #[test]
    fn resolve_token_falls_through_to_none() {
        // With no env vars set, a profile whose token_env names a guaranteed-unset
        // variable, and no config token, resolution drops past env to `None` —
        // exercising the full env→config→None chain without touching real env vars
        // the test runner might have. (Avoids mutating process env, which is racy
        // across parallel tests.)
        let profile = ProfileConfig {
            url: "https://nb.example".into(),
            token_env: Some("NBOX_TEST_DEFINITELY_UNSET_VAR_XYZ".into()),
            ..Default::default()
        };
        // Only assert the None outcome when NBOX_TOKEN isn't set in this
        // environment, so the test is hermetic.
        if std::env::var("NBOX_TOKEN").is_err() {
            let path = Path::new("/nbox/test/resolve-fallthrough/config.toml");
            assert_eq!(resolve_token(&profile, path, "default"), None);
            assert_eq!(
                resolve_token_source(&profile, path, "default"),
                TokenSource::None
            );
        }
    }

    #[test]
    fn config_token_is_resolved_after_env() {
        // A set-but-unset token_env and no NBOX_TOKEN fall through to the config
        // token, which is the lowest tier.
        let profile = ProfileConfig {
            url: "https://nb.example".into(),
            token_env: Some("NBOX_TEST_DEFINITELY_UNSET_VAR_XYZ".into()),
            token: Some(ConfigToken::new("config-secret")),
            ..Default::default()
        };
        if std::env::var("NBOX_TOKEN").is_err() {
            let path = Path::new("/nbox/test/config-token/config.toml");
            assert_eq!(
                resolve_token(&profile, path, "default").as_deref(),
                Some("config-secret")
            );
            assert_eq!(
                resolve_token_source(&profile, path, "default"),
                TokenSource::Config
            );
        }
    }

    #[test]
    fn config_token_debug_and_redaction_do_not_expose_secret() {
        let mut cfg = Config::default();
        cfg.profiles.insert(
            "work".to_string(),
            ProfileConfig {
                url: "https://nb.example".to_string(),
                token: Some(ConfigToken::new("nbt_supersecret.value")),
                ..Default::default()
            },
        );

        assert!(!format!("{cfg:?}").contains("nbt_supersecret"));
        redact_secrets(&mut cfg);
        let rendered = serde_json::to_string(&cfg).unwrap();
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("nbt_supersecret"));
    }

    #[test]
    fn profile_remove_drops_a_non_active_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let jopts = crate::output::json::JsonOptions::default();
        // The first add becomes active; the second is just a peer.
        run_profile(
            ProfileCommand::Add {
                name: "work".into(),
                url: "https://netbox.example.com".into(),
                token_env: None,
            },
            Some(&path),
            crate::output::Format::Plain,
            &jopts,
        )
        .unwrap();
        run_profile(
            ProfileCommand::Add {
                name: "lab".into(),
                url: "https://lab.example.com".into(),
                token_env: None,
            },
            Some(&path),
            crate::output::Format::Plain,
            &jopts,
        )
        .unwrap();
        run_profile(
            ProfileCommand::Remove { name: "lab".into() },
            Some(&path),
            crate::output::Format::Plain,
            &jopts,
        )
        .unwrap();
        let cfg = load(&path).unwrap();
        assert!(cfg.profiles.contains_key("work"));
        assert!(!cfg.profiles.contains_key("lab"));
        assert_eq!(cfg.active_profile.as_deref(), Some("work"));
    }

    #[test]
    fn profile_remove_refuses_missing_active_and_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let jopts = crate::output::json::JsonOptions::default();
        let add = |name: &str, url: &str| {
            run_profile(
                ProfileCommand::Add {
                    name: name.to_string(),
                    url: url.to_string(),
                    token_env: None,
                },
                Some(&path),
                crate::output::Format::Plain,
                &jopts,
            )
        };
        let remove = |name: &str| {
            run_profile(
                ProfileCommand::Remove {
                    name: name.to_string(),
                },
                Some(&path),
                crate::output::Format::Plain,
                &jopts,
            )
        };
        add("work", "https://netbox.example.com").unwrap();
        // Unknown profile name.
        assert!(remove("nope").is_err());
        // Removing the only profile would strand the config.
        assert!(remove("work").is_err());
        // With a peer present, the active profile is still protected.
        add("lab", "https://lab.example.com").unwrap();
        let err = remove("work").unwrap_err();
        assert!(err.to_string().contains("active profile"));
        // Nothing was removed by the refused calls.
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.profiles.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn config_init_creates_an_owner_only_file() {
        // 0.8.0 makes `token = "..."` the primary path and docs show hand-editing
        // the file, so `config init` must create it `0600` up front — before a user
        // adds a token to it.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        run_config(
            crate::cli::ConfigCommand::Init,
            Some(&path),
            None,
            crate::output::Format::Plain,
            &crate::output::json::JsonOptions::default(),
        )
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "config init must create an owner-only file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_doc_keeps_a_token_file_owner_only() {
        // Every profile/settings save goes through write_doc, and the document can
        // carry a `token = "..."`. A brand-new file must be created 0600, and an
        // existing world-readable file must be tightened to 0600, never left broad.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut doc = load_doc_or_new(&path).unwrap();
        set_profile_token(&mut doc, "work", Some("nbt_secret.value")).unwrap();

        // New file: created owner-only.
        write_doc(&path, &doc).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "a new token-bearing config must be created 0600"
        );

        // Pre-existing 0644 file: tightened back to 0600 on the next write.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_doc(&path, &doc).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "rewriting an over-permissive token file must restore 0600"
        );
    }

    #[test]
    fn resolve_probe_token_normalizes_each_tier() {
        // A test-connect must see the same normalized token a real connection would.
        // A typed token with a pasted `Bearer ` prefix is stripped and wins.
        assert_eq!(
            resolve_probe_token(Some("Bearer nbt_typed.tok"), None, None).as_deref(),
            Some("nbt_typed.tok")
        );
        // A typed token that normalizes to nothing falls through to the config token,
        // which is itself normalized.
        if std::env::var("NBOX_TOKEN").is_err() {
            assert_eq!(
                resolve_probe_token(Some("Bearer "), None, Some("Token nbt_cfg.tok")).as_deref(),
                Some("nbt_cfg.tok")
            );
        }
        // Nothing anywhere ⇒ None.
        if std::env::var("NBOX_TOKEN").is_err() {
            assert_eq!(resolve_probe_token(None, None, None), None);
        }
    }

    #[test]
    fn normalize_token_strips_scheme_prefix_and_whitespace() {
        // NetBox's copy button hands you "Bearer nbt_…"; pasting it verbatim must
        // still yield a bare key. Case-insensitive; stray whitespace is trimmed.
        assert_eq!(
            normalize_token("  nbt_abc.def  ").as_deref(),
            Some("nbt_abc.def")
        );
        assert_eq!(
            normalize_token("Bearer nbt_abc.def").as_deref(),
            Some("nbt_abc.def")
        );
        assert_eq!(
            normalize_token("bearer  nbt_abc.def").as_deref(),
            Some("nbt_abc.def")
        );
        assert_eq!(
            normalize_token("Token 0123abcd").as_deref(),
            Some("0123abcd")
        );
        assert_eq!(
            normalize_token("Bearer nbt_abc.def\n").as_deref(),
            Some("nbt_abc.def")
        );
        assert_eq!(normalize_token("   "), None);
        assert_eq!(normalize_token("Bearer   "), None);
    }

    #[test]
    fn resolve_token_strips_a_bearer_prefix_from_the_config_token() {
        // The exact footgun: a "Bearer nbt_…" pasted into the config token field
        // (NetBox's copy value) still resolves to the bare key on the wire.
        let profile = ProfileConfig {
            url: "https://nb.example".into(),
            token: Some(ConfigToken::new("Bearer nbt_abc.def")),
            ..Default::default()
        };
        if std::env::var("NBOX_TOKEN").is_err() {
            let path = Path::new("/nbox/test/bearer-strip/config.toml");
            assert_eq!(
                resolve_token(&profile, path, "default").as_deref(),
                Some("nbt_abc.def")
            );
        }
    }

    #[test]
    fn token_status_label_maps_each_source() {
        // The CLI's `status` label is a pure mapping over TokenSource; assert each
        // arm produces the documented, token-free label. (Mirrors the match in
        // `run_config_token`.)
        let label = |s: &TokenSource| match s {
            TokenSource::TokenEnv(var) => format!("token_env {var}"),
            TokenSource::NboxToken => "NBOX_TOKEN".to_string(),
            TokenSource::Config => "config".to_string(),
            TokenSource::None => "none".to_string(),
        };
        assert_eq!(
            label(&TokenSource::TokenEnv("NETBOX_TOKEN".into())),
            "token_env NETBOX_TOKEN"
        );
        assert_eq!(label(&TokenSource::NboxToken), "NBOX_TOKEN");
        assert_eq!(label(&TokenSource::Config), "config");
        assert_eq!(label(&TokenSource::None), "none");
        // No label ever contains a token value — only the source / env-var name.
        for s in [
            TokenSource::TokenEnv("X".into()),
            TokenSource::NboxToken,
            TokenSource::Config,
            TokenSource::None,
        ] {
            assert!(!label(&s).contains("secret"));
        }
    }

    #[test]
    fn nbox_token_normalizing_to_empty_falls_through_to_config() {
        // Reviewer edge: a high-precedence env source that's set but normalizes to
        // nothing (NBOX_TOKEN="Bearer ") must NOT mask a valid config token —
        // per-candidate normalization makes the empty source fall through.
        assert_eq!(
            select_env_token(None, Some("Bearer ".into())),
            None,
            "a scheme-prefix-only NBOX_TOKEN normalizes to nothing and is skipped"
        );
        let profile = ProfileConfig {
            url: "https://nb.example".into(),
            token: Some(ConfigToken::new("nbt_real.token")),
            ..Default::default()
        };
        // SAFETY: a uniquely-scoped var set+removed within this single-threaded test.
        let saved = std::env::var("NBOX_TOKEN").ok();
        unsafe { std::env::set_var("NBOX_TOKEN", "Bearer ") };
        let path = Path::new("/nbox/test/env-empty-fallthrough/config.toml");
        let resolved = resolve_token(&profile, path, "default");
        let source = resolve_token_source(&profile, path, "default");
        match saved {
            Some(v) => unsafe { std::env::set_var("NBOX_TOKEN", v) },
            None => unsafe { std::env::remove_var("NBOX_TOKEN") },
        }
        assert_eq!(resolved.as_deref(), Some("nbt_real.token"));
        assert_eq!(source, TokenSource::Config);
    }

    #[test]
    fn resolve_token_source_reports_token_env_when_present() {
        // Set a uniquely-named var so we don't collide with the ambient env or
        // other parallel tests; restore it after.
        let var = "NBOX_TEST_TOKENENV_SOURCE_VAR";
        // SAFETY: single-threaded within this test; the var name is unique to it.
        unsafe { std::env::set_var(var, "secret-value") };
        let profile = ProfileConfig {
            url: "https://nb.example".into(),
            token_env: Some(var.to_string()),
            ..Default::default()
        };
        let src = resolve_token_source(
            &profile,
            Path::new("/nbox/test/source/config.toml"),
            "default",
        );
        unsafe { std::env::remove_var(var) };
        assert_eq!(src, TokenSource::TokenEnv(var.to_string()));
    }

    #[test]
    fn needs_onboarding_truth_table() {
        // No config file at the path ⇒ first-run.
        let missing = Path::new("/nbox/test/onboarding/definitely-no-such-file.toml");
        assert!(needs_onboarding(missing, None));
        assert!(needs_onboarding(missing, Some("anything")));

        // Parsed config with no profiles ⇒ first-run (regardless of --profile).
        let empty: Config = toml::from_str("config_version = 1\n").unwrap();
        assert!(needs_onboarding_for(&empty, None));
        assert!(needs_onboarding_for(&empty, Some("work")));

        // Profiles exist but no active profile and no --profile ⇒ first-run.
        let no_active: Config =
            toml::from_str("[profiles.work]\nurl = \"https://nb.example\"\n").unwrap();
        assert!(no_active.active_profile.is_none());
        assert!(!no_active.profiles.is_empty());
        assert!(needs_onboarding_for(&no_active, None));

        // …but `--profile work` names an existing profile ⇒ normal launch.
        assert!(!needs_onboarding_for(&no_active, Some("work")));
        // `--profile bogus` names a missing profile ⇒ first-run.
        assert!(needs_onboarding_for(&no_active, Some("bogus")));

        // A valid active profile that exists ⇒ normal launch.
        let valid: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(valid.active_profile.as_deref(), Some("work"));
        assert!(valid.profiles.contains_key("work"));
        assert!(!needs_onboarding_for(&valid, None));
        // An explicit, existing --profile also short-circuits to normal launch.
        assert!(!needs_onboarding_for(&valid, Some("work")));

        // An active_profile naming a profile that doesn't exist ⇒ first-run.
        let dangling: Config = toml::from_str(
            "active_profile = \"gone\"\n\n[profiles.work]\nurl = \"https://nb.example\"\n",
        )
        .unwrap();
        assert!(needs_onboarding_for(&dangling, None));
    }

    #[test]
    fn needs_onboarding_ignores_an_unparseable_file() {
        // A file that exists but doesn't parse is the user's to fix — `load`
        // surfaces the parse error — so it must NOT silently trigger the wizard.
        let dir = std::env::temp_dir().join(format!("nbox-onboard-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad-config.toml");
        std::fs::write(&path, "this is = = not valid toml [[[").unwrap();
        assert!(!needs_onboarding(&path, None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_and_activate_round_trip() {
        let mut doc = DocumentMut::new();
        upsert_profile(&mut doc, "lab", "https://nb.lab", Some("NETBOX_LAB_TOKEN")).unwrap();
        set_active_profile(&mut doc, "lab");

        let cfg: Config = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(cfg.active_profile.as_deref(), Some("lab"));
        let lab = &cfg.profiles["lab"];
        assert_eq!(lab.url, "https://nb.lab");
        assert_eq!(lab.token_env.as_deref(), Some("NETBOX_LAB_TOKEN"));
    }

    #[test]
    fn set_ui_theme_round_trips_and_preserves_comments() {
        let original = "# notes\n[ui]\ntheme = \"default\"\nwide = false\n";
        let mut doc = original.parse::<DocumentMut>().unwrap();
        set_ui_theme(&mut doc, "nord");
        let out = doc.to_string();
        assert!(out.contains("# notes"), "comment should survive");
        assert!(out.contains("wide = false"), "other ui keys preserved");

        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.ui.theme, "nord");
    }

    #[test]
    fn set_ui_theme_creates_ui_table_when_absent() {
        let mut doc = DocumentMut::new();
        set_ui_theme(&mut doc, "matrix");
        let cfg: Config = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(cfg.ui.theme, "matrix");
    }

    #[test]
    fn upsert_preserves_existing_comments() {
        let original = "# my notes\nactive_profile = \"a\"\n\n[profiles.a]\nurl = \"https://a\"\n";
        let mut doc = original.parse::<DocumentMut>().unwrap();
        upsert_profile(&mut doc, "b", "https://b", None).unwrap();
        let out = doc.to_string();
        assert!(out.contains("# my notes"), "comment should survive edit");
        assert!(out.contains("[profiles.b]"));
    }

    #[test]
    fn remove_profile_preserves_comments_and_other_keys() {
        // Removing one profile must leave the file's comments, top-level keys, and
        // the *other* profile entirely intact (format-preserving round-trip).
        let original = "\
# keep me
active_profile = \"a\"

[ui]
theme = \"nord\"  # inline note

[profiles.a]
url = \"https://a\"
token_env = \"A_TOKEN\"

[profiles.b]
url = \"https://b\"
";
        let mut doc = original.parse::<DocumentMut>().unwrap();
        assert!(remove_profile(&mut doc, "a").unwrap(), "a was removed");
        let out = doc.to_string();
        // Comments and unrelated keys survive.
        assert!(out.contains("# keep me"), "top comment preserved");
        assert!(out.contains("theme = \"nord\""), "ui section preserved");
        assert!(out.contains("# inline note"), "inline comment preserved");
        assert!(
            out.contains("active_profile = \"a\""),
            "other keys preserved"
        );
        // The removed profile is gone; the sibling stays.
        let cfg: Config = toml::from_str(&out).unwrap();
        assert!(!cfg.profiles.contains_key("a"), "a removed");
        assert!(cfg.profiles.contains_key("b"), "b kept");
        assert_eq!(cfg.profiles["b"].url, "https://b");
        // Removing a non-existent profile is a no-op returning false.
        assert!(!remove_profile(&mut doc, "nope").unwrap());
        // Removing on a doc with no `profiles` table is also a clean false.
        let mut empty = DocumentMut::new();
        assert!(!remove_profile(&mut empty, "x").unwrap());
    }

    #[test]
    fn set_ui_field_round_trips_and_preserves_comments_and_other_keys() {
        // A realistic file: a top comment, an inline comment, other [ui] keys, and
        // a profile. Setting each ui field must touch only that key.
        let original = "\
# keep me
active_profile = \"a\"

[ui]
theme = \"default\"
wide = false  # kept untouched

[profiles.a]
url = \"https://a\"
";
        let mut doc = original.parse::<DocumentMut>().unwrap();
        set_ui_field(&mut doc, &UiField::Theme("nord".into()));
        set_ui_field(&mut doc, &UiField::RefreshSecs(Some(30)));
        set_ui_field(
            &mut doc,
            &UiField::OpenBrowserCommand("firefox --new-tab".into()),
        );
        set_ui_field(&mut doc, &UiField::LastBrowsed(Some("vrf".into())));
        let out = doc.to_string();
        // Comments and unrelated keys/sections survive. (An inline comment on a key
        // that *isn't* changed is preserved; overwriting a value replaces its own
        // line, same as `set_ui_theme`.)
        assert!(out.contains("# keep me"), "top comment preserved");
        assert!(
            out.contains("# kept untouched"),
            "inline comment on an unchanged key preserved"
        );
        assert!(out.contains("wide = false"), "other [ui] key preserved");
        assert!(out.contains("[profiles.a]"), "profile section preserved");
        assert!(out.contains("active_profile = \"a\""), "top key preserved");

        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.ui.theme, "nord");
        assert_eq!(cfg.ui.refresh_secs, Some(30));
        assert_eq!(cfg.ui.open_browser_command, "firefox --new-tab");
        assert_eq!(cfg.ui.last_browsed.as_deref(), Some("vrf"));

        // `LastBrowsed(None)` removes the key (e.g. a session that ends on Recent).
        set_ui_field(&mut doc, &UiField::LastBrowsed(None));
        assert!(
            !doc.to_string().contains("last_browsed"),
            "None clears the key"
        );
    }

    #[test]
    fn set_ui_field_refresh_secs_zero_or_none_clears_the_key() {
        // Start with a refresh interval set, then clear it two ways.
        let mut doc = "[ui]\nrefresh_secs = 15\n".parse::<DocumentMut>().unwrap();
        set_ui_field(&mut doc, &UiField::RefreshSecs(Some(0)));
        assert!(
            !doc.to_string().contains("refresh_secs"),
            "0 clears the key (auto-refresh off)"
        );
        let mut doc2 = "[ui]\nrefresh_secs = 15\n".parse::<DocumentMut>().unwrap();
        set_ui_field(&mut doc2, &UiField::RefreshSecs(None));
        let cfg: Config = toml::from_str(&doc2.to_string()).unwrap();
        assert_eq!(cfg.ui.refresh_secs, None, "None clears refresh_secs");
    }

    #[test]
    fn save_ui_fields_writes_all_changes_in_one_pass() {
        // M8: a batched save applies every field in one write (all-or-nothing).
        let dir = std::env::temp_dir().join(format!("nbox-uifields-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "# notes\n[ui]\ntheme = \"default\"\nwide = false\n").unwrap();
        save_ui_fields(
            &path,
            &[
                UiField::Theme("nord".into()),
                UiField::RefreshSecs(Some(20)),
                UiField::OpenBrowserCommand("firefox".into()),
            ],
        )
        .unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("# notes"), "comment preserved");
        assert!(out.contains("wide = false"), "other key preserved");
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.ui.theme, "nord");
        assert_eq!(cfg.ui.refresh_secs, Some(20));
        assert_eq!(cfg.ui.open_browser_command, "firefox");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_setting_fields_preserves_comments_unknown_keys_and_writes_log_fields() {
        // Upgrade-safety (the user's explicit requirement): a save must never blow
        // away the user's file. Comments, unknown/future keys, and unrelated
        // sections all survive, and the write is format-preserving (toml_edit), not
        // a wholesale rewrite.
        let dir = std::env::temp_dir().join(format!("nbox-setfields-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# my notes\n\
             active_profile = \"a\"\n\
             future_key = \"keep\"  # unknown to this build\n\
             \n\
             [ui]\n\
             theme = \"default\"\n\
             \n\
             [profiles.a]\n\
             url = \"https://a\"\n",
        )
        .unwrap();
        save_setting_fields(
            &path,
            &[
                SettingField::Ui(UiField::Theme("nord".into())),
                SettingField::LogLevel(Some("debug".into())),
                SettingField::LogFile(Some("/tmp/nbox.log".into())),
            ],
        )
        .unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("# my notes"), "top comment preserved");
        assert!(
            out.contains("# unknown to this build"),
            "inline comment preserved"
        );
        assert!(
            out.contains("future_key = \"keep\""),
            "unknown/future key preserved across the save"
        );
        assert!(out.contains("[profiles.a]"), "profile section preserved");
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.ui.theme, "nord");
        assert_eq!(cfg.log_level.as_deref(), Some("debug"));
        assert_eq!(cfg.log_file.as_deref(), Some("/tmp/nbox.log"));
        // Clearing a log field removes just that key; the other stays.
        save_setting_fields(&path, &[SettingField::LogLevel(None)]).unwrap();
        let out2 = std::fs::read_to_string(&path).unwrap();
        assert!(!out2.contains("log_level"), "None clears the key");
        assert!(out2.contains("log_file"), "the other log key is untouched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_ui_field_creates_ui_table_when_absent() {
        let mut doc = DocumentMut::new();
        set_ui_field(&mut doc, &UiField::OpenBrowserCommand("xdg-open".into()));
        let cfg: Config = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(cfg.ui.open_browser_command, "xdg-open");
    }

    #[test]
    fn build_open_argv_splits_command_and_appends_url() {
        // A set command splits into program + args, with the URL as the final arg.
        let argv = build_open_argv("firefox --new-tab", "https://nb/dcim/devices/1/").unwrap();
        assert_eq!(
            argv,
            vec![
                "firefox".to_string(),
                "--new-tab".to_string(),
                "https://nb/dcim/devices/1/".to_string(),
            ]
        );
        // A bare program still gets the URL appended.
        assert_eq!(
            build_open_argv("xdg-open", "https://x").unwrap(),
            vec!["xdg-open".to_string(), "https://x".to_string()]
        );
    }

    #[test]
    fn build_open_argv_blank_command_is_none_for_os_default() {
        // Empty / whitespace-only command ⇒ None ⇒ caller falls back to open::that.
        assert_eq!(build_open_argv("", "https://x"), None);
        assert_eq!(build_open_argv("   ", "https://x"), None);
    }

    #[cfg(unix)]
    #[test]
    fn open_url_reports_a_non_zero_exit_as_an_error() {
        // L1: a custom open command that exits non-zero (`false`) is an error, not
        // a false "opened". `true` (exit 0) succeeds.
        assert!(open_url("false", "https://x").is_err());
        assert!(open_url("true", "https://x").is_ok());
    }

    #[test]
    fn build_open_argv_does_not_interpolate_the_url() {
        // A URL with shell metacharacters is passed as a single, literal final
        // argument — never spliced into the command string — so it can't inject.
        let nasty = "https://nb/?q=a;rm -rf /&x=`whoami`";
        let argv = build_open_argv("open -a Safari", nasty).unwrap();
        assert_eq!(argv.first().unwrap(), "open");
        assert_eq!(argv.last().unwrap(), nasty, "URL is one literal arg");
        // The program/args never absorb any part of the URL.
        assert_eq!(argv.len(), 4); // open, -a, Safari, <url>
    }

    #[test]
    fn profile_field_setters_round_trip_and_clear() {
        let mut doc = DocumentMut::new();
        upsert_profile(&mut doc, "lab", "https://nb.lab", None).unwrap();
        set_profile_auth_scheme(&mut doc, "lab", Some(AuthScheme::Bearer)).unwrap();
        set_profile_verify_tls(&mut doc, "lab", Some(false)).unwrap();
        set_profile_token_env(&mut doc, "lab", Some("LAB_TOKEN")).unwrap();

        let cfg: Config = toml::from_str(&doc.to_string()).unwrap();
        let lab = &cfg.profiles["lab"];
        assert_eq!(lab.auth_scheme, Some(AuthScheme::Bearer));
        assert_eq!(lab.verify_tls, Some(false));
        assert_eq!(lab.token_env.as_deref(), Some("LAB_TOKEN"));

        // Clearing drops the keys (back to the implicit defaults), and `auto`
        // writes nothing (it's the default).
        set_profile_auth_scheme(&mut doc, "lab", Some(AuthScheme::Auto)).unwrap();
        set_profile_verify_tls(&mut doc, "lab", None).unwrap();
        set_profile_token_env(&mut doc, "lab", None).unwrap();
        let out = doc.to_string();
        assert!(!out.contains("auth_scheme"), "auto clears the key");
        assert!(!out.contains("verify_tls"), "None clears verify_tls");
        assert!(!out.contains("token_env"), "None clears token_env");
        let cfg2: Config = toml::from_str(&out).unwrap();
        let lab2 = &cfg2.profiles["lab"];
        assert_eq!(lab2.auth_scheme, None);
        assert_eq!(lab2.verify_tls, None);
        assert_eq!(lab2.token_env, None);
    }

    #[test]
    fn profile_numeric_and_bool_setters_round_trip_and_clear() {
        // A realistic file: a comment and a sibling key on the same profile, so the
        // setters must touch only their own key.
        let original = "\
# keep me
[profiles.lab]
url = \"https://nb.lab\"  # keep inline
token_env = \"LAB_TOKEN\"
";
        let mut doc = original.parse::<DocumentMut>().unwrap();
        set_profile_timeout_secs(&mut doc, "lab", Some(30)).unwrap();
        set_profile_page_size(&mut doc, "lab", Some(250)).unwrap();
        set_profile_exclude_config_context(&mut doc, "lab", Some(true)).unwrap();

        let out = doc.to_string();
        assert!(out.contains("# keep me"), "top comment preserved: {out}");
        assert!(out.contains("# keep inline"), "inline comment preserved");
        assert!(
            out.contains("token_env = \"LAB_TOKEN\""),
            "sibling key kept"
        );

        let cfg: Config = toml::from_str(&out).unwrap();
        let lab = &cfg.profiles["lab"];
        assert_eq!(lab.timeout_secs, Some(30));
        assert_eq!(lab.page_size, Some(250));
        assert_eq!(lab.exclude_config_context, Some(true));

        // Clearing each (None) drops the key back to the implicit default.
        set_profile_timeout_secs(&mut doc, "lab", None).unwrap();
        set_profile_page_size(&mut doc, "lab", None).unwrap();
        set_profile_exclude_config_context(&mut doc, "lab", None).unwrap();
        let out = doc.to_string();
        assert!(!out.contains("timeout_secs"), "None clears timeout_secs");
        assert!(!out.contains("page_size"), "None clears page_size");
        assert!(
            !out.contains("exclude_config_context"),
            "None clears exclude_config_context"
        );
        // The sibling key and comments are still there.
        assert!(out.contains("token_env = \"LAB_TOKEN\""));
        let cfg2: Config = toml::from_str(&out).unwrap();
        let lab2 = &cfg2.profiles["lab"];
        assert_eq!(lab2.timeout_secs, None);
        assert_eq!(lab2.page_size, None);
        assert_eq!(lab2.exclude_config_context, None);
    }

    #[test]
    fn set_profile_api_backend_writes_graphql_and_clears_to_rest() {
        let mut doc = DocumentMut::new();
        upsert_profile(&mut doc, "lab", "https://nb.lab", None).unwrap();
        // GraphQL on two surfaces writes the `[api]` table with both keys.
        set_profile_api_backend(&mut doc, "lab", ApiSurface::Vrf, BackendPreference::Graphql)
            .unwrap();
        set_profile_api_backend(
            &mut doc,
            "lab",
            ApiSurface::RouteTarget,
            BackendPreference::Graphql,
        )
        .unwrap();
        let out = doc.to_string();
        assert!(
            out.contains("[profiles.lab.api]"),
            "api table written: {out}"
        );
        assert!(out.contains("vrf = \"graphql\""));
        assert!(out.contains("route_target = \"graphql\""));
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(
            cfg.profiles["lab"].api_preference(ApiSurface::Vrf),
            BackendPreference::Graphql
        );
        assert_eq!(
            cfg.profiles["lab"].api_preference(ApiSurface::RouteTarget),
            BackendPreference::Graphql
        );

        // Clearing one surface back to REST removes just that key; the table stays
        // for the other GraphQL surface.
        set_profile_api_backend(&mut doc, "lab", ApiSurface::Vrf, BackendPreference::Rest).unwrap();
        let out = doc.to_string();
        assert!(!out.contains("vrf = "), "vrf key removed: {out}");
        assert!(
            out.contains("route_target = \"graphql\""),
            "other surface kept"
        );
        assert!(
            out.contains("[profiles.lab.api]"),
            "table kept while non-empty"
        );

        // Clearing the last GraphQL surface drops the whole `[api]` table.
        set_profile_api_backend(
            &mut doc,
            "lab",
            ApiSurface::RouteTarget,
            BackendPreference::Rest,
        )
        .unwrap();
        let out = doc.to_string();
        assert!(
            !out.contains("[profiles.lab.api]"),
            "empty api table removed: {out}"
        );
        let cfg2: Config = toml::from_str(&out).unwrap();
        assert_eq!(
            cfg2.profiles["lab"].api_preference(ApiSurface::Vrf),
            BackendPreference::Rest
        );
        assert_eq!(
            cfg2.profiles["lab"].api_preference(ApiSurface::RouteTarget),
            BackendPreference::Rest
        );
    }

    #[test]
    fn set_profile_api_backend_rest_on_a_profile_with_no_api_table_is_a_noop() {
        // Setting REST (the default) where there is no `[api]` table must not create
        // one — a REST-everywhere profile stays clean.
        let mut doc = DocumentMut::new();
        upsert_profile(&mut doc, "lab", "https://nb.lab", None).unwrap();
        set_profile_api_backend(&mut doc, "lab", ApiSurface::Vrf, BackendPreference::Rest).unwrap();
        let out = doc.to_string();
        assert!(
            !out.contains("[profiles.lab.api]"),
            "no api table created: {out}"
        );
    }

    #[test]
    fn select_env_token_precedence_and_empty_skip() {
        // token_env wins over NBOX_TOKEN; empty values are skipped at each tier; the
        // config token is tried only after this returns None (see resolve_token).
        assert_eq!(
            select_env_token(Some("env".into()), Some("nbox".into())),
            Some("env".into()),
            "token_env beats NBOX_TOKEN"
        );
        assert_eq!(
            select_env_token(Some(String::new()), Some("nbox".into())),
            Some("nbox".into()),
            "empty token_env falls through to NBOX_TOKEN"
        );
        assert_eq!(
            select_env_token(None, Some("nbox".into())),
            Some("nbox".into())
        );
        assert_eq!(
            select_env_token(Some(String::new()), Some(String::new())),
            None,
            "both empty ⇒ None (the config token is tried next)"
        );
        assert_eq!(select_env_token(None, None), None);
    }

    #[test]
    fn needs_onboarding_predicate_cases() {
        // No profiles at all ⇒ onboard.
        assert!(needs_onboarding_for(&Config::default(), None));

        let with_active: Config =
            toml::from_str("active_profile = \"work\"\n[profiles.work]\nurl = \"u\"\n").unwrap();
        // A resolvable active profile ⇒ normal launch.
        assert!(!needs_onboarding_for(&with_active, None));
        // `--profile` naming an existing profile ⇒ normal launch …
        assert!(!needs_onboarding_for(&with_active, Some("work")));
        // … naming a missing one ⇒ onboard.
        assert!(needs_onboarding_for(&with_active, Some("nope")));

        // Profiles exist but no active set ⇒ onboard.
        let no_active: Config = toml::from_str("[profiles.work]\nurl = \"u\"\n").unwrap();
        assert!(needs_onboarding_for(&no_active, None));

        // Active names a profile that no longer exists ⇒ onboard.
        let dangling: Config =
            toml::from_str("active_profile = \"gone\"\n[profiles.work]\nurl = \"u\"\n").unwrap();
        assert!(needs_onboarding_for(&dangling, None));
    }

    /// The format-preserving contract: edits keep user comments and every unrelated
    /// key/section intact, so an in-place edit (and a `cargo install` upgrade) never
    /// rewrites the file wholesale.
    #[test]
    fn edits_preserve_comments_and_unrelated_keys() {
        let original = r#"# nbox config — hand-written
config_version = 1
active_profile = "work"  # the prod box
log_level = "info"

[ui]
theme = "nord"  # favorite
refresh_secs = 30

[profiles.work]
url = "https://nb.example"  # keep me
token_env = "NB_TOKEN"
"#;
        let mut doc: DocumentMut = original.parse().unwrap();
        // A spread of edits across [ui], a profile field, and a top-level key.
        set_ui_field(&mut doc, &UiField::Theme("gruvbox".to_string()));
        set_profile_token_env(&mut doc, "work", Some("PROD_TOKEN")).unwrap();
        set_top_string(&mut doc, "log_level", Some("debug"));
        let out = doc.to_string();

        // Comments + unrelated keys survive verbatim.
        assert!(out.contains("# nbox config — hand-written"), "{out}");
        assert!(out.contains("# the prod box"), "{out}");
        assert!(out.contains("# keep me"), "{out}");
        assert!(
            out.contains("refresh_secs = 30"),
            "untouched key kept: {out}"
        );
        assert!(out.contains("config_version = 1"));
        // Targeted values changed.
        assert!(out.contains("theme = \"gruvbox\""), "{out}");
        assert!(out.contains("token_env = \"PROD_TOKEN\""), "{out}");
        assert!(out.contains("log_level = \"debug\""), "{out}");
        // And it still parses to the expected typed values.
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.ui.theme, "gruvbox");
        assert_eq!(
            cfg.profiles["work"].token_env.as_deref(),
            Some("PROD_TOKEN")
        );
        assert_eq!(cfg.log_level.as_deref(), Some("debug"));
    }

    #[test]
    fn save_setting_fields_is_atomic_and_preserves_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# keep\n[ui]\ntheme = \"nord\"\n\n[profiles.work]\nurl = \"u\"\n",
        )
        .unwrap();

        // One write spanning [ui], a top-level key, and [cache].
        save_setting_fields(
            &path,
            &[
                SettingField::Ui(UiField::RefreshSecs(Some(60))),
                SettingField::LogLevel(Some("nbox=debug".to_string())),
                SettingField::CacheTtl(45),
            ],
        )
        .unwrap();

        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("# keep"), "comment survives: {out}");
        assert!(out.contains("url = \"u\""), "profile survives: {out}");
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.ui.refresh_secs, Some(60));
        assert_eq!(cfg.log_level.as_deref(), Some("nbox=debug"));
        assert_eq!(cfg.cache.ttl_secs, 45);

        // A cleared optional field removes the key rather than writing 0.
        save_setting_fields(&path, &[SettingField::Ui(UiField::RefreshSecs(None))]).unwrap();
        let out2 = std::fs::read_to_string(&path).unwrap();
        assert!(
            !out2.contains("refresh_secs"),
            "None clears the key: {out2}"
        );
    }
}
