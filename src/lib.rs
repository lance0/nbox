//! nbx — terminal UI and CLI for NetBox.
//!
//! Library crate root. See `DESIGN.md` and `ROADMAP.md` for the architecture
//! and phasing. The binary parses a [`cli::Cli`] and dispatches into [`run`].

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::CommandFactory;

use crate::cli::{Cli, Command};
use crate::domain::device_view::DeviceView;
use crate::netbox::client::NetBoxClient;

pub mod cli;
pub mod config;
pub mod domain;
pub mod netbox;
pub mod output;
pub mod tui;

#[cfg(feature = "updates")]
pub mod update;

/// The crate version, sourced from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Context derived from the global CLI flags, used to connect to NetBox.
struct Ctx {
    config_path: Option<PathBuf>,
    profile: Option<String>,
    json: bool,
}

/// Dispatch a parsed [`Cli`] invocation.
pub async fn run(cli: Cli) -> Result<()> {
    let ctx = Ctx {
        config_path: cli.config,
        profile: cli.profile,
        json: cli.json,
    };

    match cli.command {
        None | Some(Command::Tui) => not_implemented("interactive TUI"),
        Some(Command::Search { .. }) => not_implemented("search"),
        Some(Command::Device { value }) => run_device(&ctx, &value).await,
        Some(Command::Ip { .. }) => not_implemented("IP lookup"),
        Some(Command::Prefix { .. }) => not_implemented("prefix lookup"),
        Some(Command::Site { .. }) => not_implemented("site lookup"),
        Some(Command::Rack { .. }) => not_implemented("rack lookup"),
        Some(Command::Vlan { .. }) => not_implemented("VLAN lookup"),
        Some(Command::Interface { .. }) => not_implemented("interface lookup"),
        Some(Command::Open { .. }) => not_implemented("open in browser"),
        Some(Command::Config { command }) => {
            config::run_config(command, ctx.config_path.as_deref(), ctx.json)
        }
        Some(Command::Profile { command }) => {
            config::run_profile(command, ctx.config_path.as_deref(), ctx.json)
        }
        Some(Command::Completions { shell }) => {
            let mut cmd = Cli::command();
            let bin = cmd.get_name().to_string();
            clap_complete::generate(shell.to_clap(), &mut cmd, bin, &mut std::io::stdout());
            Ok(())
        }
    }
}

/// Build a NetBox client from the active (or requested) profile.
fn connect(ctx: &Ctx) -> Result<NetBoxClient> {
    let path = match &ctx.config_path {
        Some(p) => p.clone(),
        None => config::default_path()?,
    };
    let cfg = config::load(&path)?;

    let name = ctx
        .profile
        .clone()
        .or_else(|| cfg.active_profile.clone())
        .context("no profile selected; run `nbx profile use <name>` or pass --profile")?;
    let profile = cfg
        .profiles
        .get(&name)
        .with_context(|| format!("no profile named '{name}'"))?;

    let token = config::resolve_token(profile);
    NetBoxClient::new(profile, token)
}

/// `nbx device <value>` — look up and render a device.
async fn run_device(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let device = client
        .device_by_ref(value)
        .await?
        .with_context(|| format!("no device matched \"{value}\""))?;

    let view = DeviceView::from_model(device);
    if ctx.json {
        output::json::print(&view)?;
    } else {
        view.to_key_values().print();
    }
    Ok(())
}

/// Report an unimplemented command on stderr without dirtying stdout.
fn not_implemented(what: &str) -> Result<()> {
    eprintln!("nbx: {what} is not yet implemented");
    Ok(())
}
