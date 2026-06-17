//! Regression test for the closed-stdout-pipe panic (`nbox … | head`).
//!
//! Rust ignores SIGPIPE by default, so when stdout is a pipe whose reader closes
//! early, the next write fails with EPIPE and Rust panics
//! ("failed printing to stdout: Broken pipe", exit code 101). `main` resets the
//! SIGPIPE disposition to SIG_DFL so a broken pipe terminates the process
//! normally instead.
//!
//! This launches the *real* compiled binary on a network-free, stdout-heavy
//! command (`completions bash`), reads only the first line, then drops the read
//! end of the pipe. The child keeps writing into a now-closed pipe and must
//! exit *without panicking* (status != 101), ideally with the conventional
//! broken-pipe status (signalled by SIGPIPE, or exit code 141 on platforms that
//! don't reset the disposition).
//!
//! Determinism / no-hang: we never sleep-as-sync. We read exactly one line off
//! the child's stdout, drop the reader to close the pipe, then `wait()` on the
//! child. `completions bash` emits far more than one line, so by the time we
//! drop the reader the child still has output queued and will hit the closed
//! pipe on its next write; the bounded read of the first line plus a blocking
//! `wait()` (the child is guaranteed to terminate once the pipe is closed) keeps
//! it deterministic.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

/// `completions bash` is offline and writes a large script to stdout — ideal for
/// provoking a broken-pipe write after the reader goes away.
#[test]
fn completions_into_closed_pipe_exits_without_panic() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_nbox"))
        .arg("completions")
        .arg("bash")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn nbox completions bash");

    // Read exactly the first line, then close the read end of the pipe. The
    // child has much more to write, so its next write hits the closed pipe.
    {
        let stdout = child.stdout.take().expect("child stdout");
        let mut reader = BufReader::new(stdout);
        let mut first = String::new();
        reader
            .read_line(&mut first)
            .expect("read first line of completions output");
        assert!(
            !first.is_empty(),
            "expected at least one line of completions output"
        );
        // `reader` (and thus the pipe's read end) is dropped at the end of this
        // scope, closing the pipe.
    }

    // The child is guaranteed to terminate once the pipe is closed (it either
    // gets killed by SIGPIPE or exits 141 from the error path), so a blocking
    // wait cannot hang.
    let status = child.wait().expect("wait for nbox completions bash");

    // The bug manifested as a Rust panic, which exits with code 101. The fix
    // must avoid that: the process exits via SIGPIPE or the clean 141 path.
    assert_ne!(
        status.code(),
        Some(101),
        "process panicked on broken pipe (exit 101): {status:?}"
    );

    // Confirm it really took the broken-pipe path rather than succeeding by luck:
    // either terminated by a signal (SIGPIPE under SIG_DFL) or exited with the
    // conventional 141 (128 + SIGPIPE) from the portable error path.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let broke_via_signal = status.signal() == Some(libc::SIGPIPE);
        let broke_via_code = status.code() == Some(141);
        assert!(
            broke_via_signal || broke_via_code,
            "expected a broken-pipe exit (SIGPIPE or code 141), got {status:?}"
        );
    }

    // No panic message should have leaked to stderr.
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        use std::io::Read;
        let _ = err.read_to_string(&mut stderr);
    }
    assert!(
        !stderr.contains("Broken pipe") && !stderr.contains("panicked"),
        "stderr should be quiet on broken pipe, got: {stderr:?}"
    );
}
