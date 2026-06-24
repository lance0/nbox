//! Command-line interface definitions.
//!
//! This is the `clap` derive surface for nbox, mirroring the command set in
//! `DESIGN.md` §9. Handlers are wired incrementally; see [`crate::run`].

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// Terminal UI and CLI for NetBox.
#[derive(Debug, Parser)]
#[command(name = "nbox")]
#[command(version)]
#[command(about = "Terminal UI and CLI for NetBox")]
pub struct Cli {
    /// Configuration profile to use (overrides the active profile).
    #[arg(short, long, global = true)]
    pub profile: Option<String>,

    /// Path to an alternate config file.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Emit machine-readable JSON instead of human output.
    #[arg(long, global = true)]
    pub json: bool,

    /// Output format: plain (default), json, or csv. `--json` is a shortcut.
    #[arg(short = 'o', long, global = true, value_name = "FORMAT")]
    pub output: Option<crate::output::Format>,

    /// JSON only: keep only these top-level fields (comma-separated).
    #[arg(long, global = true, value_name = "FIELDS")]
    pub fields: Option<String>,

    /// JSON only: compact output instead of pretty-printed.
    #[arg(long, global = true)]
    pub raw: bool,

    /// JSON only: wrap output as `{schema_version, data}`.
    #[arg(long, global = true)]
    pub envelope: bool,

    /// Never launch the interactive TUI.
    #[arg(long, global = true)]
    pub no_tui: bool,

    /// Logging level (e.g. `info`, `debug`, `nbox=debug`).
    #[arg(long, global = true)]
    pub log_level: Option<String>,

    /// Write logs to this file instead of (only) stderr. stdout stays clean.
    #[arg(long, global = true, value_name = "PATH")]
    pub log_file: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Launch the interactive TUI.
    Tui,

    /// Search devices, sites, racks, rack groups, IPs, prefixes, VLANs, circuits,
    /// virtual circuits, aggregates, ASNs, IP ranges, tenants, contacts, providers,
    /// VMs, VM types, clusters, VRFs, and route targets.
    Search {
        /// Free-text query.
        query: String,

        /// Maximum number of results.
        #[arg(short, long, default_value_t = 25)]
        limit: usize,

        /// Filter by status (e.g. `active`).
        #[arg(long)]
        status: Option<String>,

        /// Filter by site (slug, name, or id). Prefixes are matched on site
        /// scope. Mutually exclusive with --region/--site-group/--location.
        #[arg(long)]
        site: Option<String>,

        /// Filter by region (slug, name, or id). Prefixes are matched on region
        /// scope. Mutually exclusive with --site/--site-group/--location.
        #[arg(long)]
        region: Option<String>,

        /// Filter by site group (slug, name, or id). Prefixes are matched on
        /// site-group scope. Mutually exclusive with --site/--region/--location.
        #[arg(long = "site-group")]
        site_group: Option<String>,

        /// Filter by location (slug, name, or id). Prefixes are matched on
        /// location scope. Mutually exclusive with --site/--region/--site-group.
        #[arg(long)]
        location: Option<String>,

        /// Filter by tenant slug.
        #[arg(long)]
        tenant: Option<String>,

        /// Filter by role slug.
        #[arg(long)]
        role: Option<String>,

        /// Filter by tag slug.
        #[arg(long)]
        tag: Option<String>,

        /// Filter by owner (NetBox 4.5+) — a user, by name (username). Requires a
        /// 4.5+ NetBox; on older releases the filter is silently ignored (those
        /// releases carry no owner data).
        #[arg(long)]
        owner: Option<String>,

        /// Filter by owner group (NetBox 4.5+) — by name. See `--owner`.
        #[arg(long = "owner-group")]
        owner_group: Option<String>,

        /// Filter by VRF (id, RD, or name). Applies to IP and prefix results;
        /// other object kinds carry no VRF and are unaffected.
        #[arg(long)]
        vrf: Option<String>,

        /// Columns to include in CSV output (comma-separated, e.g. kind,display,url).
        #[arg(long)]
        cols: Option<String>,

        /// Accept partial results if some endpoints fail (default: fail closed).
        #[arg(long)]
        partial: bool,
    },

