//! Binary-level tests for `--no-tui` (agent-safety guarantee).
//!
//! `--no-tui` promises non-interactive behavior: an invocation that would
//! otherwise launch the TUI (a bare `nbox`, or an explicit `nbox tui`) must
//! instead refuse with a usage error (exit 2), print an explanation to stderr,
//! leave stdout empty, and — crucially for scripts/agents — never block waiting
//! on a terminal. These drive the *real* compiled binary.
//!
//! No-hang: we redirect stdin to /dev/null and capture stdout/stderr (so there's
//! no TTY at all), and the refusal happens before any TUI/terminal init, so a
//! blocking `wait()` cannot hang. Every command here is network-free.

mod support;

use support::binary::run_nbox;

#[test]
fn no_tui_with_no_subcommand_refuses_with_exit_2() {
    let out = run_nbox(["--no-tui"]);

    assert_eq!(out.code, Some(2), "expected usage exit code 2");
    assert!(
        out.stdout.is_empty(),
        "stdout must stay clean, got: {:?}",
        out.stdout
    );
    assert!(
        out.stderr.contains("--no-tui"),
        "stderr should explain --no-tui, got: {:?}",
        out.stderr
    );
    assert!(
        out.stderr.contains("no command given"),
        "stderr should name the empty invocation, got: {:?}",
        out.stderr
    );
}

#[test]
fn no_tui_with_explicit_tui_subcommand_refuses_with_exit_2() {
    let out = run_nbox(["--no-tui", "tui"]);

    assert_eq!(out.code, Some(2), "expected usage exit code 2");
    assert!(
        out.stdout.is_empty(),
        "stdout must stay clean, got: {:?}",
        out.stdout
    );
    assert!(
        out.stderr.contains("conflicts with the `tui` command"),
        "stderr should name the conflict with `tui`, got: {:?}",
        out.stderr
    );
}

#[test]
fn no_tui_is_a_no_op_on_a_normal_subcommand() {
    // `--no-tui` only guards the TUI-launching paths; an ordinary subcommand is
    // unaffected. With no config/profile, `nbox --no-tui status` fails to connect
    // (it does NOT get the --no-tui usage refusal), proving the flag is inert
    // here. We assert it did NOT exit 2 with the --no-tui message.
    let out = run_nbox([
        "--no-tui",
        "--config",
        "/nonexistent/nbox/config/does-not-exist.toml",
        "status",
    ]);

    // It fails (no usable config), but never launches a TUI and never prints the
    // --no-tui refusal — the flag is a silent no-op on a real subcommand.
    assert_ne!(out.code, Some(0), "status with no config should fail");
    assert!(
        !out.stderr.contains("no command given") && !out.stderr.contains("--no-tui suppresses"),
        "the --no-tui refusal must not fire on a real subcommand, got: {:?}",
        out.stderr
    );
    // stdout stays clean on the error path regardless.
    assert!(
        out.stdout.is_empty(),
        "stdout must stay clean, got: {:?}",
        out.stdout
    );
}
