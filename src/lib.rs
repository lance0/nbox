//! nbox — terminal UI and CLI for NetBox.
//!
//! Library crate root. See `DESIGN.md` and `ROADMAP.md` for the architecture
//! and phasing. The binary parses a [`cli::Cli`] and dispatches into [`run`].

// Pedantic is a project gate, configured package-wide in `[lints.clippy]` in
// Cargo.toml so it covers the lib, bin, AND the integration test crates uniformly
// (inner `#![warn(...)]` attributes here would reach only this lib crate). The
// curated allow-list lives there too.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::CommandFactory;
use ipnet::IpNet;

use crate::cli::{Cli, Command};
use crate::domain::WithJournal;
use crate::domain::detail;
use crate::domain::history_view::HistoryView;
use crate::domain::journal_view::{JournalEntryRow, JournalView};
use crate::domain::tag_view::TagsView;
use crate::domain::tagged_view::{ResolvedTag, TaggedObjectView, TaggedReport};
use crate::netbox::capabilities::SurfaceRouting;
use crate::netbox::client::NetBoxClient;
use crate::netbox::models::ipam::{AvailablePrefix, Prefix};
use crate::netbox::mutation::{MutationPlan, MutationReceipt};
use crate::netbox::search::{SearchFilters, SearchRequest};
use crate::netbox::write_audit;
use crate::output::Format;
use crate::output::plain::KeyValues;

pub mod cache;
pub mod cli;
pub mod config;
pub mod domain;
pub mod error;
pub mod export;
pub mod mac;
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
    /// `--no-tui`: the non-interactive guarantee for agents/scripts. The dispatch
    /// in [`run`] already refuses launching the TUI; carried here so `run_tui` can
    /// also refuse the first-run onboarding *wizard* (which is just as interactive)
    /// if it's ever reached under the flag — exit 2 with guidance, never prompt.
    no_tui: bool,
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
        no_tui: cli.no_tui,
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
            owner,
            owner_group,
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
                owner,
                owner_group,
                vrf,
            };
            Box::pin(run_search(&ctx, &query, limit, filters, cols, partial)).await
        }
        Some(Command::Device {
            value,
            journal,
            journal_limit,
            action,
        }) => match action {
            None => run_device(&ctx, &value, journal, journal_limit).await,
            Some(crate::cli::DeviceAction::Set {
                field,
                value: new_value,
                message,
                dry_run,
                confirm,
                allow_writes,
            }) => {
                Box::pin(run_device_set(
                    &ctx,
                    &value,
                    &field,
                    &new_value,
                    message.as_deref(),
                    dry_run,
                    confirm,
                    allow_writes,
                ))
                .await
            }
        },
        Some(Command::Ip {
            address,
            vrf,
            journal,
            journal_limit,
            action,
        }) => match action {
            None => {
                run_ip(
                    &ctx,
                    address.as_deref(),
                    vrf.as_deref(),
                    journal,
                    journal_limit,
                )
                .await
            }
            Some(crate::cli::IpAction::Reserve {
                prefix,
                vrf: reserve_vrf,
                description,
                dns_name,
                count,
                message,
                dry_run,
                confirm,
                allow_writes,
            }) => {
                let effective_vrf = reserve_vrf_scope(vrf.as_deref(), reserve_vrf.as_deref())?;
                Box::pin(run_ip_reserve(
                    &ctx,
                    &prefix,
                    effective_vrf,
                    description.as_deref(),
                    dns_name.as_deref(),
                    count,
                    message.as_deref(),
                    dry_run,
                    confirm,
                    allow_writes,
                ))
                .await
            }
        },
        Some(Command::Prefix {
            cidr,
            vrf,
            journal,
            journal_limit,
            action,
        }) => match action {
            None => run_prefix(&ctx, &cidr, vrf.as_deref(), journal, journal_limit).await,
            Some(crate::cli::PrefixAction::Reserve {
                length,
                vrf: reserve_vrf,
                description,
                message,
                dry_run,
                confirm,
                allow_writes,
            }) => {
                let effective_vrf = reserve_vrf_scope(vrf.as_deref(), reserve_vrf.as_deref())?;
                Box::pin(run_prefix_reserve(
                    &ctx,
                    &cidr,
                    effective_vrf,
                    length,
                    description.as_deref(),
                    message.as_deref(),
                    dry_run,
                    confirm,
                    allow_writes,
                ))
                .await
            }
        },
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
        Some(Command::RackGroup { value }) => run_rack_group(&ctx, &value).await,
        Some(Command::Circuit {
            value,
            journal,
            journal_limit,
        }) => run_circuit(&ctx, &value, journal, journal_limit).await,
        Some(Command::Provider { value }) => run_provider(&ctx, &value).await,
        Some(Command::VirtualCircuit { value }) => run_virtual_circuit(&ctx, &value).await,
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
            action,
        }) => match action {
            None => run_ip_range(&ctx, &value, journal, journal_limit).await,
            Some(crate::cli::IpRangeAction::Reserve {
                description,
                dns_name,
                count,
                message,
                dry_run,
                confirm,
                allow_writes,
            }) => {
                Box::pin(run_ip_range_reserve(
                    &ctx,
                    &value,
                    description.as_deref(),
                    dns_name.as_deref(),
                    count,
                    message.as_deref(),
                    dry_run,
                    confirm,
                    allow_writes,
                ))
                .await
            }
        },
        Some(Command::Tenant { value }) => run_tenant(&ctx, &value).await,
        Some(Command::Contact { value }) => run_contact(&ctx, &value).await,
        Some(Command::Vm { value }) => run_vm(&ctx, &value).await,
        Some(Command::VmType { value }) => run_vm_type(&ctx, &value).await,
        Some(Command::Cluster { value }) => run_cluster(&ctx, &value).await,
        Some(Command::Vrf { value }) => run_vrf(&ctx, &value).await,
        Some(Command::RouteTarget { value }) => run_route_target(&ctx, &value).await,
        Some(Command::Mac { value }) => run_mac(&ctx, &value).await,
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
        Some(Command::Interface {
            device,
            interface,
            action,
        }) => match action {
            None => run_interface(&ctx, &device, &interface).await,
            Some(crate::cli::InterfaceAction::Set {
                field,
                value,
                message,
                dry_run,
                confirm,
                allow_writes,
            }) => {
                Box::pin(run_interface_set(
                    &ctx,
                    &device,
                    &interface,
                    &field,
                    &value,
                    message.as_deref(),
                    dry_run,
                    confirm,
                    allow_writes,
                ))
                .await
            }
        },
        Some(Command::Open { object_ref }) => run_open(&ctx, &object_ref).await,
        Some(Command::Tags { limit }) => run_tags(&ctx, limit).await,
        Some(Command::Tagged { tag, limit }) => run_tagged(&ctx, &tag, limit).await,
        Some(Command::Tag { action }) => match action {
            crate::cli::TagAction::Add {
                object_type,
                object_name,
                tag,
                message,
                dry_run,
                confirm,
                allow_writes,
            } => {
                Box::pin(run_tag_write(
                    &ctx,
                    detail::TagOperation::Add,
                    &object_type,
                    &object_name,
                    &tag,
                    message.as_deref(),
                    dry_run,
                    confirm,
                    allow_writes,
                ))
                .await
            }
            crate::cli::TagAction::Remove {
                object_type,
                object_name,
                tag,
                message,
                dry_run,
                confirm,
                allow_writes,
            } => {
                Box::pin(run_tag_write(
                    &ctx,
                    detail::TagOperation::Remove,
                    &object_type,
                    &object_name,
                    &tag,
                    message.as_deref(),
                    dry_run,
                    confirm,
                    allow_writes,
                ))
                .await
            }
        },
        Some(Command::Journal { kind, value, limit }) => {
            run_journal(&ctx, &kind, &value, limit).await
        }
        Some(Command::History {
            kind,
            value,
            limit,
            diff,
        }) => run_history(&ctx, &kind, &value, limit, diff).await,
        Some(Command::Export { action }) => run_export(&ctx, action).await,
        Some(Command::Raw { method, path }) => run_raw(&ctx, &method, &path).await,
        Some(Command::Status) => run_status(&ctx).await,
        Some(Command::Config { command }) => config::run_config(
            command,
            ctx.config_path.as_deref(),
            ctx.profile.as_deref(),
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
        Some(Command::Man { out_dir }) => run_man(out_dir.as_deref()),
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
            allow_writes,
            print_config,
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
                    allow_writes,
                    print_config,
                },
            )
            .await
        }
    }
}

/// `nbox ip` and `nbox prefix` each have a read-path `--vrf`, and their
/// `reserve` subcommand repeats it so the write reads naturally as
/// `nbox ip reserve <cidr> --vrf <name>`. Clap accepts the parent spelling too
/// (`nbox ip --vrf <name> reserve <cidr>`), so fold both placements into one
/// effective scope and reject conflicting duplicate input before any
/// network/write work. Shared by `ip reserve` and `prefix reserve`.
fn reserve_vrf_scope<'a>(
    parent_vrf: Option<&'a str>,
    reserve_vrf: Option<&'a str>,
) -> Result<Option<&'a str>> {
    match (parent_vrf, reserve_vrf) {
        (Some(parent), Some(reserve)) if !parent.eq_ignore_ascii_case(reserve) => {
            Err(error::NboxError::Usage(
                "conflicting --vrf values; pass --vrf once, either before or \
                 after the `reserve` subcommand"
                    .to_string(),
            )
            .into())
        }
        (Some(parent), Some(_) | None) => Ok(Some(parent)),
        (None, Some(reserve)) => Ok(Some(reserve)),
        (None, None) => Ok(None),
    }
}

/// `nbox man` — generate man pages (roff) for the CLI.
///
/// With no `out_dir`, write the single top-level page to stdout (the original
/// `nbox man > nbox.1` contract). Given a directory, write the full set there:
/// the top-level `nbox.1` plus one page per (sub)command, named for the full
/// invocation — `nbox-device.1`, `nbox-config.1`, and the nested
/// `nbox-config-init.1`, `nbox-profile-add.1`, … — so `man nbox-<sub>` resolves
/// once installed. `clap_mangen` renders one page per `clap::Command`, and
/// per-subcommand flags only appear on their own pages, so the directory form is
/// what a real man-page install wants.
fn run_man(out_dir: Option<&std::path::Path>) -> Result<()> {
    let cmd = Cli::command();

    let Some(dir) = out_dir else {
        // Back-compat: bare `nbox man` streams the top-level page to stdout.
        clap_mangen::Man::new(cmd).render(&mut std::io::stdout())?;
        return Ok(());
    };

    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating man-page output dir {}", dir.display()))?;

    // The top-level `nbox.1` renders as-is: its name *is* `nbox`, so the title,
    // NAME, and SYNOPSIS already read correctly. Then walk the tree, writing one
    // page per (sub)command with the right title + invocation (see
    // `write_subcommand_pages`).
    let bin = cmd.get_name().to_string();
    write_man_page(dir, &format!("{bin}.1"), cmd.clone())?;
    write_subcommand_pages(dir, &cmd, &bin)?;

    Ok(())
}

/// Recursively write a man page per subcommand under `parent`.
///
/// `prefix` is the full invocation path of `parent` (`nbox`, then `nbox config`,
/// …). For each child, the page is titled for the dashed lookup name
/// (`nbox-config-init`) while its SYNOPSIS shows the real space-separated
/// invocation (`nbox config init …`) — see [`write_man_page`] for how the two are
/// set. Nested-subcommand parents (`config`, `profile`) recurse so their children
/// get pages too, leaving no `SEE ALSO`/cross-reference dangling at a page that
/// was never generated. The auto-`help` subcommand is dropped (it carries no man
/// content of its own and would otherwise be referenced but never written).
fn write_subcommand_pages(
    dir: &std::path::Path,
    parent: &clap::Command,
    prefix: &str,
) -> Result<()> {
    for sub in parent.get_subcommands() {
        if sub.get_name() == "help" {
            continue;
        }
        let invocation = format!("{prefix} {}", sub.get_name()); // e.g. `nbox config init`
        let dashed = invocation.replace(' ', "-"); // e.g. `nbox-config-init`
        // Prepare the command so the rendered roff reads correctly:
        // - `display_name` drives the `.TH` title + NAME section → `nbox-config-init`
        // - `bin_name` drives the SYNOPSIS usage line → `nbox config init …`
        // - dropping the help subcommand keeps the SUBCOMMANDS cross-refs pointing
        //   only at pages we actually generate.
        let prepared = sub
            .clone()
            .display_name(dashed.clone())
            .bin_name(invocation.clone())
            .disable_help_subcommand(true);
        write_man_page(dir, &format!("{dashed}.1"), prepared)?;

        // Recurse into nested-subcommand parents (config/profile) so their
        // children (`nbox-config-init.1`, `nbox-profile-add.1`, …) exist.
        if sub.get_subcommands().next().is_some() {
            write_subcommand_pages(dir, sub, &invocation)?;
        }
    }
    Ok(())
}

