//! nbox — terminal UI and CLI for NetBox.
//!
//! Library crate root. See `DESIGN.md` and `ROADMAP.md` for the architecture
//! and phasing. The binary parses a [`cli::Cli`] and dispatches into [`run`].

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::CommandFactory;

use crate::cli::{Cli, Command};
use crate::domain::device_detail::DeviceDetail;
use crate::domain::interface_view::InterfaceView;
use crate::domain::ip_view::{IpView, most_specific};
use crate::domain::prefix_view::PrefixView;
use crate::domain::rack_view::RackView;
use crate::domain::site_view::SiteView;
use crate::domain::vlan_view::VlanView;
use crate::netbox::client::NetBoxClient;
use crate::netbox::search::{SearchFilters, SearchRequest};
use crate::output::Format;
use crate::output::plain::KeyValues;

pub mod cli;
pub mod config;
pub mod domain;
pub mod error;
pub mod netbox;
pub mod output;
pub mod tui;
pub mod util;

#[cfg(feature = "updates")]
pub mod update;

/// The crate version, sourced from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Initialize logging to stderr (stdout stays clean for piping).
///
/// Level precedence: the `--log-level` flag, then `NBOX_LOG`, then `RUST_LOG`,
/// else quiet (`warn`). No-ops if a subscriber is already installed.
pub fn init_logging(log_level: Option<&str>) {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = match log_level {
        Some(level) => EnvFilter::new(level),
        None => EnvFilter::try_from_env("NBOX_LOG")
            .or_else(|_| EnvFilter::try_from_default_env())
            .unwrap_or_else(|_| EnvFilter::new("warn")),
    };

    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Context derived from the global CLI flags, used to connect to NetBox.
struct Ctx {
    config_path: Option<PathBuf>,
    profile: Option<String>,
    format: Format,
    json_opts: output::json::JsonOptions,
}

impl Ctx {
    fn is_json(&self) -> bool {
        self.format == Format::Json
    }
}

/// Render a serializable view per the selected format, or run `plain` for text.
fn emit<T: serde::Serialize>(ctx: &Ctx, view: &T, plain: impl FnOnce()) -> Result<()> {
    match ctx.format {
        Format::Json => output::json::print_with(view, &ctx.json_opts)?,
        Format::Csv => output::csv::print(view)?,
        Format::Plain => plain(),
    }
    Ok(())
}

/// Dispatch a parsed [`Cli`] invocation.
pub async fn run(cli: Cli) -> Result<()> {
    let json_opts = output::json::JsonOptions {
        fields: cli.fields.as_deref().map(|f| {
            f.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        }),
        raw: cli.raw,
        envelope: cli.envelope,
    };
    let ctx = Ctx {
        config_path: cli.config,
        profile: cli.profile,
        format: Format::resolve(cli.json, cli.output),
        json_opts,
    };

    match cli.command {
        None | Some(Command::Tui) => run_tui(&ctx).await,
        Some(Command::Search {
            query,
            limit,
            status,
            site,
            tenant,
            role,
            cols,
        }) => {
            let filters = SearchFilters {
                status,
                site,
                tenant,
                role,
            };
            run_search(&ctx, &query, limit, filters, cols).await
        }
        Some(Command::Device { value }) => run_device(&ctx, &value).await,
        Some(Command::Ip { address }) => run_ip(&ctx, &address).await,
        Some(Command::Prefix { cidr }) => run_prefix(&ctx, &cidr).await,
        Some(Command::Site { value }) => run_site(&ctx, &value).await,
        Some(Command::Rack { value }) => run_rack(&ctx, &value).await,
        Some(Command::Vlan { value }) => run_vlan(&ctx, &value).await,
        Some(Command::Interface { device, interface }) => {
            run_interface(&ctx, &device, &interface).await
        }
        Some(Command::Open { object_ref }) => run_open(&ctx, &object_ref).await,
        Some(Command::Status) => run_status(&ctx).await,
        Some(Command::Config { command }) => {
            config::run_config(command, ctx.config_path.as_deref(), ctx.is_json())
        }
        Some(Command::Profile { command }) => {
            config::run_profile(command, ctx.config_path.as_deref(), ctx.is_json())
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
        .context("no profile selected; run `nbox profile use <name>` or pass --profile")?;
    let profile = cfg
        .profiles
        .get(&name)
        .with_context(|| format!("no profile named '{name}'"))?;

    let token = config::resolve_token(profile);
    NetBoxClient::new(profile, token)
}

/// `nbox` / `nbox tui` — launch the interactive TUI.
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
        .context("no profile selected; run `nbox profile use <name>` or pass --profile")?;
    let profile = cfg
        .profiles
        .get(&name)
        .with_context(|| format!("no profile named '{name}'"))?;

    let base_url = profile.url.clone();
    let theme_name = cfg.ui.theme.clone();
    let refresh_secs = cfg.ui.refresh_secs;
    let token = config::resolve_token(profile);
    let client = NetBoxClient::new(profile, token)?;

    // Probe the instance on connect: confirms reachability + the 4.2 floor, and
    // gives us the version for the status line. (CLI commands skip this to stay
    // fast.)
    let status = client.verify_compatible().await?;

    let app = tui::state::App::new(
        client,
        &theme_name,
        name,
        base_url,
        status.netbox_version,
        Some(path),
    );
    tui::app::run(app, refresh_secs).await
}

/// `nbox status` — show NetBox connection + version info.
async fn run_status(ctx: &Ctx) -> Result<()> {
    let client = connect(ctx)?;
    let status = client.status().await?;
    let url = client.base_url().as_str().to_string();

    let report = serde_json::json!({
        "netbox_url": url,
        "netbox_version": status.netbox_version,
        "django_version": status.django_version,
        "python_version": status.python_version,
    });

    emit(ctx, &report, || {
        let mut kv = KeyValues::new();
        kv.push("netbox_url", url.clone())
            .push("netbox_version", status.netbox_version.clone())
            .push_opt("django", status.django_version.clone())
            .push_opt("python", status.python_version.clone());
        kv.print();
    })
}

/// `nbox search <query>` — normalized multi-endpoint search.
async fn run_search(
    ctx: &Ctx,
    query: &str,
    limit: usize,
    filters: SearchFilters,
    cols: Option<String>,
) -> Result<()> {
    let client = connect(ctx)?;
    let results = client
        .search(SearchRequest {
            query: query.to_string(),
            limit,
            filters,
        })
        .await?;

    match ctx.format {
        Format::Json => output::json::print_with(&results, &ctx.json_opts)?,
        Format::Csv => {
            let columns: Option<Vec<String>> = cols.as_deref().map(|c| {
                c.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            });
            let value = serde_json::to_value(&results)?;
            print!("{}", output::csv::to_csv(&value, columns.as_deref()));
        }
        Format::Plain => {
            if results.is_empty() {
                eprintln!("no results for \"{query}\"");
            } else {
                for r in &results {
                    match &r.subtitle {
                        Some(s) => println!("{:<7} {}  ({s})", r.kind.as_str(), r.display),
                        None => println!("{:<7} {}", r.kind.as_str(), r.display),
                    }
                }
            }
        }
    }
    Ok(())
}

/// `nbox device <value>` — look up a device with its interfaces, IPs, cables, VLANs.
async fn run_device(ctx: &Ctx, value: &str) -> Result<()> {
    const CAP: usize = 200;
    let client = connect(ctx)?;
    let device = client
        .device_by_ref(value)
        .await?
        .ok_or_else(|| not_found("device", value))?;

    let id = device.id;
    let (interfaces, ips) = tokio::try_join!(
        client.device_interfaces(id, CAP),
        client.device_ips(id, CAP)
    )?;

    let view = DeviceDetail::build(device, interfaces, ips);
    emit(ctx, &view, || println!("{}", view.to_plain()))
}

/// `nbox interface <device> <interface>` — show one interface and its addresses.
async fn run_interface(ctx: &Ctx, device: &str, interface: &str) -> Result<()> {
    const CAP: usize = 200;
    let client = connect(ctx)?;
    let dev = client
        .device_by_ref(device)
        .await?
        .ok_or_else(|| not_found("device", device))?;
    let iface = client
        .device_interface(dev.id, interface)
        .await?
        .ok_or_else(|| not_found("interface", interface))?;
    let ips = client.interface_ips(iface.id, CAP).await?;

    let view = InterfaceView::build(iface, ips);
    emit(ctx, &view, || println!("{}", view.to_plain()))
}

/// `nbox ip <address>` — resolve an IP and its most-specific parent prefix.
async fn run_ip(ctx: &Ctx, address: &str) -> Result<()> {
    let client = connect(ctx)?;
    let ip = client
        .ip_candidates(address)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| not_found("IP address", address))?;

    let host = address.split('/').next().unwrap_or(address);
    let parent = most_specific(client.prefixes_containing(host).await?);

    let view = IpView::build(ip, parent);
    emit(ctx, &view, || view.to_key_values().print())
}

/// `nbox prefix <cidr>` — show a prefix with its children and contained IPs.
async fn run_prefix(ctx: &Ctx, cidr: &str) -> Result<()> {
    const SECTION_CAP: usize = 50;
    let client = connect(ctx)?;
    let prefix = client
        .prefix_by_cidr(cidr)
        .await?
        .ok_or_else(|| not_found("prefix", cidr))?;

    let children = client.prefix_children(cidr, SECTION_CAP).await?;
    let ips = client.prefix_ips(cidr, SECTION_CAP).await?;

    let view = PrefixView::build(prefix, children, ips);
    emit(ctx, &view, || println!("{}", view.to_plain()))
}

/// `nbox vlan <vid|name>` — show a VLAN and the prefixes that reference it.
async fn run_vlan(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let vlan = client
        .vlan_by_ref(value)
        .await?
        .ok_or_else(|| not_found("VLAN", value))?;
    let prefixes = client.vlan_prefixes(vlan.id, 50).await?;

    let view = VlanView::build(vlan, prefixes);
    emit(ctx, &view, || println!("{}", view.to_plain()))
}

/// `nbox site <name|slug>` — show a site.
async fn run_site(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let site = client
        .site_by_ref(value)
        .await?
        .ok_or_else(|| not_found("site", value))?;

    let view = SiteView::from_model(site);
    emit(ctx, &view, || view.to_key_values().print())
}

/// `nbox rack <name|id>` — show a rack.
async fn run_rack(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let rack = client
        .rack_by_ref(value)
        .await?
        .ok_or_else(|| not_found("rack", value))?;

    let view = RackView::from_model(rack);
    emit(ctx, &view, || view.to_key_values().print())
}

/// `nbox open <kind/ref>` — resolve an object and open it in the browser.
async fn run_open(ctx: &Ctx, object_ref: &str) -> Result<()> {
    let (kind, value) = parse_object_ref(object_ref)?;

    let client = connect(ctx)?;
    let api_url = resolve_object_url(&client, kind, value)
        .await?
        .ok_or_else(|| not_found(kind, value))?;
    let web_url = util::format::api_to_web_url(&api_url);

    let report = serde_json::json!({ "url": web_url });
    emit(ctx, &report, || println!("{web_url}"))?;

    if let Err(e) = open::that(&web_url) {
        eprintln!("warning: could not launch a browser: {e}");
    }
    Ok(())
}

/// Resolve a `<kind>/<ref>` pair to the object's API URL, or `None` if no match.
async fn resolve_object_url(
    client: &NetBoxClient,
    kind: &str,
    value: &str,
) -> Result<Option<String>> {
    let url = match kind {
        "device" => client.device_by_ref(value).await?.map(|d| d.url),
        "site" => client.site_by_ref(value).await?.map(|s| s.url),
        "rack" => client.rack_by_ref(value).await?.map(|r| r.url),
        "vlan" => client.vlan_by_ref(value).await?.map(|v| v.url),
        "prefix" => client.prefix_by_cidr(value).await?.map(|p| p.url),
        "ip" | "ip-address" | "address" => client
            .ip_candidates(value)
            .await?
            .into_iter()
            .next()
            .map(|ip| ip.url),
        other => anyhow::bail!(
            "unknown object kind \"{other}\" (expected: device, site, rack, vlan, prefix, ip)"
        ),
    };
    Ok(url)
}

/// Split a `<kind>/<ref>` reference. The first `/` is the separator, so a value
/// may itself contain slashes (e.g. `prefix/10.0.0.0/24`).
fn parse_object_ref(s: &str) -> Result<(&str, &str)> {
    s.split_once('/')
        .filter(|(kind, value)| !kind.is_empty() && !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "object reference must be `<kind>/<ref>` (e.g. device/edge01)\n\nKinds: device, site, rack, vlan, prefix, ip"
            )
        })
}

