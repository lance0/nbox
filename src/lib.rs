//! nbox — terminal UI and CLI for NetBox.
//!
//! Library crate root. See `DESIGN.md` and `ROADMAP.md` for the architecture
//! and phasing. The binary parses a [`cli::Cli`] and dispatches into [`run`].

#![warn(clippy::pedantic)]
// Curated pedantic allow-list. Pedantic is a project gate (every change is
// linted against it); these are the lints judged to be pure noise or stylistic
// churn for this codebase, so they're silenced crate-wide rather than chased.
// Everything else pedantic flags is fixed. Keep this list tight — prefer fixing.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::module_inception,
    clippy::needless_pass_by_value,
    clippy::redundant_else,
    clippy::too_many_lines,
    clippy::used_underscore_binding,
    // Backtick nags on proper nouns (NetBox, JSON-RPC, IPAM, …) in doc comments.
    clippy::doc_markdown,
    // Benign u16/usize/f64 math in the TUI (scroll offsets, terminal coordinates,
    // a percent bar): the casts are intentional and bounded.
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    // Stylistic: `if let … else` vs a two-arm `match`, and merging identical arms.
    // Mixing the two styles inside one `match` hurts readability more than it helps.
    clippy::single_match_else,
    clippy::match_same_arms,
    clippy::items_after_statements,
    // The CLI flags struct legitimately carries many bool options; collapsing them
    // would be an API change, not a cleanup.
    clippy::struct_excessive_bools
)]

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

/// The resolved logging destination + level, computed by [`resolve_logging`].
///
/// Keeping the decision in a plain value makes the precedence logic a pure
/// function that's unit-testable without touching the real environment, files,
/// or the global `tracing` subscriber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggingChoice {
    /// Where to write logs, if a file was configured. `None` ⇒ stderr only.
    pub log_file: Option<PathBuf>,
    /// The `tracing` filter spec (e.g. `warn`, `info`, `nbox=debug`).
    pub level: String,
}

/// Resolve the logging destination and level from the layered sources.
///
/// Pure: every input is passed in, nothing is read from the environment or
/// disk, so the precedence can be exercised in isolation.
///
/// - **File**: `--log-file` flag, else config `log_file`, else `None` (stderr).
/// - **Level**: `--log-level` flag, else config `log_level`, else `NBOX_LOG`,
///   else `RUST_LOG`, else `warn`.
#[must_use]
pub fn resolve_logging(
    flag_file: Option<&str>,
    config_file: Option<&str>,
    flag_level: Option<&str>,
    config_level: Option<&str>,
    nbox_log: Option<&str>,
    rust_log: Option<&str>,
) -> LoggingChoice {
    let log_file = flag_file
        .or(config_file)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);

    let level = flag_level
        .or(config_level)
        .or(nbox_log)
        .or(rust_log)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("warn")
        .to_string();

    LoggingChoice { log_file, level }
}

/// Initialize logging from a resolved [`LoggingChoice`].
///
/// stdout is never used — logs go to the file (if one is set) and/or stderr:
///
/// - **No file** (default): stderr only, exactly as before.
/// - **File set**: the file *and* stderr (mirrors xfr's normal-mode behavior),
///   so `--log-file` captures a record without hiding warnings from an
///   interactive run. The file is written via [`tracing_appender::non_blocking`]
///   on a non-rolling appender (`rolling::never`) so the path is honored exactly
///   as given.
///
/// **The returned [`WorkerGuard`] must be held for the program's lifetime**:
/// the non-blocking writer flushes on the worker thread, and dropping the guard
/// flushes + joins it. Drop it early and buffered log lines are lost. The
/// caller (see `main`) binds it for the duration of [`run`].
///
/// No-ops the subscriber install if one is already present (returns `None`).
#[must_use]
pub fn init_logging(choice: &LoggingChoice) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::new(&choice.level);

    let Some(path) = &choice.log_file else {
        // No file: stderr only, as before.
        let _ = fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init();
        return None;
    };

    // Ensure the parent directory exists, then split the path into (dir, file)
    // for the appender. `rolling::never` writes to exactly `dir/file` — no date
    // suffix — so the resolved path the user gave is honored verbatim.
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let file_name = path
        .file_name()
        .map_or_else(|| std::ffi::OsString::from("nbox.log"), ToOwned::to_owned);

    let appender = tracing_appender::rolling::never(dir, file_name);
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);

    let file_layer = fmt::layer()
        .with_ansi(false) // a file is not a terminal — no color escapes
        .with_writer(non_blocking);
    let stderr_layer = fmt::layer().with_writer(std::io::stderr);

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init();

    Some(guard)
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