/// Render one `clap` command to `<dir>/<file>` as roff. The command is taken by
/// value already configured with its `display_name`/`bin_name` (see
/// [`write_subcommand_pages`]), so the rendered title and SYNOPSIS are correct.
fn write_man_page(dir: &std::path::Path, file: &str, cmd: clap::Command) -> Result<()> {
    let path = dir.join(file);
    let mut out = std::fs::File::create(&path)
        .with_context(|| format!("creating man page {}", path.display()))?;
    clap_mangen::Man::new(cmd)
        .render(&mut out)
        .with_context(|| format!("rendering man page {}", path.display()))?;
    Ok(())
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
    /// `--print-config`: print the `mcpServers` snippet and exit (no connect).
    /// `--allow-writes`: enable MCP write tools (Pattern 2). `false` (the
    /// default) keeps the server read-only.
    allow_writes: bool,
    print_config: bool,
}

/// `nbox serve` — run the MCP server (read-only by default).
///
/// Stdio is the zero-config default. `--http <ADDR>` (or `[serve].http` in the
/// config) switches to the opt-in HTTP transport, which requires the `http`
/// build feature. Adding `--oidc-issuer` + `--audience` puts it in OAuth 2.1
/// resource-server mode (inbound IdP JWTs validated on `/mcp`, a routable bind
/// allowed). Flags take precedence over the config file.
async fn run_serve(ctx: &Ctx, flags: ServeFlags) -> Result<()> {
    // `--print-config` is a pure helper: emit the `mcpServers` snippet and exit,
    // before resolving any serve config or connecting to NetBox. Works with no
    // config file and no token — run it anytime to get the paste-ready block.
    if flags.print_config {
        let cfg = build_mcp_config(ctx);
        // stdout carries only the JSON (data); pretty-printed for pasteability.
        serde_json::to_writer_pretty(std::io::stdout(), &cfg).context("writing MCP config")?;
        println!();
        return Ok(());
    }

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
    // Write tools require both the flag/config gate AND the HTTP transport
    // (writes need OIDC identity → per-user token resolution via the vault).
    let allow_writes = flags.allow_writes || serve_cfg.allow_writes;

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
    // The long-lived server shares a read cache across tool calls (chatty agents
    // re-read the same object graph); agents can drop it with `nbox_cache_clear`.
    let cache = serve_cache(ctx, &client);

    // Build the per-user credential vault when writes are enabled. The vault
    // maps OIDC `sub` → env var name holding a per-user NetBox token. Writes
    // require the HTTP transport (OIDC identity); stdio has no caller identity,
    // so writes on stdio are rejected by the vault being `None`.
    let vault = if allow_writes && http.is_some() {
        Some(mcp::vault::CredentialVault::new(serve_cfg.vault, true))
    } else {
        None
    };

    match http {
        None => mcp::serve(client, cache).await,
        Some(addr) => {
            serve_http_or_explain(
                client,
                &addr,
                http_token,
                oidc,
                jwks_url,
                allowed_hosts,
                rate_limit,
                cache,
                vault,
                ctx.profile.clone().unwrap_or_default(),
            )
            .await
        }
    }
}

/// Build the read cache for `nbox serve` from the `[cache]` config (best-effort —
/// a missing/unreadable config yields the defaults: cache on, 30s) and the
/// connected client (its URL + backend key the cache partition).
/// Build the `mcpServers` JSON object most MCP hosts read, for `nbox serve
/// --print-config`. Pure (no network, no config read beyond the globals already
/// on `Ctx`), so it's unit-testable and works with no token/config. The
/// `command` is the absolute path to this binary when discoverable, else the
/// bare `nbox` (the operator puts it on `PATH` or swaps in an absolute path).
/// `args` always begins with `serve` and echoes `--profile`/`--config` when set
/// so the snippet reproduces the invocation. `env.NBOX_TOKEN` is a placeholder
/// — the operator sets it (or removes the block if `nbox config init` holds the
/// token). Never echoes a real token.
fn build_mcp_config(ctx: &Ctx) -> serde_json::Value {
    let command = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "nbox".to_string());

    let mut args = vec!["serve".to_string()];
    if let Some(profile) = &ctx.profile {
        args.push("--profile".to_string());
        args.push(profile.clone());
    }
    if let Some(config) = &ctx.config_path
        && let Some(s) = config.to_str()
    {
        args.push("--config".to_string());
        args.push(s.to_string());
    }

    serde_json::json!({
        "mcpServers": {
            "nbox": {
                "command": command,
                "args": args,
                "env": {
                    "NBOX_TOKEN": "<set-your-token>"
                }
            }
        }
    })
}

fn serve_cache(ctx: &Ctx, client: &NetBoxClient) -> cache::Cache {
    let settings = ctx
        .config_path
        .clone()
        .or_else(|| config::default_path().ok())
        .and_then(|p| config::load(&p).ok())
        .map(|c| c.cache)
        .unwrap_or_default();
    let partition = cache::profile_partition("serve", client.base_url().as_str());
    cache::Cache::from_settings(partition, &settings)
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
#[allow(clippy::too_many_arguments)] // a flag/config forwarding wrapper; bundling would add indirection
async fn serve_http_or_explain(
    client: NetBoxClient,
    addr: &str,
    token: Option<String>,
    oidc: Option<(String, String)>,
    jwks_url: Option<String>,
    allowed_hosts: Vec<String>,
    rate_limit: u32,
    cache: cache::Cache,
    vault: Option<mcp::vault::CredentialVault>,
    profile: String,
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
            cache,
            vault,
            profile,
        },
    )
    .await
}

// Mirrors the `http`-feature variant's async signature so the `run_serve` call
// site is feature-agnostic; it has nothing to await, hence the allow. The OIDC
// inputs are unused — without the feature there is no transport to configure.
#[cfg(not(feature = "http"))]
#[allow(clippy::unused_async, clippy::too_many_arguments)]
async fn serve_http_or_explain(
    _client: NetBoxClient,
    _addr: &str,
    _token: Option<String>,
    _oidc: Option<(String, String)>,
    _jwks_url: Option<String>,
    _allowed_hosts: Vec<String>,
    _rate_limit: u32,
    _cache: cache::Cache,
    _vault: Option<mcp::vault::CredentialVault>,
    _profile: String,
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
    Ok(connect_named(ctx)?.0)
}

/// Like [`connect`] but also returns the resolved profile name, for the write
/// audit event (ADR-0001 §8: the audit records the profile). The write path uses
/// this so it doesn't re-load the config a second time.
fn connect_named(ctx: &Ctx) -> Result<(NetBoxClient, String)> {
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

    let token = config::resolve_token(profile, &path, &name);
    let client = NetBoxClient::new(profile, token)?;
    Ok((client, name))
}

/// `nbox` / `nbox tui` — launch the interactive TUI.
///
/// First-run path: when there's no usable config (no file, no profiles, or no
/// resolvable active profile and no `--profile`) we don't hard-fail with "run
/// `nbox config init`" — we run a guided in-TUI onboarding wizard that captures a
/// profile, writes it (active), then drops into the normal TUI. The wizard and the
/// app loop share one terminal so there's no re-init flicker between them.
async fn run_tui(ctx: &Ctx) -> Result<()> {
    let path = match &ctx.config_path {
        Some(p) => p.clone(),
        None => config::default_path()?,
    };

    // Onboard before `config::load` can error on a missing/empty config. Drive the
    // wizard and the app loop on a single terminal (init once, restore once).
    if config::needs_onboarding(&path, ctx.profile.as_deref()) {
        // M14: the onboarding wizard is interactive — `--no-tui` must refuse it too
        // (the same exit-2 guarantee as refusing the TUI), never drop a script into
        // a prompt. (The top-level dispatch already guards the common path; this
        // keeps the invariant local to where onboarding is decided.)
        if ctx.no_tui {
            return Err(no_tui_onboarding_refusal());
        }
        let mut terminal = ratatui::init();
        let result = run_tui_onboarding(ctx, &path, &mut terminal).await;
        ratatui::restore();
        return result;
    }

    // Normal launch: build the app, then run it on its own terminal.
    let cfg = config::load(&path)?;
    let app = build_tui_app(ctx, &path, &cfg).await?;
    tui::app::run(app, cfg.ui.refresh_secs).await
}

/// The first-run flow on an already-initialized terminal: run the onboarding
/// wizard, and on a successful save reload the config + connect + run the normal
/// app loop on the same terminal. A clean quit (Esc/Ctrl+C) writes nothing and
/// returns without launching the app.
async fn run_tui_onboarding(
    ctx: &Ctx,
    path: &std::path::Path,
    terminal: &mut ratatui::DefaultTerminal,
) -> Result<()> {
    // The wizard renders with the configured (or default) theme, honoring NO_COLOR.
    let theme_name = config::load(path)
        .ok()
        .map_or_else(|| "default".to_string(), |c| c.ui.theme);
    let theme = if tui::term::no_color() {
        tui::theme::Theme::no_color()
    } else {
        tui::theme::Theme::by_name(&theme_name)
    };

    let Some(outcome) = tui::onboarding::run(terminal, path, &theme).await? else {
        // Clean quit — nothing was written.
        return Ok(());
    };

    // The wizard wrote + activated the profile. Connect as if `--profile name` was
    // passed (its own active_profile is already set, but be explicit), then run.
    let cfg = config::load(path)?;
    let onboarded = Ctx {
        config_path: ctx.config_path.clone(),
        profile: Some(outcome.name.clone()),
        format: ctx.format,
        json_opts: ctx.json_opts.clone(),
        no_tui: ctx.no_tui,
    };
    let mut app = build_tui_app(&onboarded, path, &cfg).await?;
    // When no token landed anywhere (no config token and no token_env), steer the
    // user toward an env var so the freshly-launched app's first requests succeed.
    if outcome.needs_env_guidance {
        app.set_initial_status(
            "profile saved — set NBOX_TOKEN or a token_env to authenticate",
            tui::theme::Severity::Warning,
        );
    }
    tui::app::run_on(terminal, &mut app, cfg.ui.refresh_secs).await
}

/// Decide a freshly-launched TUI's version string + startup status from the
/// connect probe (`client.status()`). A reachable-but-too-old server is fatal
/// (`Err`) — nothing the user can fix in-app. A connection/auth failure is
/// recoverable (fix the profile via `S`, then reconnect), so it yields an empty
/// version + an actionable Error banner instead of a hard exit. Success yields
/// the version + a brief Success "connected" cue. Pure, so it's unit-testable.
fn tui_startup_status(
    probe: Result<crate::netbox::status::Status>,
) -> Result<(String, String, crate::tui::theme::Severity)> {
    use crate::netbox::status::{MIN_MAJOR, MIN_MINOR, meets_minimum};
    use crate::tui::theme::Severity;
    match probe {
        Ok(status) => {
            anyhow::ensure!(
                meets_minimum(&status.netbox_version, MIN_MAJOR, MIN_MINOR),
                "NetBox {} is unsupported; nbox requires {MIN_MAJOR}.{MIN_MINOR}+",
                status.netbox_version
            );
            let msg = format!("connected to NetBox v{}", status.netbox_version);
            Ok((status.netbox_version, msg, Severity::Success))
        }
        Err(e) => Ok((
            String::new(),
            format!("not connected — {e:#}. Press S to edit the profile or set NBOX_TOKEN."),
            Severity::Error,
        )),
    }
}