/// A friendly "not found" error with an actionable suggestion (DESIGN §17).
/// Typed as [`error::NboxError::NotFound`] so it carries a stable exit code.
fn not_found(noun: &str, value: &str) -> anyhow::Error {
    error::NboxError::NotFound(format!(
        "no {noun} matched \"{value}\"\n\nTry:\n  nbox search {value}"
    ))
    .into()
}

#[cfg(test)]
mod tests {
    use super::{not_found, parse_object_ref};

    #[test]
    fn not_found_includes_actionable_suggestion() {
        let msg = format!("{:#}", not_found("device", "edge01"));
        assert!(msg.contains("no device matched \"edge01\""));
        assert!(msg.contains("Try:"));
        assert!(msg.contains("nbox search edge01"));
    }

    #[test]
    fn object_ref_splits_on_first_slash() {
        assert_eq!(
            parse_object_ref("device/edge01").unwrap(),
            ("device", "edge01")
        );
        // A value may contain slashes; only the first separates kind from ref.
        assert_eq!(
            parse_object_ref("prefix/10.0.0.0/24").unwrap(),
            ("prefix", "10.0.0.0/24")
        );
    }

    #[test]
    fn object_ref_requires_both_parts() {
        assert!(parse_object_ref("garbage").is_err());
        assert!(parse_object_ref("device/").is_err());
        assert!(parse_object_ref("/edge01").is_err());
    }
}
