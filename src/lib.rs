//! nbox — terminal UI and CLI for NetBox.
//!
//! Library crate root. See `DESIGN.md` and `ROADMAP.md` for the architecture
//! and phasing. The binary parses a [`cli::Cli`] and dispatches into [`run`].

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::CommandFactory;
use ipnet::IpNet;

use crate::cli::{Cli, Command};
use crate::domain::WithJournal;
use crate::domain::detail;
use crate::domain::journal_view::{JournalEntryRow, JournalView};
use crate::domain::tag_view::TagsView;
use crate::netbox::client::NetBoxClient;
use crate::netbox::models::ipam::{AvailablePrefix, Prefix};
use crate::netbox::search::{SearchFilters, SearchRequest};
use crate::output::Format;
use crate::output::plain::KeyValues;

pub mod cli;
pub mod config;
pub mod domain;
pub mod error;
pub mod mcp;
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

/// Render a serializable view per the selected format, or run `plain` for text.
fn emit<T: serde::Serialize>(ctx: &Ctx, view: &T, plain: impl FnOnce()) -> Result<()> {
    output::emit(ctx.format, &ctx.json_opts, view, plain)
}

/// Emit a detail view together with its recent journal entries (`--journal`).
///
/// JSON/CSV serialize the view flattened with a top-level `journal` array; plain
/// prints the view exactly as it renders without the flag (`inner_plain`) and
/// then appends a `Journal` section in the same style as `nbox journal`.
fn emit_with_journal<T: serde::Serialize>(
    ctx: &Ctx,
    view: T,
    journal: Vec<JournalEntryRow>,
    inner_plain: String,
) -> Result<()> {
    let wrapped = WithJournal::new(view, journal);
    emit(ctx, &wrapped, || {
        if !inner_plain.is_empty() {
            println!("{inner_plain}");
        }
        let body = JournalView {
            entries: wrapped.journal.clone(),
        }
        .to_plain();
        println!("\nJournal\n{body}");
    })
}

/// Resolve a detail object's journal rows for inline display, addressing it by
/// the same kind + reference the standalone `nbox journal` command uses.
async fn inline_journal(
    client: &NetBoxClient,
    kind: &str,
    value: &str,
) -> Result<Vec<JournalEntryRow>> {
    let (content_type, id) = resolve_content_type_id(client, kind, value).await?;
    detail::journal_rows(client, content_type, id).await
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
            tag,
            cols,
            partial,
        }) => {
            let filters = SearchFilters {
                status,
                site,
                tenant,
                role,
                tag,
            };
            run_search(&ctx, &query, limit, filters, cols, partial).await
        }
        Some(Command::Device { value, journal }) => run_device(&ctx, &value, journal).await,
        Some(Command::Ip {
            address,
            vrf,
            journal,
        }) => run_ip(&ctx, &address, vrf.as_deref(), journal).await,
        Some(Command::Prefix { cidr, vrf, journal }) => {
            run_prefix(&ctx, &cidr, vrf.as_deref(), journal).await
        }
        Some(Command::NextIp { prefix, count, vrf }) => {
            run_next_ip(&ctx, &prefix, count, vrf.as_deref()).await
        }
        Some(Command::NextPrefix {
            prefix,
            length,
            vrf,
        }) => run_next_prefix(&ctx, &prefix, length, vrf.as_deref()).await,
        Some(Command::Site { value, journal }) => run_site(&ctx, &value, journal).await,
        Some(Command::Rack { value, journal }) => run_rack(&ctx, &value, journal).await,
        Some(Command::Circuit { value, journal }) => run_circuit(&ctx, &value, journal).await,
        Some(Command::Aggregate { value }) => run_aggregate(&ctx, &value).await,
        Some(Command::Asn { asn }) => run_asn(&ctx, asn).await,
        Some(Command::IpRange { value }) => run_ip_range(&ctx, &value).await,
        Some(Command::Vlan {
            value,
            site,
            group,
            journal,
        }) => run_vlan(&ctx, &value, site.as_deref(), group.as_deref(), journal).await,
        Some(Command::Interface { device, interface }) => {
            run_interface(&ctx, &device, &interface).await
        }
        Some(Command::Open { object_ref }) => run_open(&ctx, &object_ref).await,
        Some(Command::Tags { limit }) => run_tags(&ctx, limit).await,
        Some(Command::Journal { kind, value, limit }) => {
            run_journal(&ctx, &kind, &value, limit).await
        }
        Some(Command::Raw { method, path }) => run_raw(&ctx, &method, &path).await,
        Some(Command::Status) => run_status(&ctx).await,
        Some(Command::Config { command }) => config::run_config(
            command,
            ctx.config_path.as_deref(),
            ctx.format,
            &ctx.json_opts,
        ),
        Some(Command::Profile { command }) => config::run_profile(
            command,
            ctx.config_path.as_deref(),
            ctx.format,
            &ctx.json_opts,
        ),
        Some(Command::Completions { shell }) => {
            let mut cmd = Cli::command();
            let bin = cmd.get_name().to_string();
            clap_complete::generate(shell.to_clap(), &mut cmd, bin, &mut std::io::stdout());
            Ok(())
        }
        Some(Command::Man) => {
            clap_mangen::Man::new(Cli::command()).render(&mut std::io::stdout())?;
            Ok(())
        }
        // stdout is reserved for the JSON-RPC stream — connect() and the
        // server itself print nothing, and logging already goes to stderr.
        Some(Command::Serve) => {
            let client = connect(&ctx)?;
            mcp::serve(client).await
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
    partial: bool,
) -> Result<()> {
    let client = connect(ctx)?;
    let outcome = client
        .search(SearchRequest {
            query: query.to_string(),
            limit,
            filters,
        })
        .await?;

    // Fail closed by default: if some endpoints failed, don't present partial
    // results as if they were complete. `--partial` opts into a best-effort run.
    if !outcome.errors.is_empty() {
        if !partial {
            anyhow::bail!(
                "search incomplete — {} endpoint(s) failed:\n  {}\n\nRe-run with --partial to accept partial results.",
                outcome.errors.len(),
                outcome.errors.join("\n  ")
            );
        }
        eprintln!(
            "warning: partial results — {} endpoint(s) failed:\n  {}",
            outcome.errors.len(),
            outcome.errors.join("\n  ")
        );
    }

    let results = outcome.results;
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
async fn run_device(ctx: &Ctx, value: &str, journal: bool) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::device_detail_by_ref(&client, value, &not_found).await?;
    if journal {
        let entries = inline_journal(&client, "device", value).await?;
        let plain = view.to_plain();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || println!("{}", view.to_plain()))
    }
}

