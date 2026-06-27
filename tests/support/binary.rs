use std::ffi::OsStr;
use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;
use tempfile::NamedTempFile;

pub struct CommandOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_nbox<I, S>(args: I) -> CommandOutput
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = Command::new(env!("CARGO_BIN_EXE_nbox"))
        .args(args)
        .env_remove("NBOX_LOG")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn nbox");

    CommandOutput {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

pub fn temp_config(url: &str) -> NamedTempFile {
    let mut config = NamedTempFile::new().expect("create temp config");
    write!(
        config,
        "active_profile = \"test\"\n\
         \n\
         [profiles.test]\n\
         url = \"{url}\"\n\
         token_env = \"NBOX_TEST_TOKEN_UNUSED\"\n"
    )
    .expect("write temp config");
    config.flush().expect("flush temp config");
    config
}

/// Assert the process-level error contract: a stable exit code, EMPTY stdout
/// (errors never pollute the data stream), and an actionable stderr substring.
/// `stderr_contains` accepts `impl AsRef<str>` so call sites can pass `&str`
/// or `String` interchangeably.
pub fn assert_error_contract(output: &CommandOutput, code: i32, stderr_contains: impl AsRef<str>) {
    assert_eq!(output.code, Some(code), "stderr: {}", output.stderr);
    assert!(
        output.stdout.is_empty(),
        "error paths must keep stdout clean, got: {:?}",
        output.stdout
    );
    assert!(
        output.stderr.contains(stderr_contains.as_ref()),
        "stderr should contain {:?}, got: {:?}",
        stderr_contains.as_ref(),
        output.stderr
    );
}

/// Assert the success-path contract: exit code 0 (stderr is reported on failure
/// for context but otherwise unconstrained — warnings on a success path are
/// allowed).
pub fn assert_success(output: &CommandOutput) {
    assert_eq!(output.code, Some(0), "stderr: {}", output.stderr);
}

/// Assert the process exited 0 and parse stdout as JSON. Panics with both the
/// parse error and the raw stdout if the output is not valid JSON, so a
/// malformed-data regression is obvious from the failure message.
pub fn assert_json_stdout(output: &CommandOutput) -> Value {
    assert_success(output);
    serde_json::from_str(&output.stdout)
        .unwrap_or_else(|e| panic!("stdout is not valid JSON: {e}\n{}", output.stdout))
}