/// Whether a detail command should fold in journal entries. Passing
/// `--journal-limit` implies `--journal`, so either flag turns the output on.
fn wants_journal(journal: bool, journal_limit: Option<usize>) -> bool {
    journal || journal_limit.is_some()
}

/// Resolve a detail object's journal rows for inline display, addressing it by
/// the same kind + reference the standalone `nbox journal` command uses. The cap
/// is `journal_limit` when `--journal-limit` was given, else [`JOURNAL_INLINE_MAX`].
async fn inline_journal(
    client: &NetBoxClient,
    kind: &str,
    value: &str,
    journal_limit: Option<usize>,
) -> Result<Vec<JournalEntryRow>> {
    let (content_type, id) = resolve_content_type_id(client, kind, value).await?;
    let max = journal_limit.unwrap_or(detail::JOURNAL_INLINE_MAX);
    detail::journal_rows(client, content_type, id, max).await
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
        // `--no-tui` is a hard guarantee of non-interactive behavior for agents
        // and scripts: both ways into the TUI (a bare `nbox` and an explicit
        // `nbox tui`) refuse rather than launch it. Refusing the explicit `tui`
        // command — instead of letting it win — is the predictable choice: a
        // script that sets `--no-tui` never gets a terminal UI, whatever follows.
        None | Some(Command::Tui) if cli.no_tui => Err(no_tui_refusal(cli.command.is_some())),
        None | Some(Command::Tui) => run_tui(&ctx).await,
        Some(Command::Search {
            query,
            limit,
            status,
            site,
            region,
            site_group,
            location,
            tenant,
            role,
            tag,
            vrf,
            cols,
            partial,
        }) => {
            let filters = SearchFilters {
                status,
                site,
                region,
                site_group,
                location,
                tenant,
                role,
                tag,
                vrf,
            };
            run_search(&ctx, &query, limit, filters, cols, partial).await
        }
        Some(Command::Device {
            value,
            journal,
            journal_limit,
        }) => run_device(&ctx, &value, journal, journal_limit).await,
        Some(Command::Ip {
            address,
            vrf,
            journal,
            journal_limit,
        }) => run_ip(&ctx, &address, vrf.as_deref(), journal, journal_limit).await,
        Some(Command::Prefix {
            cidr,
            vrf,
            journal,
            journal_limit,
        }) => run_prefix(&ctx, &cidr, vrf.as_deref(), journal, journal_limit).await,
        Some(Command::NextIp { prefix, count, vrf }) => {
            run_next_ip(&ctx, &prefix, count, vrf.as_deref()).await
        }
        Some(Command::NextPrefix {
            prefix,
            length,
            vrf,
        }) => run_next_prefix(&ctx, &prefix, length, vrf.as_deref()).await,
        Some(Command::Site {
            value,
            journal,
            journal_limit,
        }) => run_site(&ctx, &value, journal, journal_limit).await,
        Some(Command::Rack {
            value,
            journal,
            journal_limit,
        }) => run_rack(&ctx, &value, journal, journal_limit).await,
        Some(Command::Circuit {
            value,
            journal,
            journal_limit,
        }) => run_circuit(&ctx, &value, journal, journal_limit).await,
        Some(Command::Provider { value }) => run_provider(&ctx, &value).await,
        Some(Command::Aggregate {
            value,
            journal,
            journal_limit,
        }) => run_aggregate(&ctx, &value, journal, journal_limit).await,
        Some(Command::Asn {
            asn,
            journal,
            journal_limit,
        }) => run_asn(&ctx, asn, journal, journal_limit).await,
        Some(Command::IpRange {
            value,
            journal,
            journal_limit,
        }) => run_ip_range(&ctx, &value, journal, journal_limit).await,
        Some(Command::Tenant { value }) => run_tenant(&ctx, &value).await,
        Some(Command::Contact { value }) => run_contact(&ctx, &value).await,
        Some(Command::Vm { value }) => run_vm(&ctx, &value).await,
        Some(Command::Cluster { value }) => run_cluster(&ctx, &value).await,
        Some(Command::Vlan {
            value,
            site,
            group,
            journal,
            journal_limit,
        }) => {
            run_vlan(
                &ctx,
                &value,
                site.as_deref(),
                group.as_deref(),
                journal,
                journal_limit,
            )
            .await
        }
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
        Some(Command::Serve {
            http,
            http_token,
            oidc_issuer,
            audience,
            oidc_jwks_url,
            allowed_host,
            rate_limit,
        }) => {
            run_serve(
                &ctx,
                ServeFlags {
                    http,
                    http_token,
                    oidc_issuer,
                    audience,
                    oidc_jwks_url,
                    allowed_host,
                    rate_limit,
                },
            )
            .await
        }
    }
}

