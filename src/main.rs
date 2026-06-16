//! nbx binary entry point.
//!
//! Parses the CLI and dispatches into [`nbx::run`]. Command handlers are built
//! out across Phase 1–3 (see `ROADMAP.md`).

use clap::Parser;
use nbx::cli::Cli;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    nbx::init_logging(cli.log_level.as_deref());

    // Kick off the update check (if enabled) before doing work, then report it
    // after, so a quick command isn't delayed by the network round-trip.
    #[cfg(feature = "updates")]
    let update = {
        let json = cli.json;
        (nbx::update::spawn_check(), json)
    };

    let result = nbx::run(cli).await;

    #[cfg(feature = "updates")]
    {
        let (rx, json) = update;
        nbx::update::maybe_print_notice(rx, json);
    }

    if let Err(err) = result {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}
