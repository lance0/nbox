//! nbx — terminal UI and CLI for NetBox.
//!
//! Library crate root. See `DESIGN.md` and `ROADMAP.md` for the architecture
//! and phasing. The binary parses a [`cli::Cli`] and dispatches into [`run`].

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::CommandFactory;

use crate::cli::{Cli, Command};
use crate::domain::device_view::DeviceView;
use crate::domain::ip_view::{IpView, most_specific};
use crate::domain::prefix_view::PrefixView;
use crate::domain::rack_view::RackView;
use crate::domain::site_view::SiteView;
use crate::domain::vlan_view::VlanView;
use crate::netbox::client::NetBoxClient;
use crate::netbox::search::SearchRequest;

pub mod cli;
pub mod config;
pub mod domain;
pub mod netbox;
pub mod output;
pub mod tui;
pub mod util;

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
        None | Some(Command::Tui) => run_tui(&ctx).await,
        Some(Command::Search { query, limit }) => run_search(&ctx, &query, limit).await,
        Some(Command::Device { value }) => run_device(&ctx, &value).await,
        Some(Command::Ip { address }) => run_ip(&ctx, &address).await,
        Some(Command::Prefix { cidr }) => run_prefix(&ctx, &cidr).await,
        Some(Command::Site { value }) => run_site(&ctx, &value).await,
        Some(Command::Rack { value }) => run_rack(&ctx, &value).await,
        Some(Command::Vlan { value }) => run_vlan(&ctx, &value).await,
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

/// `nbx` / `nbx tui` — launch the interactive TUI.
async fn run_tui(ctx: &Ctx) -> Result<()> {
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

    let base_url = profile.url.clone();
    let theme_name = cfg.ui.theme.clone();
    let token = config::resolve_token(profile);
    let client = NetBoxClient::new(profile, token)?;

    let app = tui::state::App::new(client, &theme_name, name, base_url);
    tui::app::run(app).await
}

/// `nbx search <query>` — normalized multi-endpoint search.
async fn run_search(ctx: &Ctx, query: &str, limit: usize) -> Result<()> {
    let client = connect(ctx)?;
    let results = client
        .search(SearchRequest {
            query: query.to_string(),
            limit,
        })
        .await?;

    if ctx.json {
        output::json::print(&results)?;
    } else if results.is_empty() {
        eprintln!("no results for \"{query}\"");
    } else {
        for r in &results {
            match &r.subtitle {
                Some(s) => println!("{:<7} {}  ({s})", r.kind.as_str(), r.display),
                None => println!("{:<7} {}", r.kind.as_str(), r.display),
            }
        }
    }
    Ok(())
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

/// `nbx ip <address>` — resolve an IP and its most-specific parent prefix.
async fn run_ip(ctx: &Ctx, address: &str) -> Result<()> {
    let client = connect(ctx)?;
    let ip = client
        .ip_candidates(address)
        .await?
        .into_iter()
        .next()
        .with_context(|| format!("no IP address matched \"{address}\""))?;

    let host = address.split('/').next().unwrap_or(address);
    let parent = most_specific(client.prefixes_containing(host).await?);

    let view = IpView::build(ip, parent);
    if ctx.json {
        output::json::print(&view)?;
    } else {
        view.to_key_values().print();
    }
    Ok(())
}

/// `nbx prefix <cidr>` — show a prefix with its children and contained IPs.
async fn run_prefix(ctx: &Ctx, cidr: &str) -> Result<()> {
    const SECTION_CAP: usize = 50;
    let client = connect(ctx)?;
    let prefix = client
        .prefix_by_cidr(cidr)
        .await?
        .with_context(|| format!("no prefix matched \"{cidr}\""))?;

    let children = client.prefix_children(cidr, SECTION_CAP).await?;
    let ips = client.prefix_ips(cidr, SECTION_CAP).await?;

    let view = PrefixView::build(prefix, children, ips);
    if ctx.json {
        output::json::print(&view)?;
    } else {
        println!("{}", view.to_plain());
    }
    Ok(())
}

/// `nbx vlan <vid|name>` — show a VLAN and the prefixes that reference it.
async fn run_vlan(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let vlan = client
        .vlan_by_ref(value)
        .await?
        .with_context(|| format!("no VLAN matched \"{value}\""))?;
    let prefixes = client.vlan_prefixes(vlan.id, 50).await?;

    let view = VlanView::build(vlan, prefixes);
    if ctx.json {
        output::json::print(&view)?;
    } else {
        println!("{}", view.to_plain());
    }
    Ok(())
}

/// `nbx site <name|slug>` — show a site.
async fn run_site(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let site = client
        .site_by_ref(value)
        .await?
        .with_context(|| format!("no site matched \"{value}\""))?;

    let view = SiteView::from_model(site);
    if ctx.json {
        output::json::print(&view)?;
    } else {
        view.to_key_values().print();
    }
    Ok(())
}

/// `nbx rack <name|id>` — show a rack.
async fn run_rack(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let rack = client
        .rack_by_ref(value)
        .await?
        .with_context(|| format!("no rack matched \"{value}\""))?;

    let view = RackView::from_model(rack);
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