/// The `nbox serve` flags, resolved from the CLI before layering in the config's
/// `[serve]` section. Grouped so [`run_serve`] takes one argument, not many.
struct ServeFlags {
    http: Option<String>,
    http_token: Option<String>,
    oidc_issuer: Option<String>,
    audience: Option<String>,
    oidc_jwks_url: Option<String>,
    /// Extra DNS-rebinding allow-list hosts (repeatable `--allowed-host`); merged
    /// with `[serve].allowed_hosts`. Only honored in OIDC/routable mode.
    allowed_host: Vec<String>,
    /// Per-caller requests-per-minute cap; `None` ⇒ fall back to config / off.
    rate_limit: Option<u32>,
}

/// `nbox serve` — run the read-only MCP server.
///
/// Stdio is the zero-config default. `--http <ADDR>` (or `[serve].http` in the
/// config) switches to the opt-in HTTP transport, which requires the `http`
/// build feature. Adding `--oidc-issuer` + `--audience` puts it in OAuth 2.1
/// resource-server mode (inbound IdP JWTs validated on `/mcp`, a routable bind
/// allowed). Flags take precedence over the config file.
async fn run_serve(ctx: &Ctx, flags: ServeFlags) -> Result<()> {
    // Resolve every serve input from flags first, then the config's `[serve]`
    // section. A missing/unreadable config is fine here — stdio needs none of it,
    // and `connect()` reports a missing config on its own.
    let serve_cfg = load_serve_config(ctx);
    let http = flags.http.or(serve_cfg.http);
    let http_token = flags.http_token.or(serve_cfg.http_token);
    let oidc_issuer = flags.oidc_issuer.or(serve_cfg.oidc_issuer);
    let audience = flags.audience.or(serve_cfg.audience);
    let jwks_url = flags.oidc_jwks_url.or(serve_cfg.jwks_url);
    // DNS-rebinding allow-list extras: union of `--allowed-host` and the config's
    // `allowed_hosts` (additive, not override — both are explicit grants). Only
    // honored in OIDC/routable mode; the transport warns if set in loopback mode.
    let mut allowed_hosts = flags.allowed_host;
    allowed_hosts.extend(serve_cfg.allowed_hosts);
    // Per-caller rate limit: flag wins, then config, then off (0). Absent / 0 =
    // disabled, so existing behavior is unchanged unless the operator opts in.
    let rate_limit = flags.rate_limit.or(serve_cfg.rate_limit).unwrap_or(0);

    // OIDC resource-server mode is enabled by the issuer's presence; the audience
    // is then required (RFC 8707 — without an expected `aud`, nbox can't bind a
    // token to itself). Validate this before connecting so it fails fast (exit 2).
    // The (issuer, audience) pair, validated. `jwks_url` rides into the transport.
    let oidc = match (oidc_issuer, audience) {
        (Some(issuer), Some(audience)) => Some((issuer, audience)),
        (Some(_), None) => {
            return Err(error::NboxError::Usage(
                "--oidc-issuer requires --audience (the expected token `aud`, i.e. nbox's \
                 canonical resource URI). The IdP must mint that audience via the RFC 8707 \
                 `resource` parameter."
                    .to_string(),
            )
            .into());
        }
        (None, _) => None,
    };

    let client = connect(ctx)?;
    match http {
        None => mcp::serve(client).await,
        Some(addr) => {
            serve_http_or_explain(
                client,
                &addr,
                http_token,
                oidc,
                jwks_url,
                allowed_hosts,
                rate_limit,
            )
            .await
        }
    }
}

/// Read just the `[serve]` section, best-effort: a missing or unparseable config
/// yields the default (no HTTP, no token), so flags alone can still drive `serve`.
fn load_serve_config(ctx: &Ctx) -> config::ServeConfig {
    let Ok(path) = (match &ctx.config_path {
        Some(p) => Ok(p.clone()),
        None => config::default_path(),
    }) else {
        return config::ServeConfig::default();
    };
    config::load(&path).map(|c| c.serve).unwrap_or_default()
}

/// Dispatch to the HTTP transport when the `http` feature is built in; otherwise
/// fail with a clear usage error rather than silently falling back to stdio.
///
/// `oidc` is the validated `(issuer, audience)` pair (OIDC resource-server mode);
/// `jwks_url` is the optional JWKS override. Both are ignored without `--http`.
#[cfg(feature = "http")]
async fn serve_http_or_explain(
    client: NetBoxClient,
    addr: &str,
    token: Option<String>,
    oidc: Option<(String, String)>,
    jwks_url: Option<String>,
    allowed_hosts: Vec<String>,
    rate_limit: u32,
) -> Result<()> {
    let oidc = oidc.map(|(issuer, audience)| mcp::OidcArgs {
        issuer,
        audience,
        jwks_url,
    });
    mcp::serve_http(
        client,
        addr,
        mcp::ServeOptions {
            token,
            oidc,
            allowed_hosts,
            rate_limit,
        },
    )
    .await
}