    /// Show a device by name, slug, or ID.
    Device {
        /// Device name, slug, or numeric ID.
        value: String,

        /// Also fetch the object's recent journal entries.
        #[arg(long)]
        journal: bool,

        /// Max inline journal entries to fold in (implies --journal; default 5).
        #[arg(long, value_name = "N")]
        journal_limit: Option<usize>,
    },

    /// Look up an IP address.
    Ip {
        /// IP address, optionally with a mask.
        address: String,

        /// Disambiguate by VRF (name, slug, or RD) when the address exists in several.
        #[arg(long)]
        vrf: Option<String>,

        /// Also fetch the object's recent journal entries.
        #[arg(long)]
        journal: bool,

        /// Max inline journal entries to fold in (implies --journal; default 5).
        #[arg(long, value_name = "N")]
        journal_limit: Option<usize>,
    },

    /// Show prefix details and children.
    Prefix {
        /// Prefix in CIDR notation.
        cidr: String,

        /// Disambiguate by VRF (name, slug, or RD) when the CIDR exists in several.
        #[arg(long)]
        vrf: Option<String>,

        /// Also fetch the object's recent journal entries.
        #[arg(long)]
        journal: bool,

        /// Max inline journal entries to fold in (implies --journal; default 5).
        #[arg(long, value_name = "N")]
        journal_limit: Option<usize>,
    },

    /// Show the next available IP address(es) in a prefix.
    NextIp {
        /// Prefix in CIDR notation.
        prefix: String,

        /// How many available addresses to return.
        #[arg(short, long, default_value_t = 1)]
        count: usize,

        /// Disambiguate the prefix by VRF (name, slug, or RD).
        #[arg(long)]
        vrf: Option<String>,
    },

    /// Show available (free) prefix(es) within a prefix.
    NextPrefix {
        /// Prefix in CIDR notation.
        prefix: String,

        /// Desired new prefix length (e.g. 26): the first free block of that size.
        #[arg(short, long)]
        length: Option<u8>,

        /// Disambiguate the prefix by VRF (name, slug, or RD).
        #[arg(long)]
        vrf: Option<String>,
    },

    /// Show a site.
    Site {
        /// Site name or slug.
        value: String,

        /// Also fetch the object's recent journal entries.
        #[arg(long)]
        journal: bool,

        /// Max inline journal entries to fold in (implies --journal; default 5).
        #[arg(long, value_name = "N")]
        journal_limit: Option<usize>,
    },

    /// Show a rack.
    Rack {
        /// Rack name or numeric ID.
        value: String,

        /// Also fetch the object's recent journal entries.
        #[arg(long)]
        journal: bool,

        /// Max inline journal entries to fold in (implies --journal; default 5).
        #[arg(long, value_name = "N")]
        journal_limit: Option<usize>,
    },

    /// Show a rack group by slug, name, or numeric ID.
    RackGroup {
        /// Rack group slug, name, or numeric ID.
        value: String,
    },

    /// Show a circuit by CID or numeric ID.
    Circuit {
        /// Circuit ID (CID) or numeric ID.
        value: String,

        /// Also fetch the object's recent journal entries.
        #[arg(long)]
        journal: bool,

        /// Max inline journal entries to fold in (implies --journal; default 5).
        #[arg(long, value_name = "N")]
        journal_limit: Option<usize>,
    },

    /// Show a provider by slug, name, or numeric ID.
    Provider {
        /// Provider slug, name, or numeric ID.
        value: String,
    },

    /// Show a virtual circuit by CID or numeric ID (NetBox 4.2+).
    VirtualCircuit {
        /// Virtual circuit CID or numeric ID.
        value: String,
    },

    /// Show an aggregate by CIDR or numeric ID.
    Aggregate {
        /// Aggregate prefix (CIDR) or numeric ID.
        value: String,

        /// Also fetch the object's recent journal entries.
        #[arg(long)]
        journal: bool,

        /// Max inline journal entries to fold in (implies --journal; default 5).
        #[arg(long, value_name = "N")]
        journal_limit: Option<usize>,
    },

