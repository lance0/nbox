//! Configuration: profiles, UI preferences, and token resolution.
//!
//! Config lives at `~/.config/nbox/config.toml` (Linux/macOS) or
//! `%APPDATA%\nbox\config.toml` (Windows). We read with `toml` and mutate with
//! `toml_edit` so user comments and formatting survive `profile add`/`use`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::cli::{ConfigCommand, ProfileCommand};
use crate::netbox::auth::AuthScheme;

/// Starter config written by `nbox config init`.
const INIT_TEMPLATE: &str = r#"# nbox configuration
# Tokens are NOT stored here — point `token_env` at an environment variable,
# or export NBOX_TOKEN to override.

config_version = 1

active_profile = "default"

[ui]
theme = "default"
wide = false
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

    /// MCP server (`nbox serve`) settings. Absent ⇒ all defaults (stdio).
    #[serde(default)]
    pub serve: ServeConfig,

    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileConfig>,
}

/// `nbox serve` (MCP server) settings. The CLI flags (`--http`, `--http-token`)
/// take precedence over these; everything is optional and absent ⇒ stdio.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

    /// Per-caller request cap on the HTTP `/mcp` endpoint, in requests per
    /// minute. Absent / `0` ⇒ disabled (default). Overridden by `--rate-limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<u32>,
}

/// UI / TUI preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub wide: bool,
    #[serde(default = "default_true")]
    pub confirm_writes: bool,
    #[serde(default)]
    pub open_browser_command: String,
    /// TUI auto-refresh interval in seconds (0/absent = disabled).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_secs: Option<u64>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            wide: false,
            confirm_writes: true,
            open_browser_command: String::new(),
            refresh_secs: None,
        }
    }
}

/// A single NetBox connection profile.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub url: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_scheme: Option<AuthScheme>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_tls: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_config_context: Option<bool>,
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
    Ok(cfg)
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

/// Resolve the API token for `profile`, preferring `NBOX_TOKEN`.
pub fn resolve_token(profile: &ProfileConfig) -> Option<String> {
    let nbox = std::env::var("NBOX_TOKEN").ok();
    let from_env = profile
        .token_env
        .as_ref()
        .and_then(|name| std::env::var(name).ok());
    select_token(nbox, from_env)
}

/// Pure token-priority logic: `NBOX_TOKEN` wins, then the profile's env var.
fn select_token(nbox_token: Option<String>, env_token: Option<String>) -> Option<String> {
    nbox_token
        .filter(|t| !t.is_empty())
        .or_else(|| env_token.filter(|t| !t.is_empty()))
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

/// Persist the active UI theme to the config file (format-preserving).
pub fn save_ui_theme(path: &Path, theme: &str) -> Result<()> {
    let mut doc = load_doc_or_new(path)?;
    set_ui_theme(&mut doc, theme);
    write_doc(path, &doc)?;
    Ok(())
}

/// Load the editable document at `path`, or start a fresh one if absent.
fn load_doc_or_new(path: &Path) -> Result<DocumentMut> {
    if path.exists() {
        let text = fs::read_to_string(path)?;
        text.parse::<DocumentMut>()
            .with_context(|| format!("parsing {}", path.display()))
    } else {
        Ok(DocumentMut::new())
    }
}

/// Write a document to `path`, creating parent directories as needed.
fn write_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, doc.to_string())?;
    Ok(())
}

/// Handle the `nbox config` subcommands.
pub fn run_config(
    cmd: ConfigCommand,
    config_path: Option<&Path>,
    format: crate::output::Format,
    json_opts: &crate::output::json::JsonOptions,
) -> Result<()> {
    let path = resolve_path(config_path)?;
    match cmd {
        ConfigCommand::Init => {
            if path.exists() {
                eprintln!("config already exists at {}", path.display());
            } else {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&path, INIT_TEMPLATE)?;
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
            let cfg = load(&path)?;
            crate::output::emit(format, json_opts, &cfg, || {
                print!("{}", toml::to_string_pretty(&cfg).unwrap_or_default());
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
        assert_eq!(work.page_size, Some(250));
        assert_eq!(work.verify_tls, None);
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
    fn token_priority_prefers_nbox_token() {
        assert_eq!(
            select_token(Some("override".into()), Some("env".into())),
            Some("override".into())
        );
        assert_eq!(select_token(None, Some("env".into())), Some("env".into()));
        assert_eq!(
            select_token(Some(String::new()), Some("env".into())),
            Some("env".into())
        );
        assert_eq!(select_token(None, None), None);
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
}