/// `nbox interface <device> <interface>` — show one interface and its addresses.
async fn run_interface(ctx: &Ctx, device: &str, interface: &str) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::interface_view_by_ref(&client, device, interface, &not_found).await?;
    emit(ctx, &view, || println!("{}", view.to_plain()))
}

/// `nbox ip <address>` — resolve an IP (scoped by `--vrf`) and its parent prefix.
async fn run_ip(ctx: &Ctx, address: &str, vrf: Option<&str>, journal: bool) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::ip_view_by_ref(&client, address, vrf, &not_found).await?;
    if journal {
        let entries = inline_journal(&client, "ip", address).await?;
        let plain = view.to_key_values().render();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || view.to_key_values().print())
    }
}

/// Resolve a CIDR to a single prefix, scoped by `--vrf`. Ambiguous → exit 5.
async fn resolve_prefix(client: &NetBoxClient, cidr: &str, vrf: Option<&str>) -> Result<Prefix> {
    detail::resolve_prefix(client, cidr, vrf, &not_found).await
}

/// `nbox prefix <cidr>` — show a prefix (scoped by `--vrf`) with children and IPs.
async fn run_prefix(ctx: &Ctx, cidr: &str, vrf: Option<&str>, journal: bool) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::prefix_view_by_ref(&client, cidr, vrf, &not_found).await?;
    if journal {
        let entries = inline_journal(&client, "prefix", cidr).await?;
        let plain = view.to_plain();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || println!("{}", view.to_plain()))
    }
}

/// `nbox next-ip <prefix>` — the next available address(es) within a prefix.
async fn run_next_ip(ctx: &Ctx, prefix: &str, count: usize, vrf: Option<&str>) -> Result<()> {
    let client = connect(ctx)?;
    let p = resolve_prefix(&client, prefix, vrf).await?;
    let available = client.prefix_available_ips(p.id, count).await?;
    let addresses: Vec<String> = available
        .into_iter()
        .take(count)
        .map(|a| a.address)
        .collect();

    let report = serde_json::json!({ "prefix": p.prefix.clone(), "available": addresses.clone() });
    emit(ctx, &report, || {
        if addresses.is_empty() {
            eprintln!("no available addresses in {}", p.prefix);
        } else {
            for a in &addresses {
                println!("{a}");
            }
        }
    })
}