    /// Show an ASN by number.
    Asn {
        /// The AS number.
        asn: u32,

        /// Also fetch the object's recent journal entries.
        #[arg(long)]
        journal: bool,

        /// Max inline journal entries to fold in (implies --journal; default 5).
        #[arg(long, value_name = "N")]
        journal_limit: Option<usize>,
    },

    /// Show an IP range by start address or numeric ID.
    IpRange {
        /// Range start address or numeric ID.
        value: String,

        /// Also fetch the object's recent journal entries.
        #[arg(long)]
        journal: bool,

        /// Max inline journal entries to fold in (implies --journal; default 5).
        #[arg(long, value_name = "N")]
        journal_limit: Option<usize>,
    },

    /// Show a tenant by slug, name, or numeric ID.
    Tenant {
        /// Tenant slug, name, or numeric ID.
        value: String,
    },

    /// Show a contact by name or numeric ID.
    Contact {
        /// Contact name or numeric ID.
        value: String,
    },

    /// Show a virtual machine by name or numeric ID.
    Vm {
        /// VM name or numeric ID.
        value: String,
    },

    /// Show a virtual machine type by slug, name, or numeric ID (NetBox 4.6+).
    VmType {
        /// VM type slug, name, or numeric ID.
        value: String,
    },

    /// Show a cluster by name or numeric ID.
    Cluster {
        /// Cluster name or numeric ID.
        value: String,
    },

    /// Show a VRF by name, route distinguisher (RD), or numeric ID.
    Vrf {
        /// VRF name, RD, or numeric ID.
        value: String,
    },

    /// Show a route target (e.g. 65000:100) and the VRFs that import/export it.
    RouteTarget {
        /// Route target name or numeric ID.
        value: String,
    },

    /// Show a MAC address — reverse-resolve it to the interface(s)/device(s)
    /// that carry it (NetBox 4.2+).
    Mac {
        /// The MAC address. Any common form is accepted and normalized
        /// (`aa:bb:cc:dd:ee:ff`, `AABB.CCDD.EEFF`, `aa-bb-…`, `aabbccddeeff`,
        /// a trailing `/48` is stripped).
        value: String,
    },

    /// Show a VLAN by VID or name.
    Vlan {
        /// VLAN VID or name.
        value: String,

        /// Disambiguate by site (name or slug) when a VID exists at several sites.
        #[arg(long)]
        site: Option<String>,

        /// Disambiguate by VLAN group (name or slug) when a VID exists in several.
        #[arg(long)]
        group: Option<String>,

        /// Also fetch the object's recent journal entries.
        #[arg(long)]
        journal: bool,

        /// Max inline journal entries to fold in (implies --journal; default 5).
        #[arg(long, value_name = "N")]
        journal_limit: Option<usize>,
    },

    /// Show an interface on a device.
    Interface {
        /// Device name, slug, or ID.
        device: String,

        /// Interface name.
        interface: String,
    },

    /// Open a NetBox object in the browser.
    Open {
        /// Object reference (e.g. `device/edge01`).
        object_ref: String,
    },

    /// List tags.
    Tags {
        /// Maximum number of tags to list.
        #[arg(short, long, default_value_t = 200)]
        limit: usize,
    },

    /// List objects carrying a tag (NetBox 4.3+ `/api/extras/tagged-objects/`).
    /// A cross-kind reverse lookup: "what has tag X" in one call — unlike
    /// `search --tag`, which needs a `q` and filters per endpoint. The tag is
    /// resolved by id, exact name, or exact slug.
    Tagged {
        /// Tag reference: id, exact name (e.g. `prod:us-east`), or exact slug.
        tag: String,

        /// Maximum number of tagged objects to list.
        #[arg(short, long, default_value_t = 200)]
        limit: usize,
    },

    /// Show recent journal entries for an object.
    Journal {
        /// Object kind: device, ip, prefix, vlan, site, rack, rack-group, circuit,
        /// virtual-circuit, aggregate, asn, ip-range, tenant, contact, provider,
        /// vm, vm-type, cluster, vrf, route-target, mac, or interface
        /// (`<device>/<name>`).
        kind: String,

        /// Object reference (name, address, CIDR, VID, slug, or ID).
        value: String,

        /// Maximum number of entries (newest first).
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },

