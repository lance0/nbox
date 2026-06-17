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

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Launch the interactive TUI.
    Tui,

    /// Search devices, IPs, prefixes, VLANs, racks, and sites.
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

    /// Show recent journal entries for an object.
    Journal {
        /// Object kind: device, ip, prefix, vlan, site, rack, circuit,
        /// aggregate, asn, or ip-range.
        kind: String,

        /// Object reference (name, address, CIDR, VID, slug, or ID).
        value: String,

        /// Maximum number of entries (newest first).
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },

    /// Make a raw read-only API request (escape hatch for unmodeled endpoints).
    Raw {
        /// HTTP method. Only GET is supported until writes land (v0.2+).
        method: String,

        /// API path, e.g. `/api/dcim/devices/?limit=1`.
        path: String,
    },

    /// Show NetBox connection and version info.
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

    /// Generate a man page (roff) for nbox, e.g. `nbox man > nbox.1`.
    Man,

    /// Run the read-only MCP server over stdio (for AI agents / MCP clients).
    ///
    /// Exposes nbox's lookups as MCP tools, speaking JSON-RPC on stdout.
    Serve,
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
