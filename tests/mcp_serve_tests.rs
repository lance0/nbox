//! End-to-end test of `nbox serve` — the MCP server (read-only by default) over stdio.
//!
//! This launches the *real* compiled binary (`env!("CARGO_BIN_EXE_nbox")`) with
//! `serve`, then speaks newline-delimited JSON-RPC over its stdin/stdout. It
//! proves three things at once:
//!
//!   1. `nbox serve` is wired correctly: `connect()` builds a client from a
//!      minimal profile + a dummy token without hitting the network, and the MCP
//!      server starts.
//!   2. The server speaks the protocol: `initialize` returns a `result` with
//!      `serverInfo` + `capabilities.tools`, and `tools/list` returns exactly the
//!      expected tool set.
//!   3. The stdio invariant holds: every line the child writes to stdout is valid
//!      JSON-RPC (nothing — banners, logs, prompts — leaks onto stdout).
//!
//! Determinism / no-hang: the handshake is driven entirely off reading the
//! responses we expect (matched by JSON-RPC `id`), never off timing. A dedicated
//! reader thread streams stdout lines onto a channel; every read is bounded by
//! `recv_timeout`, so a missing/garbled response fails the test fast instead of
//! hanging. stdin is closed to signal EOF, and the child is waited on (then
//! killed as a backstop) so no process is ever left behind.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::NamedTempFile;

/// The protocol version the server advertises (`ProtocolVersion::LATEST` in
/// `src/mcp/mod.rs`). The server negotiates down to the client's version if it
/// is older, so sending the latest is always accepted.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// Every read off the child's stdout is bounded by this. The handshake is local
/// and offline, so responses arrive in milliseconds; this is a generous ceiling
/// that exists only so a broken server fails fast rather than hanging the suite.
const READ_TIMEOUT: Duration = Duration::from_secs(20);

/// The exact tool set `nbox serve` must expose (see the `#[tool(...)]` adapters
/// in `src/mcp/mod.rs`). Order-independent: we compare as sorted sets.
const EXPECTED_TOOLS: &[&str] = &[
    "nbox_status",
    "nbox_search",
    "nbox_get",
    "nbox_get_interface",
    "nbox_next_ip",
    "nbox_next_prefix",
    "nbox_journal",
    "nbox_history",
    "nbox_list_tags",
    "nbox_tagged",
    "nbox_cache_clear",
    "nbox_plan_write",
    "nbox_apply_write",
];

/// A live `nbox serve` child plus the plumbing to talk to it. Holds the child's
/// stdin (to send requests) and a channel of stdout lines (filled by a reader
/// thread). On drop, the child is killed and reaped as a backstop.
struct ServeChild {
    child: Child,
    // `Option` so `shutdown` can drop stdin (sending EOF) without leaving an
    // invalid field behind.
    stdin: Option<std::process::ChildStdin>,
    lines: Receiver<String>,
    // Kept so the temp config file lives at least as long as the running child.
    _config: NamedTempFile,
}