    /// Make a raw read-only API request (escape hatch for unmodeled endpoints).
    Raw {
        /// HTTP method. Only GET is supported until writes land (a later release).
        method: String,

        /// API path, with or without the `/api/` prefix
        /// (e.g. `dcim/devices/?limit=1` or `/api/dcim/devices/?limit=1`).
        path: String,
    },

    /// Show NetBox connection, versions, backend capabilities, and a
    /// token-validity preflight (NetBox 4.5+).
    Status,

    /// Manage configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Manage profiles.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },

    /// Generate shell completions.
    Completions {
        /// Target shell.
        shell: CompletionShell,
    },

    /// Generate man pages (roff) for nbox.
    ///
    /// With no argument, writes the top-level `nbox.1` to stdout, e.g.
    /// `nbox man > nbox.1`. Given a directory, writes the full set instead:
    /// `nbox.1` plus one `nbox-<subcommand>.1` per subcommand, so `man
    /// nbox-device` works after install.
    Man {
        /// Directory to write the full man-page set into (created if needed).
        /// Omit to write the single top-level page to stdout.
        #[arg(value_name = "OUT_DIR")]
        out_dir: Option<PathBuf>,
    },

    /// Run the read-only MCP server (for AI agents / MCP clients).
    ///
    /// Defaults to the stdio transport: an MCP host launches `nbox serve` as a
    /// subprocess and speaks JSON-RPC over its stdin/stdout. Passing `--http`
    /// switches to a loopback HTTP transport instead (requires the `http`
    /// build feature). Add `--oidc-issuer` + `--audience` to validate inbound
    /// IdP JWTs on `/mcp` and bind a routable interface.
    Serve {
        /// Serve over HTTP on this address instead of stdio, e.g.
        /// `127.0.0.1:8080`. Loopback only unless `--oidc-issuer` is set; a
        /// routable bind requires the OIDC resource-server auth mode and a TLS
        /// terminator in front (reverse proxy).
        #[arg(long, value_name = "ADDR")]
        http: Option<String>,

        /// Require `Authorization: Bearer <TOKEN>` on the HTTP `/mcp` endpoint.
        /// Only meaningful with `--http` (and only in loopback/no-OIDC mode).
        /// Also read from `NBOX_SERVE_TOKEN`.
        #[arg(
            long,
            value_name = "TOKEN",
            env = "NBOX_SERVE_TOKEN",
            hide_env_values = true
        )]
        http_token: Option<String>,

        /// OIDC issuer URL. Enables OAuth 2.1 resource-server mode: inbound IdP
        /// JWTs are validated on `/mcp` and Protected Resource Metadata is
        /// advertised. Requires `--audience`. Only meaningful with `--http`.
        #[arg(long, value_name = "URL")]
        oidc_issuer: Option<String>,

        /// Expected token audience — nbox's canonical resource URI. Required
        /// when `--oidc-issuer` is set; the IdP must mint this `aud` via the
        /// RFC 8707 `resource` parameter.
        #[arg(long, value_name = "VALUE")]
        audience: Option<String>,

        /// JWKS URL override. Default: discovered from the issuer's
        /// `/.well-known/openid-configuration` (then `oauth-authorization-server`).
        #[arg(long, value_name = "URL")]
        oidc_jwks_url: Option<String>,

        /// Extra hostname to accept in the DNS-rebinding allow-list, on top of
        /// the `--audience` host and loopback. Repeatable. Only applies in
        /// OIDC/routable mode (a loopback bind stays loopback-only).
        #[arg(long = "allowed-host", value_name = "HOST")]
        allowed_host: Vec<String>,

        /// Per-caller request cap, in requests per minute, on the HTTP `/mcp`
        /// endpoint. Keyed on the caller (`sub`, else `client_id`, else peer IP).
        /// Over the limit → `429` with `Retry-After`. `0` (the default) disables
        /// it. Only meaningful with `--http`.
        #[arg(long, value_name = "N")]
        rate_limit: Option<u32>,

        /// Print a copy-paste MCP server config (the `mcpServers` JSON object
        /// most hosts read) to stdout and exit, without starting the server or
        /// connecting to NetBox. The `command` is the absolute path to this
        /// binary; `args` echoes any `--profile`/`--config` you passed so the
        /// snippet reproduces your invocation. The token is left as a
        /// placeholder — set it via `nbox config init`, the `NBOX_TOKEN` env
        /// var, or the printed `env` block. For the HTTP/OIDC transport see
        /// docs/MCP.md (this prints the stdio recipe).
        #[arg(long)]
        print_config: bool,
    },
}