// Mirrors the `http`-feature variant's async signature so the `run_serve` call
// site is feature-agnostic; it has nothing to await, hence the allow. The OIDC
// inputs are unused — without the feature there is no transport to configure.
#[cfg(not(feature = "http"))]
#[allow(clippy::unused_async)]
async fn serve_http_or_explain(
    _client: NetBoxClient,
    _addr: &str,
    _token: Option<String>,
    _oidc: Option<(String, String)>,
    _jwks_url: Option<String>,
    _allowed_hosts: Vec<String>,
    _rate_limit: u32,
) -> Result<()> {
    Err(error::NboxError::Usage(
        "`nbox serve --http` requires the `http` build feature, which this binary \
         was built without. Reinstall with `--features http`, or omit `--http` to \
         serve over stdio."
            .to_string(),
    )
    .into())
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

    let mut app = tui::state::App::new(
        client,
        &theme_name,
        name,
        base_url,
        status.netbox_version,
        Some(path),
    );
    // Honor NO_COLOR: render the TUI monochrome regardless of the configured
    // theme. The TUI is always a TTY when interactive, so the color decision here
    // keys on NO_COLOR (truecolor vs ANSI is moot when no color is emitted). See
    // `tui::term` for the full capability resolver used by other surfaces.
    if tui::term::no_color() {
        app.set_no_color();
    }
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
            print!("{}", output::csv::to_csv(&value, columns.as_deref())?);
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
async fn run_device(
    ctx: &Ctx,
    value: &str,
    journal: bool,
    journal_limit: Option<usize>,
) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::device_detail_by_ref(&client, value, &not_found).await?;
    if wants_journal(journal, journal_limit) {
        let entries = inline_journal(&client, "device", value, journal_limit).await?;
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
async fn run_ip(
    ctx: &Ctx,
    address: &str,
    vrf: Option<&str>,
    journal: bool,
    journal_limit: Option<usize>,
) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::ip_view_by_ref(&client, address, vrf, &not_found).await?;
    if wants_journal(journal, journal_limit) {
        let entries = inline_journal(&client, "ip", address, journal_limit).await?;
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
async fn run_prefix(
    ctx: &Ctx,
    cidr: &str,
    vrf: Option<&str>,
    journal: bool,
    journal_limit: Option<usize>,
) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::prefix_view_by_ref(&client, cidr, vrf, &not_found).await?;
    if wants_journal(journal, journal_limit) {
        let entries = inline_journal(&client, "prefix", cidr, journal_limit).await?;
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
    journal_limit: Option<usize>,
) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::vlan_view_by_ref(&client, value, site, group, &not_found).await?;
    if wants_journal(journal, journal_limit) {
        let entries = inline_journal(&client, "vlan", value, journal_limit).await?;
        let plain = view.to_plain();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || println!("{}", view.to_plain()))
    }
}

/// `nbox circuit <cid|id>` — show a circuit.
async fn run_circuit(
    ctx: &Ctx,
    value: &str,
    journal: bool,
    journal_limit: Option<usize>,
) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::circuit_view_by_ref(&client, value, &not_found).await?;
    if wants_journal(journal, journal_limit) {
        let entries = inline_journal(&client, "circuit", value, journal_limit).await?;
        let plain = view.to_key_values().render();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || view.to_key_values().print())
    }
}

/// `nbox ip-range <start|id>` — show an IP range.
async fn run_ip_range(
    ctx: &Ctx,
    value: &str,
    journal: bool,
    journal_limit: Option<usize>,
) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::ip_range_view_by_ref(&client, value, &not_found).await?;
    if wants_journal(journal, journal_limit) {
        let entries = inline_journal(&client, "ip-range", value, journal_limit).await?;
        let plain = view.to_key_values().render();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || view.to_key_values().print())
    }
}

/// `nbox aggregate <cidr|id>` — show an aggregate.
async fn run_aggregate(
    ctx: &Ctx,
    value: &str,
    journal: bool,
    journal_limit: Option<usize>,
) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::aggregate_view_by_ref(&client, value, &not_found).await?;
    if wants_journal(journal, journal_limit) {
        let entries = inline_journal(&client, "aggregate", value, journal_limit).await?;
        let plain = view.to_key_values().render();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || view.to_key_values().print())
    }
}

/// `nbox asn <asn>` — show an ASN.
async fn run_asn(ctx: &Ctx, asn: u32, journal: bool, journal_limit: Option<usize>) -> Result<()> {
    let client = connect(ctx)?;
    let value = asn.to_string();
    let view = detail::asn_view_by_ref(&client, asn, &value, &not_found).await?;
    if wants_journal(journal, journal_limit) {
        let entries = inline_journal(&client, "asn", &value, journal_limit).await?;
        let plain = view.to_key_values().render();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || view.to_key_values().print())
    }
}