/// Build a connected [`tui::state::App`] from a loaded config: resolve the active
/// (or `--profile`) profile, build + probe the client (the 4.2 floor + version
/// for the status line), and seed the session profiles + live UI settings. Shared
/// by the normal launch and the post-onboarding launch. Does no terminal I/O.
async fn build_tui_app(
    ctx: &Ctx,
    path: &std::path::Path,
    cfg: &config::Config,
) -> Result<tui::state::App> {
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
    let token = config::resolve_token(profile, path, &name);
    let client = NetBoxClient::new(profile, token)?;

    // Probe the instance on connect: confirms reachability + the 4.2 floor, and
    // gives the version for the status line. (CLI commands skip this to stay
    // fast.) A version below the floor is fatal — nothing the user can fix in-app
    // — but a connection/auth failure launches the TUI anyway with a clear,
    // recoverable banner so they can fix the profile (`S`) and reconnect without
    // re-running the binary.
    let (netbox_version, connect_status, connect_severity) =
        tui_startup_status(client.status().await)?;

    // All configured profiles the running session can cycle between without
    // restarting, in config-file (TOML document) order — `profiles` is an
    // order-preserving `IndexMap`, so `P` / `Ctrl+P` walk the file order rather
    // than alphabetical. The switcher (or the palette `profile <name>` verb)
    // reconnects + re-probes each one live; see `tui::state::App::cycle_profile`.
    let profiles: Vec<tui::state::ProfileEntry> = cfg
        .profiles
        .iter()
        .map(|(name, config)| tui::state::ProfileEntry {
            name: name.clone(),
            config: config.clone(),
        })
        .collect();

    // The cache partition keys cached view models to this connection (profile +
    // URL), computed before `name`/`base_url` are moved into the App.
    let cache_partition = crate::cache::profile_partition(&name, &base_url);

    let mut app = tui::state::App::new(
        client,
        &theme_name,
        name,
        base_url,
        netbox_version,
        Some(path.to_path_buf()),
    )
    .with_last_browsed(cfg.ui.last_browsed.clone());
    app.set_profiles(profiles);
    // Seed the startup status: a Success "connected to NetBox vX" cue (footer
    // slot), or — when the probe couldn't reach/authenticate — an actionable
    // Error banner. Onboarding's env-var guidance, set after this returns, takes
    // precedence when it applies.
    app.set_initial_status(connect_status, connect_severity);
    app.set_cache(crate::cache::Cache::from_settings(
        cache_partition,
        &cfg.cache,
    ));
    // Seed the install-appropriate upgrade command the update banner shows
    // (feature-gated; a lean build never checks and shows no banner).
    #[cfg(feature = "updates")]
    {
        app.update_command = crate::update::InstallMethod::detect().update_command();
    }
    // Seed the live UI settings the Settings section edits and the `o` open path
    // reads (auto-refresh interval + custom browser-open command).
    app.set_ui_settings(
        refresh_secs,
        cfg.ui.open_browser_command.clone(),
        cfg.log_level.clone(),
        cfg.log_file.clone(),
    );
    // Honor NO_COLOR: render the TUI monochrome regardless of the configured
    // theme. The TUI is always a TTY when interactive, so the color decision here
    // keys on NO_COLOR (truecolor vs ANSI is moot when no color is emitted). See
    // `tui::term` for the full capability resolver used by other surfaces.
    if tui::term::no_color() {
        app.set_no_color();
    }
    Ok(app)
}

/// `nbox status` — show NetBox connection + version info.
async fn run_status(ctx: &Ctx) -> Result<()> {
    let client = connect(ctx)?;
    let status = client.status().await?;
    let api = client.api_routing().await;
    // The credential preflight is independent of the capability probe, so overlap
    // them — `nbox status` costs no extra serial round-trip for the token verdict.
    // The preflight never errors (a bad token → `Invalid`, an absent endpoint →
    // `Unverified`), so it can't turn a successful status fetch into a failure.
    let (capabilities, auth) =
        tokio::join!(client.capabilities(&status), client.authentication_check(),);
    let url = client.base_url().as_str().to_string();
    let search_line = surface_routing_plain(&api.search);
    let vrf_line = surface_routing_plain(&api.vrf);
    let route_target_line = surface_routing_plain(&api.route_target);
    let token_line = auth.plain();

    let report = serde_json::json!({
        "netbox_url": url,
        "api": api,
        "netbox_version": status.netbox_version,
        "django_version": status.django_version,
        "python_version": status.python_version,
        "capabilities": capabilities,
        "token": auth,
    });

    emit(ctx, &report, || {
        let mut kv = KeyValues::new();
        kv.push("netbox_url", url.clone())
            .push("token", token_line.clone())
            .push("api search", search_line.clone())
            .push("api vrf", vrf_line.clone())
            .push("api route_target", route_target_line.clone())
            .push("netbox_version", status.netbox_version.clone())
            .push_opt("django", status.django_version.clone())
            .push_opt("python", status.python_version.clone())
            .push("rest", "available (canonical)");
        kv.print();
    })
}

/// One `api <surface>` plain-status line: the effective backend, plus the
/// fallback reason in parentheses when a GraphQL preference resolved to REST.
fn surface_routing_plain(surface: &SurfaceRouting) -> String {
    match &surface.reason {
        Some(reason) => format!("{} ({reason})", surface.effective),
        None => surface.effective.clone(),
    }
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
    let outcome = Box::pin(client.search(SearchRequest {
        query: query.to_string(),
        limit,
        filters,
    }))
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
            output::csv::print_streaming(&results, columns.as_deref())?;
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

/// The gate decision for a write command (ADR-0001 §4/§5): a pure function of
/// the write flags + the interactive context, with no I/O. Extracted so the full
/// gate matrix — including the TTY `Prompt` branch and both refusal cases — is
/// unit-testable without a terminal or network.
#[derive(Debug, PartialEq, Eq)]
enum WriteAction {
    /// `--dry-run`: plan + render the diff, send no `PATCH`. Needs neither the
    /// gate nor confirmation.
    DryRun,
    /// `--allow-writes` + `--confirm`: apply without an interactive prompt.
    Apply,
    /// `--allow-writes`, no `--confirm`, plain output with both stdout AND stdin
    /// as TTYs: show the diff, prompt, and apply only on an explicit `y`/`yes`.
    /// (Piped/closed stdin, non-TTY, JSON / CSV, or `--no-tui` never reach this —
    /// they fall to [`WriteAction::RefuseNeedsConfirm`].)
    Prompt,
    /// No `--dry-run` and no `--allow-writes`: the write-enable gate is missing.
    /// Covers `--confirm` without `--allow-writes` too (exit 2, empty stdout).
    RefuseNoGate,
    /// `--allow-writes` without `--confirm` in a non-interactive context: no
    /// prompt is allowed, so apply is refused (exit 2, empty stdout).
    RefuseNeedsConfirm,
}

/// Resolve the write gate + confirmation flags into a [`WriteAction`]. `interactive`
/// is whether the caller can show a plain diff and prompt on a TTY — plain output
/// with both stdout AND stdin as terminals, and not `--no-tui` (a piped/closed
/// stdin cannot answer a prompt). It is computed by the caller so this stays
/// I/O-free and testable.
///
/// The matrix (ADR-0001 §5):
/// - `--dry-run` wins regardless of the other flags (no mutation, no gate).
/// - apply needs BOTH `--allow-writes` (gate) AND confirmation (`--confirm` or a
///   TTY prompt). `--confirm` without the gate is `RefuseNoGate`; the gate without
///   `--confirm` non-interactively is `RefuseNeedsConfirm`; the gate without
///   `--confirm` on a TTY is `Prompt`.
#[allow(clippy::fn_params_excessive_bools)] // the four gate flags; collapsing loses the ADR mapping
#[must_use]
fn write_action(
    dry_run: bool,
    confirm: bool,
    allow_writes: bool,
    interactive: bool,
) -> WriteAction {
    if dry_run {
        return WriteAction::DryRun;
    }
    if !allow_writes {
        return WriteAction::RefuseNoGate;
    }
    if confirm {
        return WriteAction::Apply;
    }
    if interactive {
        WriteAction::Prompt
    } else {
        WriteAction::RefuseNeedsConfirm
    }
}

/// Resolve the write gate + confirmation flags into a [`WriteAction`], refusing
/// (exit 2, empty stdout) before any plan/network when the gate is missing or
/// confirmation can't be obtained non-interactively. Shared by every write
/// command so the gate matrix + refusal messages are one path (ADR-0001 §4/§5).
/// `--dry-run` is exempt (mutates nothing) and always proceeds to plan + render.
fn gate_write(ctx: &Ctx, dry_run: bool, confirm: bool, allow_writes: bool) -> Result<WriteAction> {
    use std::io::IsTerminal as _;
    // `interactive` requires a fully interactive terminal: plain output AND both
    // stdout AND stdin are TTYs (a piped/closed stdin cannot answer a prompt) AND
    // not `--no-tui`. Requiring stdin keeps the "non-TTY never prompts" contract
    // airtight — stdout alone being a TTY is not enough to prompt.
    let interactive = ctx.format == Format::Plain
        && std::io::stdout().is_terminal()
        && std::io::stdin().is_terminal()
        && !ctx.no_tui;
    let action = write_action(dry_run, confirm, allow_writes, interactive);
    match action {
        WriteAction::RefuseNoGate => Err(error::NboxError::Usage(
            "writes are not enabled. To apply, pass `--allow-writes` AND confirm \
             (`--confirm`, or answer the prompt on a TTY). To preview only, pass `--dry-run`."
                .to_string(),
        )
        .into()),
        WriteAction::RefuseNeedsConfirm => Err(error::NboxError::Usage(
            "non-interactive write requires confirmation. Add `--confirm` to apply \
             (or `--dry-run` to preview the plan)."
                .to_string(),
        )
        .into()),
        // DryRun / Apply / Prompt all proceed past the gate to plan + (render | apply).
        _ => Ok(action),
    }
}

/// The shared post-plan write lifecycle (ADR-0001 §5 steps 4–7): given an
/// already-built [`MutationPlan`] and the resolved [`WriteAction`], render a
/// dry-run, prompt on a TTY (when `Prompt`), and apply (when `Apply`/`Prompt`
/// accepted) — emitting the single structured write-audit event for each
/// outcome. The two write commands share this so the gate/prompt/audit
/// orchestration is one path, not two; only the field check + the planner +
/// the applier differ per command. `apply` boxes the command-specific apply
/// future so this stays generic over the target kind.
///
/// `plan` is borrowed for the whole body (the diff render, the audit event,
/// and the apply all read it); nothing moves it.
async fn apply_or_preview(
    ctx: &Ctx,
    client: &NetBoxClient,
    plan: &MutationPlan,
    action: WriteAction,
    profile: &str,
    host: &str,
    apply: impl for<'a> Fn(
        &'a NetBoxClient,
        &'a MutationPlan,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<MutationReceipt>> + 'a>,
    >,
) -> Result<()> {
    use std::io::Write as _;

    use crate::netbox::write_audit::{Outcome, Started, Surface};

    let field_names: Vec<&str> = plan.changed_field_names();
    // Audit the *planned* (normalized) message, not the raw `--message` arg: the
    // planner normalizes an empty/whitespace-only message to `None`, so it audits
    // as absent. The length is a character count (matching NetBox's 200-char
    // limit and the `WriteAuditEvent::message_len` "character length" contract),
    // not a byte length. Only the present-flag + length are recorded — never the
    // message body (ADR-0001 §8).
    let message_present = plan.changelog_message.is_some();
    let message_len = message_audit_len(plan.changelog_message.as_deref());

    // `--dry-run`: plan + render, no mutation, no gate, no confirm. ADR §5.
    if action == WriteAction::DryRun {
        audit_event(
            Surface::Cli,
            profile,
            host,
            plan,
            &field_names,
            Outcome::DryRun,
            "GET",
            &plan.target.endpoint,
            0,
            0,
            message_present,
            message_len,
        )
        .emit();
        if ctx.format == Format::Plain {
            eprintln!("planned, no changes sent");
        }
        return emit(ctx, plan, || render_plan_plain(plan));
    }

    // 5) confirm. On a plain TTY without `--confirm`, show the diff then prompt.
    //    A no-op short-circuits before the prompt — nothing to confirm.
    if action == WriteAction::Prompt {
        if plan.no_op {
            // A no-op sends no PATCH and needs no confirmation. Emit the
            // no-op receipt directly (same as the apply path's short-circuit).
            render_plan_plain(plan);
            eprintln!("\nno change — nothing to apply.");
            let receipt = detail::no_op_receipt(plan);
            audit_event(
                Surface::Cli,
                profile,
                host,
                plan,
                &field_names,
                Outcome::NoOp,
                plan.operation.http_method(),
                &plan.target.endpoint,
                0,
                0, // no network round-trip — short-circuited before apply
                message_present,
                message_len,
            )
            .emit();
            let _ = emit(ctx, &receipt, || render_receipt_plain(&receipt));
            return Ok(());
        }
        // The diff goes to stdout (the operator's review); the prompt to stderr.
        render_plan_plain(plan);
        eprintln!();
        eprint!("Apply this change to NetBox? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let accepted = std::io::stdin().read_line(&mut answer).ok().and_then(|_| {
            let a = answer.trim().to_ascii_lowercase();
            (!a.is_empty() && (a == "y" || a == "yes")).then_some(())
        });
        if accepted.is_none() {
            write_audit::WriteAuditEvent {
                surface: Surface::Cli,
                profile,
                host,
                operation: plan.operation,
                target_kind: &plan.target.kind,
                target_id: plan.target.id,
                target_display: &plan.target.display,
                fields: &field_names,
                outcome: Outcome::NotApplied,
                http_method: "",
                http_path: "",
                status: 0,
                latency_ms: 0,
                request_id: None,
                message_present,
                message_len,
            }
            .emit();
            eprintln!("not applied");
            return Ok(());
        }
    }

    // 6) apply (+ 7 receipt). The apply verifies the plan's token + expiry,
    // short-circuits a no-op, and sends the minimal PATCH with the precondition.
    let sw = Started::now();
    match apply(client, plan).await {
        Ok(receipt) => {
            let outcome = if receipt.no_op {
                Outcome::NoOp
            } else {
                Outcome::Applied
            };
            audit_event(
                Surface::Cli,
                profile,
                host,
                plan,
                &field_names,
                outcome,
                plan.operation.http_method(),
                &plan.target.endpoint,
                receipt.status,
                sw.elapsed_ms(),
                message_present,
                message_len,
            )
            .emit();
            emit(ctx, &receipt, || render_receipt_plain(&receipt))
        }
        Err(e) => {
            // Classify the error for the audit outcome (ADR §8), then return the
            // error so the stable exit code + stderr message reach the process.
            let (outcome, http_status) = classify_apply_error(&e);
            // For the pre-4.6 stale path the refusal happens on the re-read
            // (no PATCH sent); the 4.6+ 412 happens on the PATCH. Either way
            // the attempt targeted the PATCH endpoint. `http_status` carries
            // the real HTTP status when it can be determined (412 for stale,
            // 400 for validation, the Api status for other HTTP failures);
            // 0 when the error has no HTTP status (network, pre-4.6 re-read).
            audit_event(
                Surface::Cli,
                profile,
                host,
                plan,
                &field_names,
                outcome,
                plan.operation.http_method(),
                &plan.target.endpoint,
                http_status,
                sw.elapsed_ms(),
                message_present,
                message_len,
            )
            .emit();
            Err(e)
        }
    }
}