/// `nbox config` subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Create a starter config file.
    Init,
    /// Print the resolved config file path.
    Path,
    /// Print the effective configuration.
    Show,
    /// Inspect the API token source for a profile.
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
}

/// `nbox config token` subcommands. The token itself lives in `config.toml`
/// (`token = "…"`) or an env var; this only *reports* its source, never the value.
#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    /// Report the resolved token *source* (token_env / NBOX_TOKEN / config /
    /// none) for the active (or `--profile`) profile. Never prints the token.
    Status,
}

/// `nbox profile` subcommands.
#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// Add a new profile.
    Add {
        /// Profile name.
        name: String,

        /// NetBox base URL.
        url: String,

        /// Environment variable holding the API token.
        #[arg(long)]
        token_env: Option<String>,
    },
    /// Set the active profile.
    Use {
        /// Profile name.
        name: String,
    },
    /// Remove a profile.
    Remove {
        /// Profile name.
        name: String,
    },
    /// List configured profiles.
    List,
    /// Show a profile (defaults to the active one).
    Show {
        /// Profile name.
        name: Option<String>,
    },
}

/// Shells supported by `nbox completions`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    Powershell,
    Elvish,
}

impl CompletionShell {
    /// Map to the corresponding [`clap_complete::Shell`].
    pub fn to_clap(self) -> clap_complete::Shell {
        match self {
            CompletionShell::Bash => clap_complete::Shell::Bash,
            CompletionShell::Zsh => clap_complete::Shell::Zsh,
            CompletionShell::Fish => clap_complete::Shell::Fish,
            CompletionShell::Powershell => clap_complete::Shell::PowerShell,
            CompletionShell::Elvish => clap_complete::Shell::Elvish,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    /// Render the bash completion script in-process (the exact path
    /// `nbox completions bash` takes) and return it as a string.
    fn render_completion(shell: clap_complete::Shell) -> String {
        let mut cmd = Cli::command();
        let bin = cmd.get_name().to_string();
        let mut buf = Vec::new();
        clap_complete::generate(shell, &mut cmd, bin, &mut buf);
        String::from_utf8(buf).expect("completion output is utf-8")
    }

    /// Render the man page (roff) in-process (the exact path `nbox man` takes).
    /// `clap_mangen` renders the top-level page; subcommand flags are reached via
    /// the per-subcommand pages, so this asserts on the bits the top page carries
    /// (global flags + subcommand list) and uses completions for subcommand flags.
    fn render_man() -> String {
        let mut buf = Vec::new();
        clap_mangen::Man::new(Cli::command())
            .render(&mut buf)
            .expect("man render");
        String::from_utf8(buf).expect("man output is utf-8")
    }

    #[test]
    fn bash_completion_includes_all_new_flags() {
        // The serve flags are NOT feature-gated in the clap tree (no `cfg` on the
        // `Serve` variant), so they appear in the default-feature completion
        // regardless of the `http` build feature.
        let bash = render_completion(clap_complete::Shell::Bash);
        for flag in [
            // serve
            "--http",
            "--http-token",
            "--oidc-issuer",
            "--audience",
            "--oidc-jwks-url",
            "--rate-limit",
            // global
            "--log-file",
            // search
            "--vrf",
            "--site",
            "--region",
            "--site-group",
            "--location",
        ] {
            assert!(bash.contains(flag), "bash completion is missing `{flag}`");
        }
    }

    #[test]
    fn zsh_completion_includes_all_new_flags() {
        let zsh = render_completion(clap_complete::Shell::Zsh);
        for flag in [
            "--http",
            "--http-token",
            "--oidc-issuer",
            "--audience",
            "--oidc-jwks-url",
            "--rate-limit",
            "--log-file",
            "--vrf",
            "--site",
            "--region",
            "--site-group",
            "--location",
        ] {
            assert!(zsh.contains(flag), "zsh completion is missing `{flag}`");
        }
    }

    #[test]
    fn man_page_includes_global_flags_and_subcommands() {
        // The top-level man page carries the global flags and the subcommand
        // list. Per-subcommand flags (serve/search) live on their own pages, so
        // those are covered by the completion tests above; here we assert the
        // global `--log-file` and that `serve`/`search` are advertised.
        //
        // roff escapes hyphens as `\-`, so the flag renders as `\-\-log\-file`.
        let man = render_man();
        assert!(
            man.contains(r"\-\-log\-file"),
            "man page missing --log-file (roff-escaped)"
        );
        assert!(
            man.contains("serve"),
            "man page missing the serve subcommand"
        );
        assert!(
            man.contains("search"),
            "man page missing the search subcommand"
        );
    }

    #[test]
    fn per_subcommand_man_pages_include_their_flags() {
        // The serve/search flags live on the per-subcommand man pages (clap_mangen
        // renders one page per command). Render those pages directly and assert
        // each new flag is present (roff-escaped), proving the man surface covers
        // them even though they don't appear on the top-level page.
        let cmd = Cli::command();
        let render_sub = |name: &str| -> String {
            let sub = cmd
                .get_subcommands()
                .find(|c| c.get_name() == name)
                .unwrap_or_else(|| panic!("subcommand `{name}` not found"))
                .clone();
            let mut buf = Vec::new();
            clap_mangen::Man::new(sub)
                .render(&mut buf)
                .expect("man render");
            String::from_utf8(buf).expect("man output is utf-8")
        };

        let serve = render_sub("serve");
        for flag in [
            r"\-\-http",
            r"\-\-http\-token",
            r"\-\-oidc\-issuer",
            r"\-\-audience",
            r"\-\-oidc\-jwks\-url",
            r"\-\-rate\-limit",
            r"\-\-print\-config",
        ] {
            assert!(serve.contains(flag), "serve man page missing `{flag}`");
        }

        let search = render_sub("search");
        for flag in [
            r"\-\-vrf",
            r"\-\-site",
            r"\-\-region",
            r"\-\-site\-group",
            r"\-\-location",
        ] {
            assert!(search.contains(flag), "search man page missing `{flag}`");
        }
    }

    #[test]
    fn parses_global_flag_and_subcommand() {
        let cli = Cli::try_parse_from(["nbox", "--json", "device", "edge01"]).unwrap();
        assert!(cli.json);
        assert!(matches!(cli.command, Some(Command::Device { value, .. }) if value == "edge01"));
    }

    #[test]
    fn search_parses_scope_filters() {
        let cli = Cli::try_parse_from([
            "nbox",
            "search",
            "10.0",
            "--region",
            "us-east",
            "--site-group",
            "campus",
            "--location",
            "row-a",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Search {
                region: Some(r),
                site_group: Some(g),
                location: Some(l),
                ..
            }) if r == "us-east" && g == "campus" && l == "row-a"
        ));
    }

    #[test]
    fn search_parses_vrf_filter() {
        let cli = Cli::try_parse_from(["nbox", "search", "10.0", "--vrf", "blue"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Search { vrf: Some(v), .. }) if v == "blue"
        ));
    }