/// `nbox next-prefix <prefix>` — available free blocks, or the first of `--length`.
async fn run_next_prefix(
    ctx: &Ctx,
    prefix: &str,
    length: Option<u8>,
    vrf: Option<&str>,
) -> Result<()> {
    let client = connect(ctx)?;
    let p = resolve_prefix(&client, prefix, vrf).await?;
    let free = client.prefix_available_prefixes(p.id).await?;
    let available: Vec<String> = match length {
        Some(len) => first_subnet_of_length(&free, len).into_iter().collect(),
        None => free.into_iter().map(|f| f.prefix).collect(),
    };

    let report = serde_json::json!({ "prefix": p.prefix.clone(), "available": available.clone() });
    emit(ctx, &report, || {
        if available.is_empty() {
            eprintln!("no available prefixes in {}", p.prefix);
        } else {
            for a in &available {
                println!("{a}");
            }
        }
    })
}

/// The first free block of exactly `len` bits among the available prefixes,
/// computed locally (read-only) by subnetting each free block with `ipnet`.
pub(crate) fn first_subnet_of_length(free: &[AvailablePrefix], len: u8) -> Option<String> {
    for block in free {
        if let Ok(net) = block.prefix.parse::<IpNet>()
            && net.prefix_len() <= len
            && let Some(sub) = net.subnets(len).ok().and_then(|mut s| s.next())
        {
            return Some(sub.to_string());
        }
    }
    None
}

/// `nbox vlan <vid|name>` — show a VLAN and the prefixes that reference it.
/// A VID present at several sites/groups is scoped by `--site` / `--group`.
async fn run_vlan(
    ctx: &Ctx,
    value: &str,
    site: Option<&str>,
    group: Option<&str>,
    journal: bool,
) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::vlan_view_by_ref(&client, value, site, group, &not_found).await?;
    if journal {
        let entries = inline_journal(&client, "vlan", value).await?;
        let plain = view.to_plain();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || println!("{}", view.to_plain()))
    }
}

/// `nbox circuit <cid|id>` — show a circuit.
async fn run_circuit(ctx: &Ctx, value: &str, journal: bool) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::circuit_view_by_ref(&client, value, &not_found).await?;
    if journal {
        let entries = inline_journal(&client, "circuit", value).await?;
        let plain = view.to_key_values().render();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || view.to_key_values().print())
    }
}

/// `nbox ip-range <start|id>` — show an IP range.
async fn run_ip_range(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::ip_range_view_by_ref(&client, value, &not_found).await?;
    emit(ctx, &view, || view.to_key_values().print())
}

/// `nbox aggregate <cidr|id>` — show an aggregate.
async fn run_aggregate(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::aggregate_view_by_ref(&client, value, &not_found).await?;
    emit(ctx, &view, || view.to_key_values().print())
}

/// `nbox asn <asn>` — show an ASN.
async fn run_asn(ctx: &Ctx, asn: u32) -> Result<()> {
    let client = connect(ctx)?;
    let value = asn.to_string();
    let view = detail::asn_view_by_ref(&client, asn, &value, &not_found).await?;
    emit(ctx, &view, || view.to_key_values().print())
}

/// `nbox site <name|slug>` — show a site.
async fn run_site(ctx: &Ctx, value: &str, journal: bool) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::site_view_by_ref(&client, value, &not_found).await?;
    if journal {
        let entries = inline_journal(&client, "site", value).await?;
        let plain = view.to_key_values().render();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || view.to_key_values().print())
    }
}

/// `nbox rack <name|id>` — show a rack.
async fn run_rack(ctx: &Ctx, value: &str, journal: bool) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::rack_view_by_ref(&client, value, &not_found).await?;
    if journal {
        let entries = inline_journal(&client, "rack", value).await?;
        let plain = view.to_key_values().render();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || view.to_key_values().print())
    }
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

/// `nbox tags` — list tags.
async fn run_tags(ctx: &Ctx, limit: usize) -> Result<()> {
    let client = connect(ctx)?;
    let tags = client.tags(limit).await?;
    let view = TagsView::from_models(tags);
    emit(ctx, &view, || println!("{}", view.to_plain()))
}

/// `nbox journal <kind> <ref>` — recent journal entries for an object.
async fn run_journal(ctx: &Ctx, kind: &str, value: &str, limit: usize) -> Result<()> {
    let client = connect(ctx)?;
    let (content_type, id) = resolve_content_type_id(&client, kind, value).await?;
    let entries = client.journal_entries(content_type, id, limit).await?;

    let view = JournalView::from_models(entries);
    emit(ctx, &view, || println!("{}", view.to_plain()))
}