/// `nbox interface <device> <interface> set <field> <value>` — the first safe
/// write (ADR-0001). The lifecycle (ADR-0001 §5): resolve → read → plan →
/// render → confirm → apply → receipt. Enablement + confirmation are separate
/// (ADR §4): apply needs BOTH the `--allow-writes` gate AND confirmation
/// (`--confirm`, or a TTY prompt in plain output); `--dry-run` needs neither.
/// Non-TTY / JSON / CSV / `--no-tui` never prompt. The audit event (field names
/// only, never values/tokens/objects/the message body) is emitted on every
/// planned outcome; a pre-plan usage refusal emits none.
#[allow(clippy::too_many_arguments)] // the CLI flag set; collapsing loses clarity
async fn run_interface_set(
    ctx: &Ctx,
    device: &str,
    interface: &str,
    field: &str,
    value: &str,
    message: Option<&str>,
    dry_run: bool,
    confirm: bool,
    allow_writes: bool,
) -> Result<()> {
    // Field validation is pure input checking — fail closed before any network
    // use so `set status active` is a usage error regardless of the other flags.
    if field != detail::INTERFACE_WRITABLE_FIELD {
        return Err(error::NboxError::Usage(format!(
            "only `{}` is writable on an interface in v1; got \"{field}\". \
             Broader writes land later on the same safe-write contracts (ADR-0001 §6).",
            detail::INTERFACE_WRITABLE_FIELD
        ))
        .into());
    }

    // Gate decision BEFORE any plan/network: a read-only invocation can never
    // become a write (ADR-0001 §4/§5). `--dry-run` is exempt (mutates nothing).
    // A refusal here keeps stdout empty (no diff) and names the required flag.
    // The decision is a pure function of the flags + interactive context, so the
    // full gate matrix is unit-tested in [`write_action`] (no TTY/network).
    let action = gate_write(ctx, dry_run, confirm, allow_writes)?;

    let (client, profile) = connect_named(ctx)?;
    let host = audit_origin(client.base_url());

    // 2–4) read + plan. (1 — resolve — happens inside the planner.)
    let plan = detail::plan_interface_description_update(
        &client, device, interface, value, message, &profile, &not_found,
    )
    .await?;

    // The shared dry-run/prompt/apply/audit lifecycle — one write path for both
    // write commands; only the field check, planner, and applier differ.
    apply_or_preview(ctx, &client, &plan, action, &profile, &host, |c, p| {
        Box::pin(detail::apply_interface_description_update(c, p))
    })
    .await
}

/// `nbox device <device> set <field> <value>` — the second safe write
/// (ADR-0001 follow-on), reusing the same gate/planner/lifecycle/audit path as
/// the interface pilot. Only `status` is writable in this pilot; its allowed
/// values are enumerated live from NetBox (read-only `OPTIONS`) and the
/// operator's input is normalized to the canonical value (a label is accepted
/// case-insensitively when it maps unambiguously to one value) before any
/// `PATCH`. Unknown/ambiguous status is a usage error (exit 2) naming the input
/// and listing the allowed values. No-op status change sends no `PATCH`.
#[allow(clippy::too_many_arguments)] // the CLI flag set; collapsing loses clarity
async fn run_device_set(
    ctx: &Ctx,
    device: &str,
    field: &str,
    value: &str,
    message: Option<&str>,
    dry_run: bool,
    confirm: bool,
    allow_writes: bool,
) -> Result<()> {
    // Field validation is pure input checking — fail closed before any network
    // use so `set <non-status>` is a usage error regardless of the other flags.
    if field != detail::DEVICE_WRITABLE_FIELD {
        return Err(error::NboxError::Usage(format!(
            "only `{}` is writable on a device in this pilot; got \"{field}\". \
             Broader writes land later on the same safe-write contracts (ADR-0001 §6).",
            detail::DEVICE_WRITABLE_FIELD
        ))
        .into());
    }

    // Gate decision BEFORE any plan/network (shared with the interface pilot).
    let action = gate_write(ctx, dry_run, confirm, allow_writes)?;

    let (client, profile) = connect_named(ctx)?;
    let host = audit_origin(client.base_url());

    // The planner enumerates the allowed status values from NetBox (read-only
    // OPTIONS) and normalizes the input before building the plan — an
    // unknown/ambiguous status is a usage error (exit 2) with no `PATCH`.
    let plan =
        detail::plan_device_status_update(&client, device, value, message, &profile, &not_found)
            .await?;

    // The shared dry-run/prompt/apply/audit lifecycle — one write path.
    apply_or_preview(ctx, &client, &plan, action, &profile, &host, |c, p| {
        Box::pin(detail::apply_device_status_update(c, p))
    })
    .await
}

/// `nbox ip reserve <prefix>` — the first Allocate write (ADR-0001 follow-on):
/// reserve the next available IP in a prefix via a POST to its `available-ips`
/// endpoint, on the same gate/lifecycle/audit path as the PATCH pilots. NetBox
/// allocates the address server-side and race-safe (no client precondition), so
/// the receipt carries the created IP object. Only `description` / `dns_name`
/// may be set in v1 (the narrow allow-list — no status/role/tags/assignment).
#[allow(clippy::too_many_arguments)] // the CLI flag set; collapsing loses clarity
async fn run_ip_reserve(
    ctx: &Ctx,
    prefix: &str,
    vrf: Option<&str>,
    description: Option<&str>,
    dns_name: Option<&str>,
    count: u32,
    message: Option<&str>,
    dry_run: bool,
    confirm: bool,
    allow_writes: bool,
) -> Result<()> {
    // Gate decision BEFORE any plan/network (shared with the PATCH pilots): a
    // read-only invocation can never become a write (ADR-0001 §4/§5).
    let action = gate_write(ctx, dry_run, confirm, allow_writes)?;

    let (client, profile) = connect_named(ctx)?;
    let host = audit_origin(client.base_url());

    // 2–4) resolve the prefix + build the Allocate plan. (The read-only candidate
    // GET for the dry-run advisory happens inside the planner.)
    let plan = detail::plan_ip_reserve(
        &client,
        prefix,
        vrf,
        description,
        dns_name,
        count,
        message,
        &profile,
        &not_found,
    )
    .await?;

    // The shared dry-run/prompt/apply/audit lifecycle — one write path.
    apply_or_preview(ctx, &client, &plan, action, &profile, &host, |c, p| {
        Box::pin(detail::apply_ip_reserve(c, p))
    })
    .await
}

/// `nbox prefix reserve <cidr>` — the second Allocate write (ADR-0001
/// follow-on): reserve the next available child prefix via a POST to its
/// `available-prefixes` endpoint, on the same gate/lifecycle/audit path as
/// `ip reserve`. NetBox allocates the block server-side and race-safe (no
/// client precondition), so the receipt carries the created prefix object.
/// Only `description` may be set in v1 (the narrow allow-list).
#[allow(clippy::too_many_arguments)] // the CLI flag set; collapsing loses clarity
async fn run_prefix_reserve(
    ctx: &Ctx,
    prefix: &str,
    vrf: Option<&str>,
    length: Option<u8>,
    description: Option<&str>,
    message: Option<&str>,
    dry_run: bool,
    confirm: bool,
    allow_writes: bool,
) -> Result<()> {
    // Gate decision BEFORE any plan/network (shared with the PATCH pilots): a
    // read-only invocation can never become a write (ADR-0001 §4/§5).
    let action = gate_write(ctx, dry_run, confirm, allow_writes)?;

    let (client, profile) = connect_named(ctx)?;
    let host = audit_origin(client.base_url());

    // 2–4) resolve the parent prefix + build the Allocate plan. (The
    // read-only candidate GET for the dry-run advisory happens inside the
    // planner.)
    let plan = detail::plan_prefix_reserve(
        &client,
        prefix,
        vrf,
        length,
        description,
        message,
        &profile,
        &not_found,
    )
    .await?;

    // The shared dry-run/prompt/apply/audit lifecycle — one write path.
    apply_or_preview(ctx, &client, &plan, action, &profile, &host, |c, p| {
        Box::pin(detail::apply_prefix_reserve(c, p))
    })
    .await
}

/// `nbox ip-range reserve <start|id>` — the third Allocate write (ADR-0001
/// follow-on): reserve the next available IP address within an IP range via a
/// POST to its `available-ips` endpoint, on the same gate/lifecycle/audit path
/// as `ip reserve`. NetBox allocates the address server-side and race-safe (no
/// client precondition), so the receipt carries the created IP object.
/// Only `description` / `dns_name` may be set in v1 (the narrow allow-list).
#[allow(clippy::too_many_arguments)] // the CLI flag set; collapsing loses clarity
async fn run_ip_range_reserve(
    ctx: &Ctx,
    range_ref: &str,
    description: Option<&str>,
    dns_name: Option<&str>,
    count: u32,
    message: Option<&str>,
    dry_run: bool,
    confirm: bool,
    allow_writes: bool,
) -> Result<()> {
    // Gate decision BEFORE any plan/network (shared with the PATCH pilots): a
    // read-only invocation can never become a write (ADR-0001 §4/§5).
    let action = gate_write(ctx, dry_run, confirm, allow_writes)?;

    let (client, profile) = connect_named(ctx)?;
    let host = audit_origin(client.base_url());

    // 2–4) resolve the IP range + build the Allocate plan. (The read-only
    // candidate GET for the dry-run advisory happens inside the planner.)
    let plan = detail::plan_ip_range_reserve(
        &client,
        range_ref,
        description,
        dns_name,
        count,
        message,
        &profile,
        &not_found,
    )
    .await?;

    // The shared dry-run/prompt/apply/audit lifecycle — one write path.
    apply_or_preview(ctx, &client, &plan, action, &profile, &host, |c, p| {
        Box::pin(detail::apply_ip_range_reserve(c, p))
    })
    .await
}

/// `nbox tag add|remove <type> <name> <tag>` — tag writes on the ADR-0001
/// foundation. Resolves the tag and the target object, reads the current tags,
/// and builds a plan that adds or removes the tag's slug from the tags array
/// (a `PATCH` that replaces the whole array — NetBox semantics). A no-op (tag
/// already present for add, already absent for remove) sends no `PATCH`.
#[allow(clippy::too_many_arguments)] // the CLI flag set; collapsing loses clarity
async fn run_tag_write(
    ctx: &Ctx,
    operation: detail::TagOperation,
    object_type: &str,
    object_name: &str,
    tag: &str,
    message: Option<&str>,
    dry_run: bool,
    confirm: bool,
    allow_writes: bool,
) -> Result<()> {
    // Gate decision BEFORE any plan/network (shared with the PATCH pilots): a
    // read-only invocation can never become a write (ADR-0001 §4/§5).
    let action = gate_write(ctx, dry_run, confirm, allow_writes)?;

    let (client, profile) = connect_named(ctx)?;
    let host = audit_origin(client.base_url());

    // 2–4) resolve the tag + target object + build the plan.
    let plan = detail::plan_tag_update(
        &client,
        operation,
        object_type,
        object_name,
        tag,
        message,
        &profile,
        &not_found,
    )
    .await?;

    // The shared dry-run/prompt/apply/audit lifecycle — one write path.
    apply_or_preview(ctx, &client, &plan, action, &profile, &host, |c, p| {
        Box::pin(detail::apply_tag_update(c, p))
    })
    .await
}