impl ServeChild {
    /// Spawn `nbox serve` against a throwaway config + dummy token. The profile
    /// URL is unreachable on purpose: the initialize/tools/list handshake never
    /// makes a network call, so the bogus URL is fine and keeps the test offline.
    fn spawn() -> Self {
        let mut config = NamedTempFile::new().expect("create temp config");
        write!(
            config,
            "active_profile = \"test\"\n\
             \n\
             [profiles.test]\n\
             url = \"http://127.0.0.1:1/\"\n\
             token_env = \"NBOX_TEST_TOKEN_UNUSED\"\n"
        )
        .expect("write temp config");
        config.flush().expect("flush temp config");

        let mut child = Command::new(env!("CARGO_BIN_EXE_nbox"))
            .arg("--config")
            .arg(config.path())
            .arg("serve")
            // The direct-override token env so `connect()` finds a token without
            // a real secret; nothing authenticates during the handshake.
            .env("NBOX_TOKEN", "dummy")
            // Pin logging quiet and to stderr regardless of the caller's env, so
            // the stdout-cleanliness assertion isn't perturbed by NBOX_LOG/RUST_LOG.
            .env_remove("NBOX_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn nbox serve");

        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");

        // Reader thread: stream stdout lines onto a channel so every read on the
        // test side is bounded by recv_timeout (no blocking read can hang us).
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
    fn send(&mut self, msg: &Value) {
        let line = serde_json::to_string(msg).expect("serialize message");
        // Exactly one compact JSON object per line — the stdio framing.
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

    /// Read responses until one carries `id == want_id`. Notifications and other
    /// ids are skipped; every consumed line is still validated as JSON by
    /// `next_json`, so this also enforces stdout cleanliness as a side effect.
    fn read_response(&self, want_id: i64) -> Value {
        loop {
            let v = self.next_json();
            if v.get("id").and_then(Value::as_i64) == Some(want_id) {
                return v;
            }
        }
    }

    /// Close stdin (EOF) and ensure the child has exited, killing it as a
    /// backstop so the test can never leave a process behind.
    fn shutdown(mut self) {
        // Dropping stdin closes it → the server's stdio transport sees EOF and
        // `serve` returns.
        self.stdin = None;

        // Give the child a brief, bounded chance to exit cleanly on EOF; if it
        // hasn't, kill it. Either way we reap it so there's no zombie.
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

#[test]
fn serve_handshake_lists_all_tools_with_clean_stdout() {
    let mut server = ServeChild::spawn();

    // 1) initialize (id 1) with a valid protocolVersion, empty capabilities, and
    //    a clientInfo. The server replies with the negotiated InitializeResult.
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "nbox-e2e-test", "version": "0.0.0" }
        }
    }));

    let init = server.read_response(1);
    assert_eq!(init["jsonrpc"], "2.0");
    let result = init
        .get("result")
        .unwrap_or_else(|| panic!("initialize had no result: {init}"));
    // serverInfo identifies the server (name/version present).
    let server_info = &result["serverInfo"];
    assert!(
        server_info.get("name").and_then(Value::as_str).is_some(),
        "initialize result missing serverInfo.name: {init}"
    );
    // capabilities.tools must be advertised (the server enables tools).
    assert!(
        result["capabilities"].get("tools").is_some(),
        "initialize result missing capabilities.tools: {init}"
    );
    // capabilities.resources must be advertised (the server enables resources).
    assert!(
        result["capabilities"].get("resources").is_some(),
        "initialize result missing capabilities.resources: {init}"
    );
    // capabilities.prompts must be advertised (the server enables the prompt catalog).
    assert!(
        result["capabilities"].get("prompts").is_some(),
        "initialize result missing capabilities.prompts: {init}"
    );
    // The negotiated protocol version is echoed back.
    assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);

    // 2) notifications/initialized (no id — a notification, no response expected).
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));

    // 3) tools/list (id 2).
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));

    let list = server.read_response(2);
    let tools = list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list had no result.tools array: {list}"));

    let mut got: Vec<String> = tools
        .iter()
        .map(|t| {
            t["name"]
                .as_str()
                .unwrap_or_else(|| panic!("tool entry missing name: {t}"))
                .to_string()
        })
        .collect();
    got.sort();

    let mut want: Vec<String> = EXPECTED_TOOLS.iter().map(ToString::to_string).collect();
    want.sort();

    assert_eq!(got, want, "tools/list returned an unexpected tool set");

    // 4) resources/templates/list (id 3): the server advertises the single
    //    `nbox://{kind}/{ref}` template. This proves the resource ServerHandler
    //    methods are wired through the real protocol, not just the inner helper.
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "resources/templates/list",
        "params": {}
    }));

    let templates = server.read_response(3);
    let list = templates["result"]["resourceTemplates"]
        .as_array()
        .unwrap_or_else(|| panic!("templates list had no result.resourceTemplates: {templates}"));
    assert_eq!(list.len(), 1, "expected exactly one resource template");
    assert_eq!(list[0]["uriTemplate"], "nbox://{kind}/{ref}");

    // 5) resources/list (id 4): no static resources (the template covers
    //    everything), so an empty list — but the method must succeed, not 404.
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "resources/list",
        "params": {}
    }));

    let resources = server.read_response(4);
    let empty = resources["result"]["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("resources/list had no result.resources: {resources}"));
    assert!(
        empty.is_empty(),
        "expected no static resources: {resources}"
    );

    // 6) prompts/list (id 5): the curated investigation-prompt catalog. Proves
    //    the prompt ServerHandler methods are wired through the real protocol.
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "prompts/list",
        "params": {}
    }));

    let prompts_resp = server.read_response(5);
    let prompts = prompts_resp["result"]["prompts"]
        .as_array()
        .unwrap_or_else(|| panic!("prompts/list had no result.prompts: {prompts_resp}"));
    let names: Vec<String> = prompts
        .iter()
        .map(|p| {
            p["name"]
                .as_str()
                .unwrap_or_else(|| panic!("prompt missing name: {p}"))
                .to_string()
        })
        .collect();
    assert_eq!(
        names,
        vec![
            "ip_utilization_audit",
            "cable_path_trace",
            "find_stale_prefixes",
            "object_change_review",
        ],
        "prompts/list returned an unexpected catalog"
    );

    // 7) prompts/get (id 6): expanding a named prompt returns a user-role
    //    message whose text references the nbox tools to call.
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "prompts/get",
        "params": {
            "name": "cable_path_trace",
            "arguments": {"device": "edge01", "interface": "xe-0/0/1"}
        }
    }));

    let get = server.read_response(6);
    let messages = get["result"]["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("prompts/get had no result.messages: {get}"));
    assert_eq!(messages.len(), 1, "expected one message: {get}");
    assert_eq!(messages[0]["role"], "user");
    let text = messages[0]["content"]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("prompt message had no text: {get}"));
    assert!(
        text.contains("interface=\"xe-0/0/1\""),
        "args not substituted: {text}"
    );
    assert!(
        text.contains("nbox_get_interface"),
        "plan missing tool ref: {text}"
    );

    // Close stdin and make sure the process exits (killed as a backstop).
    server.shutdown();
}