/// Resolve a `<kind> <ref>` to the object's dotted content type and numeric ID,
/// reusing the existing per-kind resolvers.
async fn resolve_content_type_id(
    client: &NetBoxClient,
    kind: &str,
    value: &str,
) -> Result<(&'static str, u64)> {
    let resolved = match kind {
        "device" => client
            .device_by_ref(value)
            .await?
            .map(|d| ("dcim.device", d.id)),
        "ip" => client
            .ip_candidates(value)
            .await?
            .into_iter()
            .next()
            .map(|i| ("ipam.ipaddress", i.id)),
        "prefix" => client
            .prefix_by_cidr(value)
            .await?
            .map(|p| ("ipam.prefix", p.id)),
        "vlan" => client
            .vlan_by_ref(value)
            .await?
            .map(|v| ("ipam.vlan", v.id)),
        "site" => client
            .site_by_ref(value)
            .await?
            .map(|s| ("dcim.site", s.id)),
        "rack" => client
            .rack_by_ref(value)
            .await?
            .map(|r| ("dcim.rack", r.id)),
        "circuit" => client
            .circuit_by_ref(value)
            .await?
            .map(|c| ("circuits.circuit", c.id)),
        other => anyhow::bail!(
            "unknown object kind \"{other}\" (expected: device, ip, prefix, vlan, site, rack, circuit)"
        ),
    };
    resolved.ok_or_else(|| not_found(kind, value))
}

/// `nbox raw <method> <path>` — a raw read-only API GET (escape hatch).
async fn run_raw(ctx: &Ctx, method: &str, path: &str) -> Result<()> {
    check_raw_method(method)?;
    let client = connect(ctx)?;
    let value: serde_json::Value = client.get(path, &[]).await?;
    emit(ctx, &value, || {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    })
}

/// Allow only `GET` for `nbox raw` until write support lands. Write verbs are a
/// deliberate v0.2+ feature behind the safe-write engine.
fn check_raw_method(method: &str) -> Result<()> {
    if method.eq_ignore_ascii_case("GET") {
        Ok(())
    } else {
        anyhow::bail!(
            "`nbox raw` only supports GET today; write verbs land with safe writes (v0.2+)"
        )
    }
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
    use super::{check_raw_method, error, first_subnet_of_length, not_found, parse_object_ref};
    use crate::domain::detail::resolve_unique;
    use crate::netbox::models::ipam::AvailablePrefix;

    #[test]
    fn raw_allows_get_only() {
        assert!(check_raw_method("GET").is_ok());
        assert!(check_raw_method("get").is_ok());
        assert!(check_raw_method("POST").is_err());
        assert!(check_raw_method("delete").is_err());
    }

    #[test]
    fn first_subnet_of_length_picks_first_fitting_block() {
        let ap = |p: &str| AvailablePrefix { prefix: p.into() };
        let free = vec![ap("10.0.0.0/24"), ap("10.1.0.0/16")];
        // A /26 fits in the first /24 → its first subnet.
        assert_eq!(
            first_subnet_of_length(&free, 26).as_deref(),
            Some("10.0.0.0/26")
        );
        // Nothing here can contain a /8 (both blocks are longer).
        assert_eq!(first_subnet_of_length(&free, 8), None);
        // A request equal to a block's length returns the block itself.
        assert_eq!(
            first_subnet_of_length(&[ap("10.0.0.0/24")], 24).as_deref(),
            Some("10.0.0.0/24")
        );
    }

    #[test]
    fn resolve_unique_distinguishes_none_one_many() {
        let label = |s: &String| s.clone();

        let one = resolve_unique(
            "device",
            "edge01",
            vec!["edge01".to_string()],
            label,
            &not_found,
        )
        .unwrap();
        assert_eq!(one, "edge01");

        let none = resolve_unique("device", "edge99", Vec::<String>::new(), label, &not_found)
            .unwrap_err();
        assert_eq!(error::NboxError::exit_code_for(&none), 4); // not found

        let many = resolve_unique(
            "device",
            "edge",
            vec!["edge01".to_string(), "edge02".to_string()],
            label,
            &not_found,
        )
        .unwrap_err();
        assert_eq!(error::NboxError::exit_code_for(&many), 5); // ambiguous
        assert!(format!("{many:#}").contains("edge01"));
    }

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