/// Classify an apply error into the coarse audit outcome. A stale precondition
/// (412 or pre-4.6 before-hash mismatch) is a recoverable refusal; a 400 is a
/// NetBox validation rejection; anything else (network, auth, 5xx) is a plain
/// error. Walks the chain so a `.context(...)`-wrapped typed error still maps.
fn classify_apply_error(e: &anyhow::Error) -> (write_audit::Outcome, u16) {
    use crate::netbox::write_audit::Outcome;
    // Walk the chain for the most specific typed error. The first NboxError
    // that maps to an outcome wins; its HTTP status (if any) is recorded in
    // the audit so a 400 validation rejection or a 412 stale precondition
    // logs the real status, not the placeholder 0 (ADR-0001 §8, P3 fix).
    for cause in e.chain() {
        if let Some(n) = cause.downcast_ref::<error::NboxError>() {
            let status = n.http_status().unwrap_or(0);
            let outcome = match n {
                error::NboxError::StalePrecondition(_) => Outcome::Stale,
                error::NboxError::WriteValidation(_) => Outcome::Validation,
                _ => Outcome::Error,
            };
            return (outcome, status);
        }
    }
    (Outcome::Error, 0)
}

/// Build a [`WriteAuditEvent`] from a plan's target + the resolved outcome.
/// The NetBox host as it appears in the write audit (ADR-0001 §8): the base
/// URL's origin — scheme + host [+ port] — minus any path/query. Disambiguates
/// `http` vs `https`, non-default ports, and same-host lab instances; empty
/// when the base URL has no host (matching the previous `host_str().unwrap_or("")`
/// fallback). No path is logged — a token could ride a query string.
fn audit_origin(base: &reqwest::Url) -> String {
    if base.host_str().is_some() {
        base.origin().ascii_serialization()
    } else {
        String::new()
    }
}

/// Character length of the planned (normalized) `changelog_message`, for the
/// write audit (ADR-0001 §8) — a char count, matching NetBox's 200-char limit
/// and the `WriteAuditEvent::message_len` "character length" contract, not a
/// byte length. `None` (an empty/normalized-away message) is 0. Extracted so the
/// policy (chars, not bytes) is pinned by a unit test with a non-ASCII message.
#[must_use]
fn message_audit_len(msg: Option<&str>) -> usize {
    msg.map_or(0, |m| m.chars().count())
}

/// Keeps the (many) allow-list fields in one place so the dry-run and apply
/// paths log the same shape.
#[allow(clippy::too_many_arguments)]
fn audit_event<'a>(
    surface: write_audit::Surface,
    profile: &'a str,
    host: &'a str,
    plan: &'a MutationPlan,
    field_names: &'a [&'a str],
    outcome: write_audit::Outcome,
    http_method: &'a str,
    http_path: &'a str,
    status: u16,
    latency_ms: u128,
    message_present: bool,
    message_len: usize,
) -> write_audit::WriteAuditEvent<'a> {
    write_audit::WriteAuditEvent {
        surface,
        profile,
        host,
        operation: plan.operation,
        target_kind: &plan.target.kind,
        target_id: plan.target.id,
        target_display: &plan.target.display,
        fields: field_names,
        outcome,
        http_method,
        http_path,
        status,
        latency_ms,
        request_id: None,
        message_present,
        message_len,
    }
}

/// Plain rendering of a [`MutationPlan`]: the target, the scoped field diff, the
/// precondition in force, and (for dry-run) the no-op note. Goes to stdout as
/// the operator's review; the apply prompt and status lines go to stderr.
fn render_plan_plain(plan: &MutationPlan) {
    println!("{}: {}", plan.target.kind, plan.target.display);
    for f in &plan.fields {
        println!(
            "  {}: {} → {}",
            f.field,
            value_or_empty(&f.before),
            value_or_empty(&f.after)
        );
    }
    let pre = match &plan.precondition {
        crate::netbox::mutation::Precondition::Etag { .. } => "etag",
        crate::netbox::mutation::Precondition::LastUpdated { .. } => "last_updated",
        crate::netbox::mutation::Precondition::None => "none",
    };
    println!("precondition: {pre}");
    for w in &plan.warnings {
        println!("note: {w}");
    }
    if plan.no_op {
        println!("(no change: current value already matches)");
    }
}

/// Plain rendering of a [`MutationReceipt`]: the outcome line + the diff. The
/// outcome line uses the exact ADR-0001 §8 wording (the receipt's `message`).
fn render_receipt_plain(receipt: &MutationReceipt) {
    println!("{}", receipt.message);
    for f in &receipt.fields {
        println!(
            "  {}: {} → {}",
            f.field,
            value_or_empty(&f.before),
            value_or_empty(&f.after)
        );
    }
    // Allocate receipts carry the created object (an `IpView` for a single IP,
    // or a JSON array of `IpView`s for a multi-IP allocation); render its
    // key/values so the operator sees the address(es) NetBox assigned.
    if let Some(serde_json::Value::Object(map)) = &receipt.object {
        for (k, v) in map {
            println!("  {}: {}", k, value_or_empty(v));
        }
    } else if let Some(serde_json::Value::Array(items)) = &receipt.object {
        for (i, item) in items.iter().enumerate() {
            if let serde_json::Value::Object(map) = item {
                println!("  [{i}]:");
                for (k, v) in map {
                    println!("    {}: {}", k, value_or_empty(v));
                }
            }
        }
    }
}

