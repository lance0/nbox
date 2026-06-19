use std::ffi::OsStr;
use std::io::Write;
use std::process::{Command, Stdio};

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