/// `nbox site <name|slug>` — show a site.
async fn run_site(
    ctx: &Ctx,
    value: &str,
    journal: bool,
    journal_limit: Option<usize>,
) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::site_view_by_ref(&client, value, &not_found).await?;
    if wants_journal(journal, journal_limit) {
        let entries = inline_journal(&client, "site", value, journal_limit).await?;
        let plain = view.to_key_values().render();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || view.to_key_values().print())
    }
}

/// `nbox rack <name|id>` — show a rack.
async fn run_rack(
    ctx: &Ctx,
    value: &str,
    journal: bool,
    journal_limit: Option<usize>,
) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::rack_view_by_ref(&client, value, &not_found).await?;
    if wants_journal(journal, journal_limit) {
        let entries = inline_journal(&client, "rack", value, journal_limit).await?;
        let plain = view.to_key_values().render();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || view.to_key_values().print())
    }
}

/// `nbox tenant <slug|id>` — show a tenant.
async fn run_tenant(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::tenant_view_by_ref(&client, value, &not_found).await?;
    emit(ctx, &view, || view.to_key_values().print())
}

/// `nbox contact <name|id>` — show a contact.
async fn run_contact(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::contact_view_by_ref(&client, value, &not_found).await?;
    emit(ctx, &view, || view.to_key_values().print())
}

/// `nbox provider <slug|id>` — show a provider.
async fn run_provider(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::provider_view_by_ref(&client, value, &not_found).await?;
    emit(ctx, &view, || view.to_key_values().print())
}

/// `nbox vm <name|id>` — show a virtual machine.
async fn run_vm(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::vm_view_by_ref(&client, value, &not_found).await?;
    emit(ctx, &view, || view.to_key_values().print())
}

