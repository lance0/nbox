//! nbx — terminal UI and CLI for NetBox.
//!
//! This is the library crate root. Modules (`cli`, `config`, `netbox`, `domain`,
//! `tui`, `output`, …) are introduced as the implementation progresses; see
//! `DESIGN.md` and `ROADMAP.md` for the intended structure and phasing.

use clap::CommandFactory;

use crate::cli::{Cli, Command};

pub mod cli;

/// The crate version, sourced from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Dispatch a parsed [`Cli`] invocation.
///
/// Most subcommands are still stubs at this phase; they report that they are
/// not yet implemented on stderr so that stdout stays clean for piping.
pub fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        None | Some(Command::Tui) => not_implemented("interactive TUI"),
        Some(Command::Search { .. }) => not_implemented("search"),
        Some(Command::Device { .. }) => not_implemented("device lookup"),
        Some(Command::Ip { .. }) => not_implemented("IP lookup"),
        Some(Command::Prefix { .. }) => not_implemented("prefix lookup"),
        Some(Command::Site { .. }) => not_implemented("site lookup"),
        Some(Command::Rack { .. }) => not_implemented("rack lookup"),
        Some(Command::Vlan { .. }) => not_implemented("VLAN lookup"),
        Some(Command::Interface { .. }) => not_implemented("interface lookup"),
        Some(Command::Open { .. }) => not_implemented("open in browser"),
        Some(Command::Config { .. }) => not_implemented("config management"),
        Some(Command::Profile { .. }) => not_implemented("profile management"),
        Some(Command::Completions { shell }) => {
            let mut cmd = Cli::command();
            let bin = cmd.get_name().to_string();
            clap_complete::generate(shell.to_clap(), &mut cmd, bin, &mut std::io::stdout());
            Ok(())
        }
    }
}

/// Report an unimplemented command on stderr without dirtying stdout.
fn not_implemented(what: &str) -> anyhow::Result<()> {
    eprintln!("nbx: {what} is not yet implemented");
    Ok(())
}