/// Render a JSON `Value` for plain text: a quoted string as its contents, null
/// as `<unset>`, anything else as its JSON form. Keeps the diff readable for both
/// `description` (a string or null) and future non-string fields.
fn value_or_empty(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "<unset>".to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `nbox ip <address>` — resolve an IP (scoped by `--vrf`) and its parent prefix.
async fn run_ip(
    ctx: &Ctx,
    address: Option<&str>,
    vrf: Option<&str>,
    journal: bool,
    journal_limit: Option<usize>,
) -> Result<()> {
    // The read positional is now optional (a subcommand like `reserve` takes its
    // place), so a bare `nbox ip` with neither is a usage error (exit 2).
    let address = address.ok_or_else(|| {
        error::NboxError::Usage(
            "missing IP address. Usage: `nbox ip <address>` to read, or \
             `nbox ip reserve <prefix>` to reserve the next available IP."
                .to_string(),
        )
    })?;
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
        let plain = view.to_plain();
        emit_with_journal(ctx, view, entries, plain)
    } else {
        emit(ctx, &view, || println!("{}", view.to_plain()))
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

/// `nbox rack-group <slug|name|id>` — show a rack group.
async fn run_rack_group(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::rack_group_view_by_ref(&client, value, &not_found).await?;
    emit(ctx, &view, || view.to_key_values().print())
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

/// `nbox virtual-circuit <cid|id>` — show a virtual circuit and its terminations.
async fn run_virtual_circuit(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::virtual_circuit_view_by_ref(&client, value, &not_found).await?;
    emit(ctx, &view, || println!("{}", view.to_plain()))
}

/// `nbox vm <name|id>` — show a virtual machine.
async fn run_vm(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::vm_view_by_ref(&client, value, &not_found).await?;
    emit(ctx, &view, || view.to_key_values().print())
}

/// `nbox vm-type <slug|name|id>` — show a virtual machine type.
async fn run_vm_type(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::vm_type_view_by_ref(&client, value, &not_found).await?;
    emit(ctx, &view, || view.to_key_values().print())
}

/// `nbox cluster <name|id>` — show a cluster.
async fn run_cluster(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let view = detail::cluster_view_by_ref(&client, value, &not_found).await?;
    emit(ctx, &view, || view.to_key_values().print())
}

/// `nbox vrf <name|rd|id>` — show a VRF as a routing context (summary + its
/// prefix tree, addresses, and route targets).
async fn run_vrf(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let detail = detail::vrf_detail_by_ref(&client, value, &not_found).await?;
    emit(ctx, &detail, || println!("{}", detail.to_plain()))
}

async fn run_route_target(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let detail = detail::route_target_detail_by_ref(&client, value, &not_found).await?;
    emit(ctx, &detail, || println!("{}", detail.to_plain()))
}

/// `nbox mac <addr>` — reverse-resolve a MAC to its interface(s)/device(s).
/// The input is normalized first (any common MAC form); a non-MAC is a usage
/// error (exit 2), no match is not-found (4), and >1 interface carrying it is
/// ambiguous (5) — MACs aren't enforced globally unique.
async fn run_mac(ctx: &Ctx, value: &str) -> Result<()> {
    let client = connect(ctx)?;
    let mac = crate::mac::normalize(value).ok_or_else(|| {
        error::NboxError::Usage(format!(
            "invalid MAC address \"{value}\" — try aa:bb:cc:dd:ee:ff"
        ))
    })?;
    let view = detail::mac_view_by_ref(&client, &mac, value, &not_found).await?;
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

    // Honor a configured `open_browser_command` (read best-effort; a missing /
    // unparseable config just falls back to the OS default). The URL is appended
    // as a literal final argument, never shell-interpolated.
    let browser_command = config_browser_command(ctx);
    if let Err(e) = config::open_url(&browser_command, &web_url) {
        eprintln!("warning: could not launch a browser: {e}");
    }
    Ok(())
}

/// The configured `[ui].open_browser_command`, read best-effort from the resolved
/// config path. A missing or unparseable config yields the empty default (the OS
/// default opener). Used by `nbox open`.
fn config_browser_command(ctx: &Ctx) -> String {
    let path = match &ctx.config_path {
        Some(p) => p.clone(),
        None => match config::default_path() {
            Ok(p) => p,
            Err(_) => return String::new(),
        },
    };
    config::load(&path)
        .map(|c| c.ui.open_browser_command)
        .unwrap_or_default()
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
        "rack-group" | "rackgroup" => client.rack_group_by_ref(value).await?.map(|rg| rg.url),
        "vlan" => client.vlan_by_ref(value).await?.map(|v| v.url),
        "prefix" => client.prefix_by_cidr(value).await?.map(|p| p.url),
        "ip" | "ip-address" | "address" => client
            .ip_candidates(value)
            .await?
            .into_iter()
            .next()
            .map(|ip| ip.url),
        "circuit" => client.circuit_by_ref(value).await?.map(|c| c.url),
        "virtual-circuit" | "virtualcircuit" => {
            client.virtual_circuit_by_ref(value).await?.map(|vc| vc.url)
        }
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
        "vm-type" | "vmtype" | "virtual-machine-type" => {
            client.vm_type_by_ref(value).await?.map(|t| t.url)
        }
        "cluster" => client.cluster_by_ref(value).await?.map(|c| c.url),
        "vrf" => client.vrf_by_ref(value).await?.map(|v| v.url),
        "route-target" | "routetarget" => client.route_target_by_ref(value).await?.map(|rt| rt.url),
        // `interface/<device-ref>/<name>`: the device ref is the first segment of
        // `value`, and EVERYTHING after the next `/` is the interface name —
        // taken verbatim, since names contain slashes (e.g. `xe-0/0/1`,
        // `Ethernet1/49`). The shared splitter produces the usage error.
        "interface" => {
            let (device, name) = detail::split_interface_ref(value)?;
            let dev = client
                .device_by_ref(device)
                .await?
                .ok_or_else(|| not_found("device", device))?;
            client.device_interface(dev.id, name).await?.map(|i| i.url)
        }
        "mac" => {
            // Normalize first (a non-MAC is a usage error), then resolve uniquely:
            // several interfaces carrying the MAC is ambiguous (exit 5) — the same
            // contract `nbox mac` honors — not a silent first-pick that could open
            // the wrong object.
            let mac = crate::mac::normalize(value).ok_or_else(|| {
                error::NboxError::Usage(format!(
                    "invalid MAC address \"{value}\" — try aa:bb:cc:dd:ee:ff"
                ))
            })?;
            Some(
                detail::resolve_mac(client, &mac, value, &not_found)
                    .await?
                    .url,
            )
        }
        other => anyhow::bail!(
            "unknown object kind \"{other}\" (expected: device, ip, prefix, vlan, site, rack, rack-group, circuit, virtual-circuit, aggregate, asn, ip-range, tenant, contact, provider, vm, vm-type, cluster, vrf, route-target, interface, mac)"
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
                "object reference must be `<kind>/<ref>` (e.g. device/edge01)\n\nKinds: device, ip, prefix, vlan, site, rack, rack-group, circuit, virtual-circuit, aggregate, asn, ip-range, tenant, contact, provider, vm, vm-type, cluster, vrf, route-target, mac"
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

/// `nbox tagged <tag>` — objects carrying a tag (cross-kind reverse lookup).
async fn run_tagged(ctx: &Ctx, tag: &str, limit: usize) -> Result<()> {
    let client = connect(ctx)?;
    let tag_info = client
        .tag_by_ref(tag)
        .await?
        .ok_or_else(|| not_found("tag", tag))?;
    let objects = client.tagged_objects(tag_info.id, limit).await?;
    let report = TaggedReport {
        tag: ResolvedTag::from_info(tag_info),
        results: objects
            .into_iter()
            .map(TaggedObjectView::from_model)
            .collect(),
    };
    emit(ctx, &report, || {
        if report.results.is_empty() {
            eprintln!("no objects carry tag \"{}\"", report.tag.name);
            return;
        }
        for r in &report.results {
            println!("{:<7} {}", r.kind, r.display);
        }
    })
}

/// `nbox export <action>` — structured read-only exports.
///
/// `prometheus-sd` queries NetBox for IPs in a prefix (or carrying a tag),
/// enriches each with its assigned device's site/role, and emits Prometheus
/// file-SD JSON to stdout. The SD JSON is the only sensible output shape for
/// this subcommand, so it is written directly (not via the plain/JSON/CSV
/// view layer) — `--json`/`--output` are accepted (global flags) but have no
/// effect: the export's format is fixed by its consumer.
async fn run_export(ctx: &Ctx, action: crate::cli::ExportAction) -> Result<()> {
    use crate::cli::ExportAction::{AddressList, DeviceInventory, PrometheusSd};
    match action {
        PrometheusSd {
            prefix,
            tag,
            vrf,
            port,
        } => run_export_prometheus_sd(ctx, prefix, tag, vrf, port).await,
        AddressList {
            prefix,
            tag,
            vrf,
            family,
            summarize,
            format,
        } => run_export_address_list(ctx, prefix, tag, vrf, family, summarize, format).await,
        DeviceInventory {
            site,
            role,
            tag,
            status,
            manufacturer,
            format,
        } => run_export_device_inventory(ctx, site, role, tag, status, manufacturer, format).await,
    }
}

async fn run_export_prometheus_sd(
    ctx: &Ctx,
    prefix: Option<String>,
    tag: Option<String>,
    vrf: Option<String>,
    port: u16,
) -> Result<()> {
    use crate::export::{ExportIp, prometheus_sd, strip_prefix_len};
    use crate::netbox::models::ipam::IpAddress;

    // Exactly one source — `--prefix` xor `--tag`.
    match (prefix.as_deref(), tag.as_deref()) {
        (Some(_), Some(_)) => {
            return Err(error::NboxError::Usage(
                "--prefix and --tag are mutually exclusive — pass one".to_string(),
            )
            .into());
        }
        (None, None) => {
            return Err(error::NboxError::Usage(
                "pass --prefix <cidr> or --tag <slug>".to_string(),
            )
            .into());
        }
        _ => {}
    }

    let client = connect(ctx)?;

    // Gather the source IPs. The prefix path resolves the prefix (so --vrf
    // disambiguation + 404 reuse the read engine) then lists its member IPs,
    // scoped to the resolved prefix's VRF. The tag path resolves the tag (id)
    // then lists IPs carrying it via the IP-addresses `?tag=` filter.
    let ips: Vec<IpAddress> = if let Some(cidr) = prefix.as_deref() {
        let p = detail::resolve_prefix(&client, cidr, vrf.as_deref(), &not_found).await?;
        let vrf_id = p.vrf.as_ref().map(|v| v.id);
        client.prefix_ips(cidr, vrf_id, EXPORT_IP_CAP).await?
    } else {
        let tag_slug = tag.as_deref().unwrap();
        let tag_info = client
            .tag_by_ref(tag_slug)
            .await?
            .ok_or_else(|| not_found("tag", tag_slug))?;
        client
            .list_all(
                crate::netbox::endpoints::Endpoint::IpAddresses,
                vec![("tag", tag_info.slug.clone())],
                EXPORT_IP_CAP,
            )
            .await?
    };

    // Enrich: resolve the distinct assigned devices in one `id__in` fetch so
    // site/role/status come from the device (the IP's `assigned_object`
    // interface brief carries a nested device id but not site/role). IPs
    // without an assigned device keep `device=None` and fall back to per-site
    // grouping in [`prometheus_sd`].
    let device_map = resolve_assigned_devices(&client, &ips).await?;

    let export_ips: Vec<ExportIp> = ips
        .into_iter()
        .map(|ip| {
            let (device_id, device_name) = assigned_device(&ip);
            let (site, role, status, dev_tags) = device_id
                .and_then(|id| device_map.get(&id))
                .map(|d| {
                    let site = d
                        .site
                        .as_ref()
                        .map(crate::netbox::models::common::BriefObject::label);
                    let role = d
                        .role
                        .as_ref()
                        .map(crate::netbox::models::common::BriefObject::label);
                    (
                        site,
                        role,
                        d.status.as_ref().map(|c| c.value.clone()),
                        d.tags.clone(),
                    )
                })
                .unwrap_or((
                    None,
                    None,
                    ip.status.as_ref().map(|c| c.value.clone()),
                    Vec::new(),
                ));
            // Prefer the device's tags when a device resolved; else the IP's own tags.
            let tags = if dev_tags.is_empty() {
                ip.tags.into_iter().map(|t| t.slug).collect::<Vec<_>>()
            } else {
                dev_tags.into_iter().map(|t| t.slug).collect()
            };
            ExportIp {
                address: strip_prefix_len(&ip.address),
                device: device_name,
                site,
                role,
                status,
                tags,
            }
        })
        .collect();

    let groups = prometheus_sd(&export_ips, port);
    // SD JSON is a compact array on stdout — pipe-safe, no envelope.
    let json = serde_json::to_string(&groups).context("serializing Prometheus SD JSON")?;
    println!("{json}");
    Ok(())
}

/// Cap on the number of source IPs an export gathers. Generous — a /24 is
/// 254 addresses — but bounded so a misconfigured `--tag` over a huge table
/// can't stream unbounded work into one export.
const EXPORT_IP_CAP: usize = 5_000;

/// Cap on the number of devices a `device-inventory` export gathers. Generous
/// for a mid-size DCIM, but bounded so an unfiltered export of a huge fleet
/// can't stream unbounded work into one command.
const EXPORT_DEVICE_CAP: usize = 10_000;

/// Resolve the distinct devices an IP set is assigned to, in one
/// `?id__in=…` fetch. Returns an empty map when no IP has an assigned device
/// (or NetBox omits the nested device brief). Stale/missing device ids are
/// tolerated — they simply don't appear, so the IP groups as unassigned.
async fn resolve_assigned_devices(
    client: &NetBoxClient,
    ips: &[crate::netbox::models::ipam::IpAddress],
) -> Result<std::collections::HashMap<u64, crate::netbox::models::dcim::Device>> {
    use std::collections::HashSet;
    let mut ids: HashSet<u64> = HashSet::new();
    for ip in ips {
        if let Some(id) = assigned_device(ip).0 {
            ids.insert(id);
        }
    }
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let id_list = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let devices: Vec<crate::netbox::models::dcim::Device> = client
        .list_all(
            crate::netbox::endpoints::Endpoint::Devices,
            vec![("id__in", id_list)],
            ids.len(),
        )
        .await?;
    Ok(devices.into_iter().map(|d| (d.id, d)).collect())
}

/// Extract the assigned device's `(id, name)` from an IP's polymorphic
/// `assigned_object`. The brief is a `dcim.interface` carrying a nested
/// `device: {id, display, name}`; both keys are optional and permissively
/// read so an unassigned IP (or a non-interface assignment) yields `None`.
fn assigned_device(ip: &crate::netbox::models::ipam::IpAddress) -> (Option<u64>, Option<String>) {
    let dev = ip.assigned_object.as_ref().and_then(|o| o.get("device"));
    let id = dev
        .and_then(|d| d.get("id"))
        .and_then(serde_json::Value::as_u64);
    let name = dev.and_then(|d| {
        d.get("name")
            .and_then(serde_json::Value::as_str)
            .or_else(|| d.get("display").and_then(serde_json::Value::as_str))
    });
    (id, name.map(str::to_string))
}

/// `nbox export address-list` — gather source networks (a prefix's assigned IPs,
/// or the IPs and prefixes carrying a tag), build the de-duplicated/sorted/
/// optionally-summarized list, and emit JSON or newline-delimited CIDRs.
async fn run_export_address_list(
    ctx: &Ctx,
    prefix: Option<String>,
    tag: Option<String>,
    vrf: Option<String>,
    family: Option<u8>,
    summarize: bool,
    format: crate::cli::AddressListFormat,
) -> Result<()> {
    use crate::cli::AddressListFormat;
    use crate::export::{build_address_list, ip_host_net, parse_net};
    use crate::netbox::endpoints::Endpoint;
    use crate::netbox::models::ipam::{IpAddress, Prefix};

    if let Some(f) = family
        && f != 4
        && f != 6
    {
        return Err(error::NboxError::Usage("--family must be 4 or 6".to_string()).into());
    }

    // Exactly one source — `--prefix` xor `--tag` (mirrors prometheus-sd).
    match (prefix.as_deref(), tag.as_deref()) {
        (Some(_), Some(_)) => {
            return Err(error::NboxError::Usage(
                "--prefix and --tag are mutually exclusive — pass one".to_string(),
            )
            .into());
        }
        (None, None) => {
            return Err(error::NboxError::Usage(
                "pass --prefix <cidr> or --tag <slug>".to_string(),
            )
            .into());
        }
        _ => {}
    }

    let client = connect(ctx)?;

    // Gather source networks. The prefix path lists the prefix's assigned IPs as
    // host entries. The tag path takes both IPs (as hosts) and whole prefixes
    // carrying the tag — a tag spans object types, the `netbox-lists` behavior.
    let mut nets: Vec<ipnet::IpNet> = Vec::new();
    if let Some(cidr) = prefix.as_deref() {
        let p = detail::resolve_prefix(&client, cidr, vrf.as_deref(), &not_found).await?;
        let vrf_id = p.vrf.as_ref().map(|v| v.id);
        let ips = client.prefix_ips(cidr, vrf_id, EXPORT_IP_CAP).await?;
        nets.extend(ips.iter().filter_map(|ip| ip_host_net(&ip.address)));
    } else {
        let tag_slug = tag.as_deref().unwrap();
        let tag_info = client
            .tag_by_ref(tag_slug)
            .await?
            .ok_or_else(|| not_found("tag", tag_slug))?;
        let ips: Vec<IpAddress> = client
            .list_all(
                Endpoint::IpAddresses,
                vec![("tag", tag_info.slug.clone())],
                EXPORT_IP_CAP,
            )
            .await?;
        nets.extend(ips.iter().filter_map(|ip| ip_host_net(&ip.address)));
        let prefixes: Vec<Prefix> = client
            .list_all(
                Endpoint::Prefixes,
                vec![("tag", tag_info.slug.clone())],
                EXPORT_IP_CAP,
            )
            .await?;
        nets.extend(prefixes.iter().filter_map(|p| parse_net(&p.prefix)));
    }

    let list = build_address_list(&nets, family, summarize);
    let cidrs: Vec<String> = list.iter().map(ToString::to_string).collect();
    match format {
        // Compact JSON array on one line — pipe-safe, no envelope.
        AddressListFormat::Json => {
            let json = serde_json::to_string(&cidrs).context("serializing address list JSON")?;
            println!("{json}");
        }
        AddressListFormat::Plain => {
            for cidr in &cidrs {
                println!("{cidr}");
            }
        }
    }
    Ok(())
}

/// `nbox export device-inventory` — list devices (filtered by any of
/// site/role/tag/status/manufacturer), project them to inventory records, and
/// emit JSON or CSV.
async fn run_export_device_inventory(
    ctx: &Ctx,
    site: Option<String>,
    role: Option<String>,
    tag: Option<String>,
    status: Option<String>,
    manufacturer: Option<String>,
    format: crate::cli::InventoryFormat,
) -> Result<()> {
    use crate::cli::InventoryFormat;
    use crate::export::{device_inventory, inventory_csv};
    use crate::netbox::endpoints::Endpoint;
    use crate::netbox::models::dcim::Device;

    let client = connect(ctx)?;

    // All filters are optional and ANDed; none → every device (capped).
    let mut filters: Vec<(&str, String)> = Vec::new();
    if let Some(s) = site {
        filters.push(("site", s));
    }
    if let Some(r) = role {
        filters.push(("role", r));
    }
    if let Some(t) = tag {
        filters.push(("tag", t));
    }
    if let Some(st) = status {
        filters.push(("status", st));
    }
    if let Some(m) = manufacturer {
        filters.push(("manufacturer", m));
    }

    let devices: Vec<Device> = client
        .list_all(Endpoint::Devices, filters, EXPORT_DEVICE_CAP)
        .await?;
    let records = device_inventory(&devices);

    match format {
        InventoryFormat::Json => {
            let json = serde_json::to_string(&records).context("serializing inventory JSON")?;
            println!("{json}");
        }
        // `to_csv` already terminates with a newline.
        InventoryFormat::Csv => print!("{}", inventory_csv(&records)?),
    }
    Ok(())
}

/// `nbox journal <kind> <ref>` — recent journal entries for an object.
async fn run_journal(ctx: &Ctx, kind: &str, value: &str, limit: usize) -> Result<()> {
    let client = connect(ctx)?;
    let (content_type, id) = resolve_content_type_id(&client, kind, value).await?;
    let entries = client.journal_entries(content_type, id, limit).await?;

    let view = JournalView::from_models(entries);
    emit(ctx, &view, || println!("{}", view.to_plain()))
}

/// `nbox history <kind> <ref>`: resolve the object, fetch its audit-log entries
/// (system-recorded create/update/delete), and render the timeline. Mirrors
/// [`run_journal`](fn@run_journal) against `/api/core/object-changes/` instead
/// of operator journal notes.
async fn run_history(
    ctx: &Ctx,
    kind: &str,
    value: &str,
    limit: Option<usize>,
    diff: bool,
) -> Result<()> {
    let client = connect(ctx)?;
    let (content_type, id) = resolve_content_type_id(&client, kind, value).await?;
    // `--diff` inspects a single change's full before/after payload, so default
    // to one entry; an explicit `--limit` still wins so an agent can pull several
    // full payloads at once.
    let limit = limit.unwrap_or(if diff { 1 } else { 20 });
    let changes = client.object_changes(content_type, id, limit).await?;

    let view = HistoryView::from_models(changes, diff);
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
        "rack-group" | "rackgroup" => client
            .rack_group_by_ref(value)
            .await?
            .map(|rg| ("dcim.rackgroup", rg.id)),
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
        "virtual-circuit" | "virtualcircuit" => client
            .virtual_circuit_by_ref(value)
            .await?
            .map(|vc| ("circuits.virtualcircuit", vc.id)),
        "vm" => client
            .vm_by_ref(value)
            .await?
            .map(|vm| ("virtualization.virtualmachine", vm.id)),
        "vm-type" | "vmtype" | "virtual-machine-type" => client
            .vm_type_by_ref(value)
            .await?
            .map(|t| ("virtualization.virtualmachinetype", t.id)),
        "cluster" => client
            .cluster_by_ref(value)
            .await?
            .map(|c| ("virtualization.cluster", c.id)),
        "vrf" => client.vrf_by_ref(value).await?.map(|v| ("ipam.vrf", v.id)),
        "route-target" | "routetarget" => client
            .route_target_by_ref(value)
            .await?
            .map(|rt| ("ipam.routetarget", rt.id)),
        "mac" => {
            // Normalize first; a non-MAC is a usage error here too. Resolve
            // uniquely — a duplicate MAC is ambiguous (exit 5), matching `nbox
            // mac`, not a silent first-pick.
            let mac = crate::mac::normalize(value)
                .ok_or_else(|| anyhow::anyhow!("invalid MAC address \"{value}\""))?;
            let m = detail::resolve_mac(client, &mac, value, &not_found).await?;
            Some(("dcim.macaddress", m.id))
        }
        // `interface/<device>/<name>`: interfaces have no single-string ref (they're
        // addressed by device + name, or numeric id), so the journal resolver takes
        // the `device/name` form and resolves the interface id from it. The dotted
        // content type is `dcim.interface`. A not-found device or interface flows
        // through as `None` to the final `ok_or_else`, matching every other kind.
        "interface" => {
            let (device, name) = detail::split_interface_ref(value)?;
            let dev = client.device_by_ref(device).await?;
            let iface = match dev {
                Some(d) => client.device_interface(d.id, name).await?,
                None => None,
            };
            iface.map(|i| ("dcim.interface", i.id))
        }
        other => anyhow::bail!(
            "unknown object kind \"{other}\" (expected: device, ip, prefix, vlan, site, rack, rack-group, circuit, virtual-circuit, aggregate, asn, ip-range, tenant, contact, provider, vm, vm-type, cluster, vrf, route-target, interface, mac)"
        ),
    };
    resolved.ok_or_else(|| not_found(kind, value))
}

/// `nbox raw <method> <path>` — a raw read-only API GET (escape hatch).
async fn run_raw(ctx: &Ctx, method: &str, path: &str) -> Result<()> {
    check_raw_method(method)?;
    let path = normalize_raw_path(path)?;
    let client = connect(ctx)?;
    let value: serde_json::Value = client.get(&path, &[]).await?;
    emit(ctx, &value, || {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    })
}

/// Allow only `GET` for `nbox raw` until write support lands. Write verbs are a
/// deliberate later feature behind the safe-write engine.
fn check_raw_method(method: &str) -> Result<()> {
    if method.eq_ignore_ascii_case("GET") {
        Ok(())
    } else {
        anyhow::bail!(
            "`nbox raw` only supports GET today; raw write verbs are still deferred behind the safe-write engine"
        )
    }
}

/// Normalize a `nbox raw` path so it always targets NetBox's REST API and stays
/// scoped to the configured instance. The API path is accepted with or without
/// the `/api/` prefix — `dcim/devices/?limit=1`, `api/dcim/...`, and
/// `/api/dcim/...` all resolve to the same `/api/...`. Absolute URLs / schemes are
/// rejected: the client joins relative to the profile's base URL, and
/// [`reqwest::Url::join`] with an absolute input *replaces* the base, which would
/// silently send the request to another host.
///
/// Without this, a bare `dcim/devices/` joined onto `https://host/` resolves to
/// the web UI (`https://host/dcim/devices/`), which returns HTML and fails to
/// decode as JSON — the root cause of the confusing "expected value at line N".
fn normalize_raw_path(path: &str) -> Result<String> {
    let path = path.trim();
    // The first path segment must not look like a scheme (`https:`, or a malformed
    // `http:/…`) — `raw` is path-only against the active profile, never an
    // arbitrary URL.
    if path.split('/').next().is_some_and(|seg| seg.contains(':')) {
        anyhow::bail!(
            "`nbox raw` takes a NetBox API path (e.g. `dcim/devices/?limit=1`), not an absolute URL"
        );
    }
    let rel = path.trim_start_matches('/');
    // Prefix `api/` unless the path is already API-rooted (first segment `api`),
    // so both `dcim/...` and `api/dcim/...` hit `/api/...` without doubling it.
    let normalized = match rel.split(['/', '?']).next() {
        Some("api") => rel.to_string(),
        _ => format!("api/{rel}"),
    };
    Ok(normalized)
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

/// The usage error `--no-tui` raises when a launch would otherwise drop into the
/// interactive first-run onboarding wizard. Typed as [`error::NboxError::Usage`]
/// so it exits `2`, and points the user at the non-interactive setup path.
fn no_tui_onboarding_refusal() -> anyhow::Error {
    error::NboxError::Usage(
        "no usable config and --no-tui set: the interactive onboarding wizard is suppressed.\n\n\
         Set up a profile non-interactively first:\n  \
         nbox config init\n  \
         nbox profile add <name> <url> [--token-env <VAR>]\n\
         then export NBOX_TOKEN or set the profile's token_env."
            .to_string(),
    )
    .into()
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
        Cli, CommandFactory, WriteAction, audit_origin, build_mcp_config, check_raw_method, error,
        first_subnet_of_length, init_logging, message_audit_len, no_tui_refusal,
        normalize_raw_path, not_found, parse_object_ref, reserve_vrf_scope,
        resolve_content_type_id, resolve_logging, run_man, tui_startup_status, wants_journal,
        write_action,
    };
    use crate::domain::detail::resolve_unique;
    use crate::netbox::models::ipam::AvailablePrefix;
    use std::path::PathBuf;

    // --- `nbox raw` path normalization -------------------------------------

    /// A minimal `Ctx` for `build_mcp_config` tests — only `profile`/`config_path`
    /// matter to the builder; the rest are inert defaults.
    fn ctx_for(profile: Option<&str>, config: Option<&str>) -> super::Ctx {
        super::Ctx {
            config_path: config.map(std::path::PathBuf::from),
            profile: profile.map(str::to_string),
            format: crate::output::Format::default(),
            json_opts: crate::output::json::JsonOptions::default(),
            no_tui: false,
        }
    }

    #[test]
    fn build_mcp_config_emits_stdio_recipe_with_placeholder_token() {
        let cfg = build_mcp_config(&ctx_for(None, None));
        let nbox = &cfg["mcpServers"]["nbox"];
        // `command` is an absolute path to this binary (resolves in the test run).
        let command = nbox["command"].as_str().expect("command present");
        assert!(command.contains("nbox"), "command names nbox: {command}");
        // `args` always begins with `serve`; no profile/config → just that.
        assert_eq!(nbox["args"].as_array().unwrap()[0].as_str(), Some("serve"));
        assert_eq!(nbox["args"].as_array().unwrap().len(), 1);
        // The token is a placeholder, never a real value.
        assert_eq!(nbox["env"]["NBOX_TOKEN"].as_str(), Some("<set-your-token>"));
    }

    #[test]
    fn build_mcp_config_echoes_profile_and_config_flags() {
        let cfg = build_mcp_config(&ctx_for(Some("work"), Some("/tmp/nb.toml")));
        let args: Vec<String> = nbox_args(&cfg);
        assert_eq!(
            args,
            vec!["serve", "--profile", "work", "--config", "/tmp/nb.toml"]
        );
    }

    #[test]
    fn build_mcp_config_omits_profile_when_unset() {
        // Only `--config` set → no `--profile` pair in args.
        let cfg = build_mcp_config(&ctx_for(None, Some("/tmp/nb.toml")));
        let args: Vec<String> = nbox_args(&cfg);
        assert_eq!(args, vec!["serve", "--config", "/tmp/nb.toml"]);
    }

    fn nbox_args(cfg: &serde_json::Value) -> Vec<String> {
        cfg["mcpServers"]["nbox"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn normalize_raw_path_targets_the_api_for_every_accepted_form() {
        // All three accepted inputs resolve to the same API-rooted path.
        for input in [
            "dcim/devices/?limit=1",
            "api/dcim/devices/?limit=1",
            "/api/dcim/devices/?limit=1",
        ] {
            assert_eq!(
                normalize_raw_path(input).unwrap(),
                "api/dcim/devices/?limit=1",
                "input: {input}"
            );
        }
        // A leading slash without /api/ is still rooted at the API.
        assert_eq!(
            normalize_raw_path("/dcim/sites/").unwrap(),
            "api/dcim/sites/"
        );
        // Already-API paths aren't doubled; surrounding whitespace is trimmed.
        assert_eq!(
            normalize_raw_path("  api/ipam/prefixes/  ").unwrap(),
            "api/ipam/prefixes/"
        );
        // A segment that merely starts with "api" is not treated as API-rooted.
        assert_eq!(
            normalize_raw_path("apiserver/x/").unwrap(),
            "api/apiserver/x/"
        );
        // A query value may contain a colon; only the first path segment is checked.
        assert_eq!(
            normalize_raw_path("dcim/devices/?name=a:b").unwrap(),
            "api/dcim/devices/?name=a:b"
        );
    }

    #[test]
    fn normalize_raw_path_rejects_absolute_urls() {
        for input in [
            "https://evil.example/api/dcim/devices/",
            "http://other-host/api/x",
            "http:/malformed",
        ] {
            assert!(
                normalize_raw_path(input).is_err(),
                "absolute/scheme input must be rejected: {input}"
            );
        }
    }

    // --- TUI startup status (connect probe → version + banner) -------------

    #[test]
    fn tui_startup_status_branches_on_the_probe() {
        use crate::tui::theme::Severity;

        // Connected + compatible → version + a Success "connected" cue.
        let ok: crate::netbox::status::Status =
            serde_json::from_value(serde_json::json!({"netbox-version": "4.5.10"})).unwrap();
        let (ver, msg, sev) = tui_startup_status(Ok(ok)).expect("compatible");
        assert_eq!(ver, "4.5.10");
        assert!(msg.contains("connected to NetBox v4.5.10"), "got: {msg}");
        assert!(matches!(sev, Severity::Success));

        // Reachable but below the 4.2 floor → fatal (Err), not an in-app banner.
        let old: crate::netbox::status::Status =
            serde_json::from_value(serde_json::json!({"netbox-version": "4.1.0"})).unwrap();
        assert!(tui_startup_status(Ok(old)).is_err(), "old version is fatal");

        // Connection/auth failure → recoverable: empty version + an actionable
        // Error banner (the user fixes the profile via S, then reconnects).
        let (ver, msg, sev) =
            tui_startup_status(Err(anyhow::anyhow!("authentication failed (HTTP 401)")))
                .expect("recoverable");
        assert!(ver.is_empty());
        assert!(msg.contains("authentication failed"), "got: {msg}");
        assert!(msg.contains("Press S to edit the profile"), "got: {msg}");
        assert!(matches!(sev, Severity::Error));
    }

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

    /// Walk the clap tree the way `write_subcommand_pages` does, collecting the
    /// expected page basenames (sans `.1`): the top-level `nbox`, every
    /// subcommand (`nbox-device`), and every nested subcommand
    /// (`nbox-config-init`, `nbox-profile-add`, …) — dropping the auto-`help`
    /// command, which we never emit. This is the single source of truth the
    /// parity assertion compares the produced files against.
    fn expected_man_pages(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
        for sub in cmd.get_subcommands() {
            if sub.get_name() == "help" {
                continue;
            }
            let invocation = format!("{prefix} {}", sub.get_name());
            out.push(invocation.replace(' ', "-"));
            if sub.get_subcommands().next().is_some() {
                expected_man_pages(sub, &invocation, out);
            }
        }
    }

    #[test]
    fn man_out_dir_writes_a_page_per_command_with_correct_titles_and_no_dangling_refs() {
        // `nbox man <dir>` must emit the full set the release packages: the
        // top-level `nbox.1` plus one page per (sub)command, named for the full
        // invocation (`nbox-device.1`, `nbox-config.1`, and the nested
        // `nbox-config-init.1`, `nbox-profile-add.1`, …). Anything less leaves a
        // referenced-but-missing page; anything extra leaves a
        // generated-but-unpackaged page. Assert exact parity with the clap tree.
        let dir = tempfile::tempdir().unwrap();
        run_man(Some(dir.path())).unwrap();

        let cmd = Cli::command();
        let bin = cmd.get_name().to_string();

        // The expected basenames: the top-level page + the walked tree.
        let mut expected = vec![bin.clone()];
        expected_man_pages(&cmd, &bin, &mut expected);

        for name in &expected {
            let page = dir.path().join(format!("{name}.1"));
            assert!(page.is_file(), "missing man page {name}.1");
        }

        // No stray pages: exactly the expected set, all `.1`.
        let produced = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(produced, expected.len(), "unexpected man-page count");

        // The nested pages exist (config/profile subcommands), proving there are
        // no dangling SUBCOMMANDS cross-references.
        for nested in [
            "nbox-config-init",
            "nbox-config-path",
            "nbox-config-show",
            "nbox-profile-add",
            "nbox-profile-use",
            "nbox-profile-list",
            "nbox-profile-show",
        ] {
            assert!(
                dir.path().join(format!("{nested}.1")).is_file(),
                "missing nested page {nested}.1"
            );
        }

        // A subcommand page must be titled `nbox-<sub>` but show the real
        // `nbox <sub>` invocation in its SYNOPSIS. The `.TH` title is a raw macro
        // argument (no roff hyphen-escaping); the NAME section escapes hyphens as
        // `nbox\-device`; the SYNOPSIS bolds the program name as `\fBnbox device\fR`.
        let device = std::fs::read_to_string(dir.path().join("nbox-device.1")).unwrap();
        assert!(
            device.contains(".TH nbox-device "),
            "nbox-device.1 title is not `nbox-device`"
        );
        assert!(
            device.contains(r"nbox\-device \-"),
            "nbox-device.1 NAME section is not `nbox-device`"
        );
        assert!(
            device.contains(r"\fBnbox device\fR"),
            "nbox-device.1 SYNOPSIS should read `nbox device …`, not the bare subcommand"
        );

        // The serve flags live only on the per-subcommand page; confirm the
        // generated `nbox-serve.1` carries `--http` (roff-escaped) and the right
        // invocation.
        let serve = std::fs::read_to_string(dir.path().join("nbox-serve.1")).unwrap();
        assert!(
            serve.contains(r"\-\-http"),
            "nbox-serve.1 missing the --http flag"
        );
        assert!(
            serve.contains(r"\fBnbox serve\fR"),
            "nbox-serve.1 SYNOPSIS should read `nbox serve …`"
        );

        // The `config` page's SUBCOMMANDS cross-refs must point at pages we
        // generate (`nbox-config-init(1)`), not the old dangling `config-init(1)`.
        let config = std::fs::read_to_string(dir.path().join("nbox-config.1")).unwrap();
        assert!(
            config.contains(r"nbox\-config\-init(1)"),
            "nbox-config.1 should cross-reference nbox-config-init(1)"
        );
        assert!(
            !config.contains("\nconfig\\-init(1)"),
            "nbox-config.1 still has a dangling `config-init(1)` reference"
        );
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
        assert!(format!("{err:#}").contains("`<device>/<name>`"));
    }

    #[tokio::test]
    async fn resolve_content_type_id_interface_resolves_device_then_name() {
        // The journal resolver's `interface` arm: `device/name` → device id, then
        // interface id, returning the dotted `dcim.interface` content type. The
        // interface name is everything after the first `/`, verbatim (names may
        // contain slashes).
        let server = MockServer::start().await;
        mount_interface(&server, 7, 42, "xe-0/0/1").await;

        let (content_type, id) =
            resolve_content_type_id(&open_client(&server), "interface", "7/xe-0/0/1")
                .await
                .expect("interface resolves");
        assert_eq!(content_type, "dcim.interface");
        assert_eq!(id, 42);
    }

    #[tokio::test]
    async fn resolve_content_type_id_interface_name_may_contain_slashes() {
        // A name WITH a slash (`Ethernet1/49`): the part after the device is the
        // whole name verbatim, not split again.
        let server = MockServer::start().await;
        mount_interface(&server, 7, 49, "Ethernet1/49").await;

        let (_content_type, id) =
            resolve_content_type_id(&open_client(&server), "interface", "7/Ethernet1/49")
                .await
                .expect("interface resolves");
        assert_eq!(id, 49);
    }

    #[tokio::test]
    async fn resolve_content_type_id_interface_missing_name_is_usage_exit_2() {
        // No `/` (or empty name) is a usage error, not a network round-trip.
        let server = MockServer::start().await;
        let err = resolve_content_type_id(&open_client(&server), "interface", "edge01")
            .await
            .unwrap_err();
        assert_eq!(error::NboxError::exit_code_for(&err), 2);
        assert!(format!("{err:#}").contains("`<device>/<name>`"));
    }

    #[tokio::test]
    async fn resolve_content_type_id_interface_not_found_is_exit_4() {
        // Device resolves, interface doesn't → not-found (exit 4), not a 500.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/devices/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 7, "url": format!("{}/api/dcim/devices/7/", server.uri()), "name": "edge01"
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

        let err = resolve_content_type_id(&open_client(&server), "interface", "7/xe-0/0/9")
            .await
            .unwrap_err();
        assert_eq!(error::NboxError::exit_code_for(&err), 4);
    }

    #[tokio::test]
    async fn mac_is_ambiguous_exit_5_on_open_and_journal_not_first_pick() {
        // Parity regression: a MAC on >1 interface is ambiguous (exit 5) on EVERY
        // surface, not just `nbox mac`. `nbox open mac/<addr>` (resolve_object_url)
        // and `nbox journal mac <addr>` (resolve_content_type_id) must NOT silently
        // pick the first match — two candidates → both resolvers exit 5.
        let server = MockServer::start().await;
        let two = json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": format!("{}/api/dcim/mac-addresses/1/", server.uri()),
                 "mac_address": "aa:bb:cc:dd:ee:ff"},
                {"id": 2, "url": format!("{}/api/dcim/mac-addresses/2/", server.uri()),
                 "mac_address": "aa:bb:cc:dd:ee:ff"}
            ]
        });
        Mock::given(method("GET"))
            .and(path("/api/dcim/mac-addresses/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(two))
            .mount(&server)
            .await;
        let c = open_client(&server);

        let open_err = crate::resolve_object_url(&c, "mac", "aa:bb:cc:dd:ee:ff")
            .await
            .unwrap_err();
        assert_eq!(
            error::NboxError::exit_code_for(&open_err),
            5,
            "open mac/<dup> must be ambiguous, not a first-pick"
        );
        let journal_err = resolve_content_type_id(&c, "mac", "aa:bb:cc:dd:ee:ff")
            .await
            .unwrap_err();
        assert_eq!(
            error::NboxError::exit_code_for(&journal_err),
            5,
            "journal mac <dup> must be ambiguous, not a first-pick"
        );
    }

    // --- safe-write gate decision (ADR-0001 §4/§5) -------------------------

    /// The gate matrix as a decision table: `--dry-run` always wins (no gate,
    /// no confirm); apply needs BOTH `--allow-writes` (gate) AND confirmation
    /// (`--confirm` or a TTY prompt). Pure of I/O, so every branch — including
    /// the TTY `Prompt` and both refusal cases — is exercisable here.
    #[test]
    fn write_action_matrix() {
        // `--dry-run` wins regardless of the other flags (mutates nothing).
        for &confirm in &[false, true] {
            for &gate in &[false, true] {
                for &interactive in &[false, true] {
                    assert_eq!(
                        write_action(true, confirm, gate, interactive),
                        WriteAction::DryRun,
                        "dry_run wins: confirm={confirm} gate={gate} tty={interactive}"
                    );
                }
            }
        }
        // No gate + intent to apply → refuse naming the gate (covers `--confirm`
        // without `--allow-writes`, and the bare no-flags case).
        assert_eq!(
            write_action(false, false, false, false),
            WriteAction::RefuseNoGate
        );
        assert_eq!(
            write_action(false, true, false, false),
            WriteAction::RefuseNoGate
        );
        assert_eq!(
            write_action(false, true, false, true),
            WriteAction::RefuseNoGate
        );
        // Gate + `--confirm` → apply (no prompt).
        assert_eq!(write_action(false, true, true, false), WriteAction::Apply);
        assert_eq!(write_action(false, true, true, true), WriteAction::Apply);
        // Gate, no `--confirm`, interactive TTY → prompt.
        assert_eq!(write_action(false, false, true, true), WriteAction::Prompt);
        // Gate, no `--confirm`, non-interactive → refuse naming `--confirm`
        // (no prompt allowed in non-TTY / JSON / CSV / --no-tui).
        assert_eq!(
            write_action(false, false, true, false),
            WriteAction::RefuseNeedsConfirm
        );
    }

    #[test]
    fn reserve_vrf_scope_accepts_either_cli_placement_and_rejects_conflict() {
        assert_eq!(reserve_vrf_scope(Some("blue"), None).unwrap(), Some("blue"));
        assert_eq!(reserve_vrf_scope(None, Some("blue")).unwrap(), Some("blue"));
        assert_eq!(
            reserve_vrf_scope(Some("blue"), Some("BLUE")).unwrap(),
            Some("blue")
        );
        assert!(reserve_vrf_scope(None, None).unwrap().is_none());

        let err = reserve_vrf_scope(Some("blue"), Some("red")).unwrap_err();
        let nbox = err.downcast_ref::<error::NboxError>().expect("usage error");
        assert!(matches!(nbox, error::NboxError::Usage(msg) if msg.contains("conflicting --vrf")));
    }

    // --- write audit host origin (scheme + host [+ port], no path) ---------

    #[test]
    fn audit_origin_includes_scheme_host_and_port_without_path() {
        // scheme + host + non-default port; any path/query is dropped (a token
        // could ride a query string).
        let u = reqwest::Url::parse("https://netbox.example.com:8443/api/dcim/?q=x").unwrap();
        assert_eq!(audit_origin(&u), "https://netbox.example.com:8443");
        // default https port (443) is omitted by the URL origin serialization.
        let u = reqwest::Url::parse("https://netbox.example.com/api/").unwrap();
        assert_eq!(audit_origin(&u), "https://netbox.example.com");
        // http vs https is disambiguated (default port 80 omitted).
        let u = reqwest::Url::parse("http://netbox.example.com/api/").unwrap();
        assert_eq!(audit_origin(&u), "http://netbox.example.com");
        // no host → empty, matching the old host_str().unwrap_or("") fallback.
        let u = reqwest::Url::parse("file:///etc/hosts").unwrap();
        assert_eq!(audit_origin(&u), "");
    }

    // --- write audit message length is a character count, not bytes --------

    #[test]
    fn message_audit_len_counts_chars_not_bytes() {
        // ASCII: char count == byte count.
        assert_eq!(message_audit_len(Some("rotating uplink xe-0/0/1")), 24);
        // Non-ASCII: "ü" is 1 char / 2 bytes — the audit records the char count
        // (matching NetBox's 200-char limit), not the byte length.
        assert_eq!(message_audit_len(Some("hümlaut")), 7);
        assert_eq!("hümlaut".len(), 8); // sanity: bytes differ from chars
        // An emoji is 1 char but 4 bytes — the limit is characters.
        assert_eq!(message_audit_len(Some("a😀b")), 3);
        assert_eq!(message_audit_len(None), 0);
        assert_eq!(message_audit_len(Some("")), 0);
    }
}
