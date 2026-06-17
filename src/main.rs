//! nbox binary entry point.
//!
//! Parses the CLI and dispatches into [`nbox::run`]. Command handlers are built
//! out across Phase 1–3 (see `ROADMAP.md`).

#![warn(clippy::pedantic)]

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

    // Resolve logging (file + level) from flags > config > env > default, then
    // initialize. The non-blocking file writer returns a WorkerGuard that MUST
    // outlive every log call: it's bound here (`_log_guard`) so it lives for the
    // whole of `run` and flushes on the way out — drop it early and buffered
    // lines are lost. Logs never touch stdout (kept clean for `--json`/`serve`).
    let log_cfg = nbox::config::load_logging(cli.config.as_deref());
    let choice = nbox::resolve_logging(
        cli.log_file.as_deref().and_then(std::path::Path::to_str),
        log_cfg.log_file.as_deref(),
        cli.log_level.as_deref(),
        log_cfg.log_level.as_deref(),
        std::env::var("NBOX_LOG").ok().as_deref(),
        std::env::var("RUST_LOG").ok().as_deref(),
    );
    let log_guard = nbox::init_logging(&choice);

    // Kick off the update check (if enabled) before doing work, then report it
    // after, so a quick command isn't delayed by the network round-trip.
    #[cfg(feature = "updates")]
    let update = {
        let json = cli.json;
        (nbox::update::spawn_check(), json)
    };

    // The command dispatch in `run` is a large match over every subcommand;
    // box the future to keep it off the stack (clippy::large_futures).
    let result = Box::pin(nbox::run(cli)).await;

    #[cfg(feature = "updates")]
    {
        let (rx, json) = update;
        nbox::update::maybe_print_notice(rx, json);
    }

    if let Err(err) = result {
        // `std::process::exit` skips destructors, so the WorkerGuard would never
        // flush the file appender on these paths. Drop it explicitly first to
        // flush + join the writer before the process is torn down.
        drop(log_guard);
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
    // Success path: `main` returning drops `log_guard` here, flushing the file
    // appender. Bind it so it isn't dropped before `run` completes.
    drop(log_guard);
}