    #[test]
    fn search_limit_defaults_to_25() {
        let cli = Cli::try_parse_from(["nbox", "search", "edge"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Search { limit: 25, .. })
        ));
    }

    #[test]
    fn journal_flag_defaults_off_and_parses() {
        let off = Cli::try_parse_from(["nbox", "device", "edge01"]).unwrap();
        assert!(matches!(
            off.command,
            Some(Command::Device { journal: false, .. })
        ));
        let on = Cli::try_parse_from(["nbox", "device", "edge01", "--journal"]).unwrap();
        assert!(matches!(
            on.command,
            Some(Command::Device { journal: true, .. })
        ));
        // The flag is also accepted on the other wired detail commands.
        let site = Cli::try_parse_from(["nbox", "site", "iad1", "--journal"]).unwrap();
        assert!(matches!(
            site.command,
            Some(Command::Site { journal: true, .. })
        ));
    }

    #[test]
    fn serve_defaults_to_stdio_and_parses_http_flags() {
        // Bare `serve` → stdio (no http address, no token, no OIDC, no limit).
        let stdio = Cli::try_parse_from(["nbox", "serve"]).unwrap();
        assert!(matches!(
            stdio.command,
            Some(Command::Serve {
                http: None,
                http_token: None,
                oidc_issuer: None,
                audience: None,
                oidc_jwks_url: None,
                ref allowed_host,
                rate_limit: None,
                print_config: false,
            }) if allowed_host.is_empty()
        ));
        // `--http` (and the optional `--http-token`) parse onto the variant.
        let http = Cli::try_parse_from([
            "nbox",
            "serve",
            "--http",
            "127.0.0.1:8080",
            "--http-token",
            "abc123",
        ])
        .unwrap();
        assert!(matches!(
            http.command,
            Some(Command::Serve { http: Some(a), http_token: Some(t), print_config: false, .. })
                if a == "127.0.0.1:8080" && t == "abc123"
        ));
    }

    #[test]
    fn serve_print_config_flag_parses_and_short_circuits() {
        // `--print-config` sets the boolean; it coexists with the stdio defaults
        // (no `--http`). The handler short-circuits before connecting, so this
        // is the only signal needed to drive it.
        let pc = Cli::try_parse_from(["nbox", "serve", "--print-config"]).unwrap();
        assert!(matches!(
            pc.command,
            Some(Command::Serve {
                print_config: true,
                http: None,
                ..
            })
        ));
        // It composes with `--profile`/`--config` (echoed into the printed args).
        let with_profile =
            Cli::try_parse_from(["nbox", "--profile", "work", "serve", "--print-config"]).unwrap();
        assert!(matches!(
            with_profile.command,
            Some(Command::Serve {
                print_config: true,
                ..
            })
        ));
        assert_eq!(with_profile.profile.as_deref(), Some("work"));
    }

    #[test]
    fn serve_parses_oidc_resource_server_flags() {
        let oidc = Cli::try_parse_from([
            "nbox",
            "serve",
            "--http",
            "0.0.0.0:8080",
            "--oidc-issuer",
            "https://idp.example.com",
            "--audience",
            "https://nbox.example.com",
            "--oidc-jwks-url",
            "https://idp.example.com/keys",
        ])
        .unwrap();
        assert!(matches!(
            oidc.command,
            Some(Command::Serve {
                http: Some(addr),
                oidc_issuer: Some(iss),
                audience: Some(aud),
                oidc_jwks_url: Some(jwks),
                ..
            }) if addr == "0.0.0.0:8080"
                && iss == "https://idp.example.com"
                && aud == "https://nbox.example.com"
                && jwks == "https://idp.example.com/keys"
        ));
    }

    #[test]
    fn serve_parses_repeatable_allowed_host_flag() {
        let parsed = Cli::try_parse_from([
            "nbox",
            "serve",
            "--http",
            "0.0.0.0:8080",
            "--oidc-issuer",
            "https://idp.example.com",
            "--audience",
            "https://nbox.example.com",
            "--allowed-host",
            "nbox.example.com",
            "--allowed-host",
            "alt.example.com",
        ])
        .unwrap();
        let Some(Command::Serve { allowed_host, .. }) = parsed.command else {
            panic!("expected a serve command");
        };
        assert_eq!(allowed_host, vec!["nbox.example.com", "alt.example.com"]);

        // Absent → empty (loopback-only allow-list by default).
        let none = Cli::try_parse_from(["nbox", "serve", "--http", "127.0.0.1:8080"]).unwrap();
        assert!(matches!(
            none.command,
            Some(Command::Serve { ref allowed_host, .. }) if allowed_host.is_empty()
        ));
    }

    #[test]
    fn serve_parses_rate_limit_flag() {
        let rl = Cli::try_parse_from([
            "nbox",
            "serve",
            "--http",
            "127.0.0.1:8080",
            "--rate-limit",
            "120",
        ])
        .unwrap();
        assert!(matches!(
            rl.command,
            Some(Command::Serve {
                rate_limit: Some(120),
                ..
            })
        ));
        // Absent → None (disabled by default).
        let none = Cli::try_parse_from(["nbox", "serve", "--http", "127.0.0.1:8080"]).unwrap();
        assert!(matches!(
            none.command,
            Some(Command::Serve {
                rate_limit: None,
                ..
            })
        ));
    }

    #[test]
    fn parses_tenant_and_contact_lookups() {
        let tenant = Cli::try_parse_from(["nbox", "tenant", "acme"]).unwrap();
        assert!(matches!(
            tenant.command,
            Some(Command::Tenant { value }) if value == "acme"
        ));
        let contact = Cli::try_parse_from(["nbox", "contact", "Jane Doe"]).unwrap();
        assert!(matches!(
            contact.command,
            Some(Command::Contact { value }) if value == "Jane Doe"
        ));
    }

    #[test]
    fn parses_vm_and_cluster_lookups() {
        let vm = Cli::try_parse_from(["nbox", "vm", "web-01"]).unwrap();
        assert!(matches!(
            vm.command,
            Some(Command::Vm { value }) if value == "web-01"
        ));
        let cluster = Cli::try_parse_from(["nbox", "cluster", "prod"]).unwrap();
        assert!(matches!(
            cluster.command,
            Some(Command::Cluster { value }) if value == "prod"
        ));
    }

    #[test]
    fn parses_rack_group_lookup() {
        let rg = Cli::try_parse_from(["nbox", "rack-group", "row-a"]).unwrap();
        assert!(matches!(
            rg.command,
            Some(Command::RackGroup { value }) if value == "row-a"
        ));
    }

    #[test]
    fn parses_vm_type_lookup() {
        let t = Cli::try_parse_from(["nbox", "vm-type", "web-tier"]).unwrap();
        assert!(matches!(
            t.command,
            Some(Command::VmType { value }) if value == "web-tier"
        ));
    }

    #[test]
    fn parses_provider_lookup() {
        let provider = Cli::try_parse_from(["nbox", "provider", "acme-telecom"]).unwrap();
        assert!(matches!(
            provider.command,
            Some(Command::Provider { value }) if value == "acme-telecom"
        ));
    }

    #[test]
    fn parses_virtual_circuit_lookup() {
        let vc = Cli::try_parse_from(["nbox", "virtual-circuit", "VC-100"]).unwrap();
        assert!(matches!(
            vc.command,
            Some(Command::VirtualCircuit { value }) if value == "VC-100"
        ));
    }

    #[test]
    fn journal_flag_wired_on_aggregate_asn_and_ip_range() {
        let agg = Cli::try_parse_from(["nbox", "aggregate", "10.0.0.0/8", "--journal"]).unwrap();
        assert!(matches!(
            agg.command,
            Some(Command::Aggregate { journal: true, .. })
        ));
        let asn = Cli::try_parse_from(["nbox", "asn", "64512", "--journal"]).unwrap();
        assert!(matches!(
            asn.command,
            Some(Command::Asn { journal: true, .. })
        ));
        let range = Cli::try_parse_from(["nbox", "ip-range", "10.0.0.10", "--journal"]).unwrap();
        assert!(matches!(
            range.command,
            Some(Command::IpRange { journal: true, .. })
        ));
        // Defaults off when the flag is absent.
        let bare = Cli::try_parse_from(["nbox", "aggregate", "10.0.0.0/8"]).unwrap();
        assert!(matches!(
            bare.command,
            Some(Command::Aggregate { journal: false, .. })
        ));
    }
}
