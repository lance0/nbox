//! Shared harness for driving `nbox serve` (the stdio MCP transport) in
//! integration tests. Spawns the *real* compiled binary
//! (`env!("CARGO_BIN_EXE_nbox")`) and speaks newline-delimited JSON-RPC over its
//! stdin/stdout.
//!
//! Determinism / no-hang: a reader thread streams stdout lines onto a channel so
//! every read is bounded by `recv_timeout`; a missing/garbled response fails
//! fast instead of hanging. On drop the child is killed and reaped, so no process
//! is left behind.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::NamedTempFile;

/// The protocol version the server advertises (`ProtocolVersion::LATEST`). The
/// server negotiates down to an older client version, so sending the latest is
/// always accepted.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// Every read off the child's stdout is bounded by this — a generous ceiling so
/// a broken server fails fast rather than hanging the suite.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// A live `nbox serve` child plus the plumbing to talk to it.
pub struct ServeChild {
    child: Child,
    // `Option` so `shutdown` can drop stdin (EOF) without leaving an invalid field.
    stdin: Option<std::process::ChildStdin>,
    lines: Receiver<String>,
    // Kept so the temp config outlives the running child.
    _config: NamedTempFile,
}

impl ServeChild {
    /// Spawn `nbox serve <serve_args>` with `config_toml` written to a temp file
    /// (`--config`) and `token` exported as `NBOX_TOKEN` (the direct override
    /// `connect()` reads first). Logging is forced quiet + to stderr so the
    /// stdout-is-pure-JSON-RPC invariant holds regardless of the caller's env.
    pub fn spawn(config_toml: &str, serve_args: &[&str], token: &str) -> Self {
        let mut config = NamedTempFile::new().expect("create temp config");
        config
            .write_all(config_toml.as_bytes())
            .expect("write temp config");
        config.flush().expect("flush temp config");

        let mut command = Command::new(env!("CARGO_BIN_EXE_nbox"));
        command.arg("--config").arg(config.path()).arg("serve");
        command.args(serve_args);
        let mut child = command
            .env("NBOX_TOKEN", token)
            .env_remove("NBOX_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn nbox serve");

        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");

        let (tx, rx) = channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break; // receiver gone — test finished
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        ServeChild {
            child,
            stdin: Some(stdin),
            lines: rx,
            _config: config,
        }
    }

    /// Send one compact JSON value as a single newline-terminated line.
    pub fn send(&mut self, msg: &Value) {
        let line = serde_json::to_string(msg).expect("serialize message");
        assert!(!line.contains('\n'), "framing must be one line per message");
        let stdin = self.stdin.as_mut().expect("child stdin is open");
        writeln!(stdin, "{line}").expect("write to child stdin");
        stdin.flush().expect("flush child stdin");
    }

    /// Read the next stdout line within the bounded timeout, asserting it parses
    /// as JSON (the stdout-is-pure-JSON-RPC invariant), and return the value.
    fn next_json(&self) -> Value {
        match self.lines.recv_timeout(READ_TIMEOUT) {
            Ok(line) => serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout line was not valid JSON: {e}\nline: {line:?}")),
            Err(RecvTimeoutError::Timeout) => {
                panic!("timed out waiting for a stdout line from `nbox serve`")
            }
            Err(RecvTimeoutError::Disconnected) => {
                panic!("`nbox serve` closed stdout before sending the expected response")
            }
        }
    }

    /// Read responses until one carries `id == want_id` (skipping notifications).
    pub fn read_response(&self, want_id: i64) -> Value {
        loop {
            let v = self.next_json();
            if v.get("id").and_then(Value::as_i64) == Some(want_id) {
                return v;
            }
        }
    }

    /// `initialize` (id 1) + `notifications/initialized`. Returns the initialize
    /// result message.
    pub fn handshake(&mut self) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "nbox-it", "version": "0.0.0" }
            }
        }));
        let init = self.read_response(1);
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        init
    }

    /// `tools/call` `name` with `args` at `id`; returns the raw response message.
    pub fn call(&mut self, id: i64, name: &str, args: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": args }
        }));
        self.read_response(id)
    }

    /// Close stdin (EOF) and ensure the child has exited, killing it as a backstop.
    pub fn shutdown(mut self) {
        self.stdin = None;
        for _ in 0..100 {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ServeChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Extract a successful tool's structured payload (the `Json<T>` it returns).
pub fn tool_payload(msg: &Value) -> Value {
    assert!(msg.get("error").is_none(), "tool returned an error: {msg}");
    let result = &msg["result"];
    assert_ne!(
        result["isError"],
        json!(true),
        "tool execution error: {msg}"
    );
    if let Some(sc) = result.get("structuredContent")
        && !sc.is_null()
    {
        return sc.clone();
    }
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result has no structuredContent/text: {msg}"));
    serde_json::from_str(text).unwrap_or_else(|_| panic!("tool result text not JSON: {text}"))
}

/// The error text of a *failed* tool call — a JSON-RPC `error`, or an `isError`
/// content payload — or `None` when the call succeeded. For negative tests.
pub fn tool_error(msg: &Value) -> Option<String> {
    if let Some(e) = msg.get("error") {
        return Some(e.to_string());
    }
    let result = msg.get("result")?;
    if result.get("isError") == Some(&json!(true)) {
        return Some(
            result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }
    None
}
