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

use std::process::{Command, Stdio};

/// Run `nbox <args>` with no TTY (stdin from null, piped stdout/stderr) and
/// return `(exit_code, stdout, stderr)`. `wait_with_output` reads to EOF and
/// reaps the child, so it cannot hang once the process exits.
fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_nbox"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn nbox");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn no_tui_with_no_subcommand_refuses_with_exit_2() {
    let (code, stdout, stderr) = run(&["--no-tui"]);

    assert_eq!(code, Some(2), "expected usage exit code 2");
    assert!(stdout.is_empty(), "stdout must stay clean, got: {stdout:?}");
    assert!(
        stderr.contains("--no-tui"),
        "stderr should explain --no-tui, got: {stderr:?}"
    );
    assert!(
        stderr.contains("no command given"),
        "stderr should name the empty invocation, got: {stderr:?}"
    );
}

#[test]
fn no_tui_with_explicit_tui_subcommand_refuses_with_exit_2() {
    let (code, stdout, stderr) = run(&["--no-tui", "tui"]);

    assert_eq!(code, Some(2), "expected usage exit code 2");
    assert!(stdout.is_empty(), "stdout must stay clean, got: {stdout:?}");
    assert!(
        stderr.contains("conflicts with the `tui` command"),
        "stderr should name the conflict with `tui`, got: {stderr:?}"
    );
}

#[test]
fn no_tui_is_a_no_op_on_a_normal_subcommand() {
    // `--no-tui` only guards the TUI-launching paths; an ordinary subcommand is
    // unaffected. With no config/profile, `nbox --no-tui status` fails to connect
    // (it does NOT get the --no-tui usage refusal), proving the flag is inert
    // here. We assert it did NOT exit 2 with the --no-tui message.
    let (code, stdout, stderr) = run(&[
        "--no-tui",
        "--config",
        "/nonexistent/nbox/config/does-not-exist.toml",
        "status",
    ]);

    // It fails (no usable config), but never launches a TUI and never prints the
    // --no-tui refusal — the flag is a silent no-op on a real subcommand.
    assert_ne!(code, Some(0), "status with no config should fail");
    assert!(
        !stderr.contains("no command given") && !stderr.contains("--no-tui suppresses"),
        "the --no-tui refusal must not fire on a real subcommand, got: {stderr:?}"
    );
    // stdout stays clean on the error path regardless.
    assert!(stdout.is_empty(), "stdout must stay clean, got: {stdout:?}");
}