/// `nbox cluster <name|id>` — show a cluster.
async fn run_cluster(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::cluster_view_by_ref(&client, value, &not_found).await?;
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
///
/// Each kind reuses the same `*_by_ref` resolver the `device`/`search`/`journal`
/// commands use and reads the NetBox `url` the resolved object already carries,
/// so `open`'s link is identical to what those commands emit — no web routes are
/// hardcoded here.
pub(crate) async fn resolve_object_url(
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
        "circuit" => client.circuit_by_ref(value).await?.map(|c| c.url),
        "aggregate" => client.aggregate_by_ref(value).await?.map(|a| a.url),
        "asn" => {
            let number: u32 = value
                .parse()
                .map_err(|_| anyhow::anyhow!("ASN must be a number, got \"{value}\""))?;
            client.asn_by_ref(number).await?.map(|a| a.url)
        }
        "ip-range" | "iprange" => client.ip_range_by_ref(value).await?.map(|r| r.url),
        "tenant" => client.tenant_by_ref(value).await?.map(|t| t.url),
        "contact" => client.contact_by_ref(value).await?.map(|c| c.url),
        "provider" => client.provider_by_ref(value).await?.map(|p| p.url),
        "vm" => client.vm_by_ref(value).await?.map(|vm| vm.url),
        "cluster" => client.cluster_by_ref(value).await?.map(|c| c.url),
        // `interface/<device-ref>/<name>`: the device ref is the first segment of
        // `value`, and EVERYTHING after the next `/` is the interface name —
        // taken verbatim, since names contain slashes (e.g. `xe-0/0/1`,
        // `Ethernet1/49`). A future `interface-id/<id>` form could be added here.
        "interface" => {
            let (device, name) = value.split_once('/').filter(|(d, n)| !d.is_empty() && !n.is_empty()).ok_or_else(|| {
                error::NboxError::Usage(format!(
                    "interface reference must be `interface/<device>/<name>` (e.g. interface/edge01/xe-0/0/1)\n\nInterface names may contain slashes; the part after the device is the name verbatim. Got \"interface/{value}\"."
                ))
            })?;
            let dev = client
                .device_by_ref(device)
                .await?
                .ok_or_else(|| not_found("device", device))?;
            client.device_interface(dev.id, name).await?.map(|i| i.url)
        }
        other => anyhow::bail!(
            "unknown object kind \"{other}\" (expected: device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip-range, tenant, contact, provider, vm, cluster, interface)"
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
                "object reference must be `<kind>/<ref>` (e.g. device/edge01)\n\nKinds: device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip-range, tenant, contact, provider, vm, cluster"
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
///
/// This is the single source of truth for the journal-able kind set: both the
/// CLI `nbox journal`/`--journal` path and the MCP `nbox_journal` tool resolve
/// through here, so the two can't drift apart. Callers pass the CLI spelling of
/// the kind (e.g. `ip-range`); the MCP tool maps its underscore enum first.
pub(crate) async fn resolve_content_type_id(
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
        "aggregate" => client
            .aggregate_by_ref(value)
            .await?
            .map(|a| ("ipam.aggregate", a.id)),
        "asn" => {
            let number: u32 = value
                .parse()
                .map_err(|_| anyhow::anyhow!("ASN must be a number, got \"{value}\""))?;
            client.asn_by_ref(number).await?.map(|a| ("ipam.asn", a.id))
        }
        "ip-range" | "iprange" => client
            .ip_range_by_ref(value)
            .await?
            .map(|r| ("ipam.iprange", r.id)),
        "tenant" => client
            .tenant_by_ref(value)
            .await?
            .map(|t| ("tenancy.tenant", t.id)),
        "contact" => client
            .contact_by_ref(value)
            .await?
            .map(|c| ("tenancy.contact", c.id)),
        "provider" => client
            .provider_by_ref(value)
            .await?
            .map(|p| ("circuits.provider", p.id)),
        "vm" => client
            .vm_by_ref(value)
            .await?
            .map(|vm| ("virtualization.virtualmachine", vm.id)),
        "cluster" => client
            .cluster_by_ref(value)
            .await?
            .map(|c| ("virtualization.cluster", c.id)),
        other => anyhow::bail!(
            "unknown object kind \"{other}\" (expected: device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip-range, tenant, contact, provider, vm, cluster)"
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

/// The usage error `--no-tui` raises on an invocation that would otherwise launch
/// the TUI. Typed as [`error::NboxError::Usage`] so it exits `2`, like other usage
/// errors. `explicit_tui` is true when the `tui` subcommand was given (vs. a bare
/// `nbox`), so the message names the right conflict.
fn no_tui_refusal(explicit_tui: bool) -> anyhow::Error {
    let msg = if explicit_tui {
        "--no-tui conflicts with the `tui` command, which launches the interactive UI.\n\nDrop one or the other. Run `nbox --help` for the non-interactive commands."
    } else {
        "no command given; --no-tui suppresses the interactive UI.\n\nRun `nbox --help` for the available commands."
    };
    error::NboxError::Usage(msg.to_string()).into()
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
    use super::{
        check_raw_method, error, first_subnet_of_length, init_logging, no_tui_refusal, not_found,
        parse_object_ref, resolve_logging, wants_journal,
    };
    use crate::domain::detail::resolve_unique;
    use crate::netbox::models::ipam::AvailablePrefix;
    use std::path::PathBuf;

    // --- logging resolution (pure precedence) ------------------------------

    #[test]
    fn logging_level_precedence_flag_config_env_default() {
        // Flag wins over everything.
        let c = resolve_logging(
            None,
            None,
            Some("flag"),
            Some("config"),
            Some("nbox_log"),
            Some("rust_log"),
        );
        assert_eq!(c.level, "flag");
        // Config wins when no flag.
        let c = resolve_logging(
            None,
            None,
            None,
            Some("config"),
            Some("nbox_log"),
            Some("rust_log"),
        );
        assert_eq!(c.level, "config");
        // NBOX_LOG wins over RUST_LOG.
        let c = resolve_logging(None, None, None, None, Some("nbox_log"), Some("rust_log"));
        assert_eq!(c.level, "nbox_log");
        // RUST_LOG when it's the only one set.
        let c = resolve_logging(None, None, None, None, None, Some("rust_log"));
        assert_eq!(c.level, "rust_log");
        // Default when nothing is set.
        let c = resolve_logging(None, None, None, None, None, None);
        assert_eq!(c.level, "warn");
        // Blank/whitespace values are ignored, falling through to the default.
        let c = resolve_logging(Some("  "), None, Some("   "), None, Some(""), None);
        assert_eq!(c.level, "warn");
        assert_eq!(c.log_file, None);
    }

    #[test]
    fn logging_file_precedence_flag_config_none() {
        // Flag wins over config.
        let c = resolve_logging(
            Some("/tmp/flag.log"),
            Some("/tmp/config.log"),
            None,
            None,
            None,
            None,
        );
        assert_eq!(c.log_file, Some(PathBuf::from("/tmp/flag.log")));
        // Config when no flag.
        let c = resolve_logging(None, Some("/tmp/config.log"), None, None, None, None);
        assert_eq!(c.log_file, Some(PathBuf::from("/tmp/config.log")));
        // Neither → none (stderr).
        let c = resolve_logging(None, None, None, None, None, None);
        assert_eq!(c.log_file, None);
    }

    #[test]
    fn init_logging_writes_a_line_to_the_file() {
        use std::io::Read;

        // A subscriber may already be installed by another test in this binary;
        // `try_init` then no-ops and our events go elsewhere. Skip the assertion
        // in that case rather than emit a flaky failure.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nbox.log");
        let choice = super::LoggingChoice {
            log_file: Some(path.clone()),
            level: "info".to_string(),
        };
        let guard = init_logging(&choice);
        if guard.is_none() {
            // Another subscriber owns the process-global; can't assert on output.
            return;
        }

        tracing::info!(target: "nbox", "file-logging-test-marker");
        // Dropping the guard flushes + joins the non-blocking writer thread, so
        // the line is on disk before we read it (no sleep / no flakiness).
        drop(guard);

        let mut contents = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert!(
            contents.contains("file-logging-test-marker"),
            "log file should contain the emitted event, got: {contents:?}"
        );
    }

    #[test]
    fn wants_journal_is_implied_by_either_flag() {
        // Neither flag: no journal output (default behavior unchanged).
        assert!(!wants_journal(false, None));
        // `--journal` alone.
        assert!(wants_journal(true, None));
        // `--journal-limit` alone implies journal output.
        assert!(wants_journal(false, Some(3)));
        // `--journal-limit 0` still implies journal (it's `Some`).
        assert!(wants_journal(false, Some(0)));
        // Both set.
        assert!(wants_journal(true, Some(10)));
    }

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
    fn no_tui_refusal_is_usage_exit_2_and_explains() {
        // Bare `nbox --no-tui`: a usage error (exit 2) that names the empty
        // invocation and points at --help.
        let bare = no_tui_refusal(false);
        assert_eq!(error::NboxError::exit_code_for(&bare), 2);
        let bare_msg = format!("{bare:#}");
        assert!(bare_msg.contains("no command given"));
        assert!(bare_msg.contains("--no-tui"));
        assert!(bare_msg.contains("nbox --help"));

        // `nbox --no-tui tui`: still exit 2, but the message names the conflict
        // with the explicit `tui` command rather than an empty invocation.
        let explicit = no_tui_refusal(true);
        assert_eq!(error::NboxError::exit_code_for(&explicit), 2);
        let explicit_msg = format!("{explicit:#}");
        assert!(explicit_msg.contains("conflicts with the `tui` command"));
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

    // --- `nbox open` URL resolution ----------------------------------------
    //
    // `open` reuses each kind's `*_by_ref` resolver and reads the NetBox `url`
    // the resolved object carries, then `api_to_web_url` strips `/api`. These
    // tests mock the resolve and assert the final web URL per kind, so the link
    // `open` prints stays consistent with what `device`/`search` emit.

    use super::{NetBoxClient, resolve_object_url};
    use crate::config::ProfileConfig;
    use crate::util::format::api_to_web_url;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn open_client(server: &MockServer) -> NetBoxClient {
        let profile = ProfileConfig {
            url: server.uri(),
            ..Default::default()
        };
        NetBoxClient::new(&profile, None).unwrap()
    }

    /// Resolve `<kind>/<value>` and return the web URL `open` would print/open.
    async fn open_web_url(client: &NetBoxClient, kind: &str, value: &str) -> String {
        let api_url = resolve_object_url(client, kind, value)
            .await
            .unwrap()
            .expect("object should resolve");
        api_to_web_url(&api_url)
    }

    #[tokio::test]
    async fn open_device_url_drops_api_segment() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/devices/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 7, "url": format!("{}/api/dcim/devices/7/", server.uri()),
                "name": "edge07"
            })))
            .mount(&server)
            .await;

        let url = open_web_url(&open_client(&server), "device", "7").await;
        assert_eq!(url, format!("{}/dcim/devices/7/", server.uri()));
    }

    #[tokio::test]
    async fn open_ip_url_uses_first_candidate() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/ip-addresses/"))
            .and(query_param("address", "10.0.0.1/32"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{
                    "id": 3, "url": format!("{}/api/ipam/ip-addresses/3/", server.uri()),
                    "address": "10.0.0.1/32"
                }]
            })))
            .mount(&server)
            .await;

        let url = open_web_url(&open_client(&server), "ip", "10.0.0.1/32").await;
        assert_eq!(url, format!("{}/ipam/ip-addresses/3/", server.uri()));
    }

    #[tokio::test]
    async fn open_circuit_url_by_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/circuits/circuits/9/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 9, "url": format!("{}/api/circuits/circuits/9/", server.uri()),
                "cid": "WAN-9"
            })))
            .mount(&server)
            .await;

        let url = open_web_url(&open_client(&server), "circuit", "9").await;
        assert_eq!(url, format!("{}/circuits/circuits/9/", server.uri()));
    }

    #[tokio::test]
    async fn open_aggregate_url_by_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/aggregates/4/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 4, "url": format!("{}/api/ipam/aggregates/4/", server.uri()),
                "prefix": "10.0.0.0/8"
            })))
            .mount(&server)
            .await;

        let url = open_web_url(&open_client(&server), "aggregate", "4").await;
        assert_eq!(url, format!("{}/ipam/aggregates/4/", server.uri()));
    }

    #[tokio::test]
    async fn open_asn_url_by_number() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/asns/"))
            .and(query_param("asn", "65000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{
                    "id": 2, "url": format!("{}/api/ipam/asns/2/", server.uri()),
                    "asn": 65000
                }]
            })))
            .mount(&server)
            .await;

        let url = open_web_url(&open_client(&server), "asn", "65000").await;
        assert_eq!(url, format!("{}/ipam/asns/2/", server.uri()));
    }

    #[tokio::test]
    async fn open_ip_range_url_by_id_and_alias() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/ip-ranges/6/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 6, "url": format!("{}/api/ipam/ip-ranges/6/", server.uri()),
                "start_address": "10.0.0.1/32", "end_address": "10.0.0.9/32"
            })))
            .expect(2)
            .mount(&server)
            .await;

        let client = open_client(&server);
        // Both the canonical kind and the `iprange` alias resolve identically.
        let expected = format!("{}/ipam/ip-ranges/6/", server.uri());
        assert_eq!(open_web_url(&client, "ip-range", "6").await, expected);
        assert_eq!(open_web_url(&client, "iprange", "6").await, expected);
    }

    #[tokio::test]
    async fn open_unknown_kind_errors() {
        let server = MockServer::start().await;
        let err = resolve_object_url(&open_client(&server), "wormhole", "x")
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("unknown object kind"));
    }

    /// Mount the device-by-id + interface-by-name mocks `open interface/...` needs:
    /// a numeric device ref hits `/devices/{id}/` directly, then the interface is
    /// resolved by exact `name` (device-scoped) returning a single result.
    async fn mount_interface(server: &MockServer, device_id: u64, iface_id: u64, name: &str) {
        Mock::given(method("GET"))
            .and(path(format!("/api/dcim/devices/{device_id}/")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": device_id,
                "url": format!("{}/api/dcim/devices/{device_id}/", server.uri()),
                "name": "edge01"
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/interfaces/"))
            .and(query_param("device_id", device_id.to_string()))
            .and(query_param("name", name))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{
                    "id": iface_id,
                    "url": format!("{}/api/dcim/interfaces/{iface_id}/", server.uri()),
                    "name": name
                }]
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn open_interface_url_resolves_device_then_interface() {
        let server = MockServer::start().await;
        mount_interface(&server, 7, 42, "xe-0/0/1").await;

        // `interface/edge01/xe-0/0/1` → device=7, name=xe-0/0/1 (rest is the name).
        let url = open_web_url(&open_client(&server), "interface", "7/xe-0/0/1").await;
        assert_eq!(url, format!("{}/dcim/interfaces/42/", server.uri()));
    }

    #[tokio::test]
    async fn open_interface_name_may_contain_slashes() {
        let server = MockServer::start().await;
        // A name WITH a slash: proves "everything after the device is the name".
        mount_interface(&server, 7, 49, "Ethernet1/49").await;

        let url = open_web_url(&open_client(&server), "interface", "7/Ethernet1/49").await;
        assert_eq!(url, format!("{}/dcim/interfaces/49/", server.uri()));
    }

    #[tokio::test]
    async fn open_interface_not_found_is_exit_4() {
        let server = MockServer::start().await;
        // Device resolves, but no interface matches (exact + contains both empty).
        Mock::given(method("GET"))
            .and(path("/api/dcim/devices/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 7, "url": format!("{}/api/dcim/devices/7/", server.uri()),
                "name": "edge01"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/interfaces/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 0, "next": null, "previous": null, "results": []
            })))
            .mount(&server)
            .await;

        // `resolve_object_url` returns Ok(None); `run_open` maps that to not_found
        // (exit 4). Mirror that mapping here.
        let resolved = resolve_object_url(&open_client(&server), "interface", "7/xe-0/0/9")
            .await
            .unwrap();
        assert!(resolved.is_none());
        let err = not_found("interface", "7/xe-0/0/9");
        assert_eq!(error::NboxError::exit_code_for(&err), 4);
    }

    #[tokio::test]
    async fn open_interface_missing_name_is_usage_exit_2() {
        let server = MockServer::start().await;
        // `interface/edge01` with no name part → usage error (exit 2), no request.
        let err = resolve_object_url(&open_client(&server), "interface", "edge01")
            .await
            .unwrap_err();
        assert_eq!(error::NboxError::exit_code_for(&err), 2);
        assert!(format!("{err:#}").contains("interface/<device>/<name>"));
    }
}
