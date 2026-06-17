//! nbox binary entry point.
//!
//! Parses the CLI and dispatches into [`nbox::run`]. Command handlers are built
//! out across Phase 1–3 (see `ROADMAP.md`).

use clap::Parser;
use nbox::cli::Cli;

#[tokio::main]
async fn main() {
    // Restore the default SIGPIPE disposition (SIG_DFL) before any output. Rust
    // ignores SIGPIPE by default, which turns a closed stdout reader (e.g.
    // `nbox completions bash | head`) into an EPIPE write error and then a panic
    // ("failed printing to stdout: Broken pipe"). Resetting to SIG_DFL makes the
    // OS terminate the process normally on a broken pipe, exactly like standard
    // Unix tools.
    #[cfg(unix)]
    // SAFETY: called once at the very start of `main`, before any threads are
    // spawned or any I/O occurs; `signal(2)` with SIG_DFL on SIGPIPE is the
    // documented way to opt back into default broken-pipe handling.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let cli = Cli::parse();
    nbox::init_logging(cli.log_level.as_deref());

    // Kick off the update check (if enabled) before doing work, then report it
    // after, so a quick command isn't delayed by the network round-trip.
    #[cfg(feature = "updates")]
    let update = {
        let json = cli.json;
        (nbox::update::spawn_check(), json)
    };

    let result = nbox::run(cli).await;

    #[cfg(feature = "updates")]
    {
        let (rx, json) = update;
        nbox::update::maybe_print_notice(rx, json);
    }

    if let Err(err) = result {
        // A broken stdout pipe that surfaced as an error (rather than being
        // handled by the SIGPIPE reset above — e.g. on a non-Unix platform)
        // exits quietly with the conventional 141 (128 + SIGPIPE), never
        // printing an error to stderr. Only BrokenPipe takes this path; all
        // other IO and non-IO errors keep their normal, noisy reporting.
        if nbox::error::is_broken_pipe(&err) {
            std::process::exit(141);
        }
        eprintln!("error: {err:#}");
        std::process::exit(nbox::error::NboxError::exit_code_for(&err));
    }
}
