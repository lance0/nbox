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
    },

    /// Show a device by name, slug, or ID.
    Device {
        /// Device name, slug, or numeric ID.
        value: String,
    },

    /// Look up an IP address.
    Ip {
        /// IP address, optionally with a mask.
        address: String,
    },

    /// Show prefix details and children.
    Prefix {
        /// Prefix in CIDR notation.
        cidr: String,
    },

    /// Show a site.
    Site {
        /// Site name or slug.
        value: String,
    },

    /// Show a rack.
    Rack {
        /// Rack name or numeric ID.
        value: String,
    },

    /// Show a VLAN by VID or name.
    Vlan {
        /// VLAN VID or name.
        value: String,
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
        assert!(matches!(cli.command, Some(Command::Device { value }) if value == "edge01"));
    }

    #[test]
    fn search_limit_defaults_to_25() {
        let cli = Cli::try_parse_from(["nbox", "search", "edge"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Search { limit: 25, .. })
        ));
    }
}
