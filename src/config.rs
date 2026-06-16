//! Configuration: profiles, UI preferences, and token resolution.
//!
//! Config lives at `~/.config/nbx/config.toml` (Linux/macOS) or
//! `%APPDATA%\nbx\config.toml` (Windows). We read with `toml` and mutate with
//! `toml_edit` so user comments and formatting survive `profile add`/`use`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::cli::{ConfigCommand, ProfileCommand};
use crate::netbox::auth::AuthScheme;

/// Starter config written by `nbx config init`.
const INIT_TEMPLATE: &str = r#"# nbx configuration
# Tokens are NOT stored here — point `token_env` at an environment variable,
# or export NBX_TOKEN to override.

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

/// Top-level configuration document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,

    #[serde(default)]
    pub ui: UiConfig,

    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileConfig>,
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
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            wide: false,
            confirm_writes: true,
            open_browser_command: String::new(),
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
    Ok(dir.join("nbx").join("config.toml"))
}

/// Resolve an explicit `--config` path, falling back to [`default_path`].
fn resolve_path(explicit: Option<&Path>) -> Result<PathBuf> {
    match explicit {
        Some(p) => Ok(p.to_path_buf()),
        None => default_path(),
    }
}

/// Load and deserialize the typed config at `path`.
pub fn load(path: &Path) -> Result<Config> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("no config at {} — run `nbx config init`", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Resolve the API token for `profile`, preferring `NBX_TOKEN`.
pub fn resolve_token(profile: &ProfileConfig) -> Option<String> {
    let nbx = std::env::var("NBX_TOKEN").ok();
    let from_env = profile
        .token_env
        .as_ref()
        .and_then(|name| std::env::var(name).ok());
    select_token(nbx, from_env)
}

/// Pure token-priority logic: `NBX_TOKEN` wins, then the profile's env var.
fn select_token(nbx_token: Option<String>, env_token: Option<String>) -> Option<String> {
    nbx_token
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

/// Handle the `nbx config` subcommands.
pub fn run_config(cmd: ConfigCommand, config_path: Option<&Path>, json: bool) -> Result<()> {
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
            println!("{}", path.display());
            Ok(())
        }
        ConfigCommand::Show => {
            let cfg = load(&path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&cfg)?);
            } else {
                print!("{}", toml::to_string_pretty(&cfg)?);
            }
            Ok(())
        }
    }
}

/// Handle the `nbx profile` subcommands.
pub fn run_profile(cmd: ProfileCommand, config_path: Option<&Path>, json: bool) -> Result<()> {
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
                .map(|t| t.contains_key(&name))
                .unwrap_or(false);
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
            if json {
                let names: Vec<&String> = cfg.profiles.keys().collect();
                println!("{}", serde_json::to_string_pretty(&names)?);
            } else {
                for name in cfg.profiles.keys() {
                    let marker = if Some(name) == cfg.active_profile.as_ref() {
                        "*"
                    } else {
                        " "
                    };
                    println!("{marker} {name}");
                }
            }
            Ok(())
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
            if json {
                println!("{}", serde_json::to_string_pretty(profile)?);
            } else {
                print!("{}", toml::to_string_pretty(profile)?);
            }
            Ok(())
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
    fn token_priority_prefers_nbx_token() {
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
    fn upsert_preserves_existing_comments() {
        let original = "# my notes\nactive_profile = \"a\"\n\n[profiles.a]\nurl = \"https://a\"\n";
        let mut doc = original.parse::<DocumentMut>().unwrap();
        upsert_profile(&mut doc, "b", "https://b", None).unwrap();
        let out = doc.to_string();
        assert!(out.contains("# my notes"), "comment should survive edit");
        assert!(out.contains("[profiles.b]"));
    }
}
