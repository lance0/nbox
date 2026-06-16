//! nbx binary entry point.
//!
//! Parses the CLI and dispatches into [`nbx::run`]. Command handlers are built
//! out across Phase 1–3 (see `ROADMAP.md`).

use clap::Parser;
use nbx::cli::Cli;

fn main() {
    let cli = Cli::parse();
    if let Err(err) = nbx::run(cli) {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}
