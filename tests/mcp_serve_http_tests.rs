//! End-to-end test of `nbox serve --http` — the read-only MCP server over the
//! opt-in loopback HTTP transport (rung 2 of the transport ladder; DESIGN §24).
//!
//! Gated behind the `http` feature: the whole file compiles only when the
//! transport is built in (the binary it drives must understand `--http`). It
//! mirrors `tests/mcp_serve_tests.rs` (stdio): launch the *real* compiled binary
//! against a throwaway config + dummy token (no network — the handshake and
//! tools/list never call NetBox), then speak MCP over HTTP.
//!
//! What it proves:
//!   1. `nbox serve --http` binds the loopback address and serves the protocol:
//!      the `initialize` handshake succeeds over Streamable HTTP and `tools/list`
//!      returns the expected tool set.
//!   2. The `Origin` check rejects a non-loopback origin with 403 (DNS-rebinding
//!      defense), and advertises `MCP-Protocol-Version: 2025-11-25`.
//!   3. The optional static bearer (`--http-token`): a missing/wrong bearer is
//!      401, the correct bearer is accepted.
//!   4. OIDC resource-server **writes** (Pattern 2): a minted RS256 JWT drives
//!      `nbox_plan_write` + `nbox_apply_write` end to end through the real auth
//!      gate → identity propagation → per-user credential vault → the caller's
//!      NetBox token on the write (header-matched on the mock NetBox, so the
//!      identity bridge is a precondition of the flow succeeding). A token
//!      without `nbox:write` and an unmapped `sub` are refused before any NetBox
//!      call. This is the path the #122 regression shipped non-functional.
//!
//! Determinism / no-hang: the SSE body is read chunk-by-chunk and we stop as
//! soon as the awaited JSON-RPC `id` arrives (SSE keep-alive holds the stream
//! open otherwise), each read bounded by a timeout so a broken server fails fast.

#![cfg(feature = "http")]

use std::io::Write;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use reqwest::StatusCode;
use rsa::RsaPrivateKey;
use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};
use rsa::traits::PublicKeyParts as _;
use serde_json::{Value, json};
use tempfile::NamedTempFile;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The protocol version the server advertises (DESIGN §24 / `MCP_PROTOCOL_VERSION`).
const PROTOCOL_VERSION: &str = "2025-11-25";

/// Bound on each network read; the handshake is local + offline so responses
/// arrive in milliseconds — this only exists so a broken server fails fast.
const READ_TIMEOUT: Duration = Duration::from_secs(20);

/// The exact tool set `nbox serve` exposes — identical to the stdio path, since
/// HTTP reuses the same handler + tool router.
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

/// A live `nbox serve --http` child plus its base URL. On drop the child is
/// killed and reaped so no process is left behind.
struct HttpServer {
    child: Child,
    base: String,
    _config: NamedTempFile,
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl HttpServer {
    /// Spawn `nbox serve --http 127.0.0.1:<port>` on a free ephemeral port,
    /// optionally with a static bearer token, and wait until it accepts
    /// connections. The profile URL is unreachable on purpose (offline test).
    fn spawn(token: Option<&str>) -> Self {
        let port = free_port();
        let addr = format!("127.0.0.1:{port}");

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

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nbox"));
        cmd.arg("--config")
            .arg(config.path())
            .arg("serve")
            .arg("--http")
            .arg(&addr)
            .env("NBOX_TOKEN", "dummy")
            // Don't let an inherited NBOX_SERVE_TOKEN perturb the no-auth tests.
            .env_remove("NBOX_SERVE_TOKEN")
            .env_remove("NBOX_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(t) = token {
            cmd.arg("--http-token").arg(t);
        }
        let child = cmd.spawn().expect("spawn nbox serve --http");

        let server = HttpServer {
            child,
            base: format!("http://{addr}"),
            _config: config,
        };
        server.wait_until_ready();
        server
    }

    /// Poll the listener until it accepts a TCP connection (bounded), so the
    /// test never races the server's startup.
    fn wait_until_ready(&self) {
        let host_port = self.base.trim_start_matches("http://").to_string();
        for _ in 0..200 {
            if std::net::TcpStream::connect(&host_port).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("nbox serve --http never started accepting connections");
    }

    fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base)
    }
}

/// Reserve a free TCP port by binding to `:0`, then release it. A small race
/// window exists before the child re-binds it; acceptable for a local test.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// Read the SSE response body chunk-by-chunk until a `data:` line carries a
/// JSON-RPC message with `id == want_id`, returning that message. SSE keep-alive
/// holds the stream open, so we must stop on the awaited id rather than on EOF.
async fn read_sse_for_id(mut resp: reqwest::Response, want_id: i64) -> Value {
    let mut buf = String::new();
    loop {
        let chunk = tokio::time::timeout(READ_TIMEOUT, resp.chunk())
            .await
            .expect("timed out reading SSE body")
            .expect("error reading SSE body");
        let Some(bytes) = chunk else {
            panic!("SSE stream ended before id {want_id} arrived; got:\n{buf}");
        };
        buf.push_str(&String::from_utf8_lossy(&bytes));
        for line in buf.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() {
                continue; // priming / keep-alive
            }
            if let Ok(v) = serde_json::from_str::<Value>(data)
                && v.get("id").and_then(Value::as_i64) == Some(want_id)
            {
                return v;
            }
        }
    }
}

/// POST a JSON-RPC message to `/mcp` with the MCP-required Accept header,
/// optional session id, and optional bearer. Returns the raw response.
async fn post(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    session: Option<&str>,
    bearer: Option<&str>,
) -> reqwest::Response {
    let mut req = client
        .post(url)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .json(body);
    if let Some(sid) = session {
        req = req.header("mcp-session-id", sid);
    }
    if let Some(b) = bearer {
        req = req.header("authorization", format!("Bearer {b}"));
    }
    req.send().await.expect("send POST /mcp")
}

/// Drive the full handshake (`initialize` → `notifications/initialized`) and
/// return the negotiated session id, asserting the init result is well-formed.
async fn handshake(client: &reqwest::Client, url: &str, bearer: Option<&str>) -> String {
    let init = post(
        client,
        url,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "nbox-http-e2e", "version": "0.0.0" }
            }
        }),
        None,
        bearer,
    )
    .await;
    assert_eq!(init.status(), StatusCode::OK, "initialize should be 200");
    // The protocol version is advertised on every response.
    assert_eq!(
        init.headers()
            .get("mcp-protocol-version")
            .and_then(|v| v.to_str().ok()),
        Some(PROTOCOL_VERSION),
        "missing/wrong MCP-Protocol-Version header"
    );
    let session = init
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .expect("initialize must return an Mcp-Session-Id")
        .to_string();

    let result = read_sse_for_id(init, 1).await;
    let result = result
        .get("result")
        .unwrap_or_else(|| panic!("initialize had no result: {result}"));
    assert!(
        result["serverInfo"].get("name").is_some(),
        "initialize result missing serverInfo.name: {result}"
    );
    assert!(
        result["capabilities"].get("tools").is_some(),
        "initialize result missing capabilities.tools: {result}"
    );
    assert!(
        result["capabilities"].get("resources").is_some(),
        "initialize result missing capabilities.resources: {result}"
    );
    assert!(
        result["capabilities"].get("prompts").is_some(),
        "initialize result missing capabilities.prompts: {result}"
    );
    assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);

    // notifications/initialized (a notification → 202 Accepted, no body of note).
    let ack = post(
        client,
        url,
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        Some(&session),
        bearer,
    )
    .await;
    assert_eq!(ack.status(), StatusCode::ACCEPTED);

    session
}

#[tokio::test]
async fn http_handshake_lists_all_tools() {
    let server = HttpServer::spawn(None);
    let client = reqwest::Client::new();
    let url = server.mcp_url();

    let session = handshake(&client, &url, None).await;

    let list = post(
        &client,
        &url,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(list.status(), StatusCode::OK);
    let list = read_sse_for_id(list, 2).await;
    let tools = list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list had no result.tools array: {list}"));

    let mut got: Vec<String> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("tool name").to_string())
        .collect();
    got.sort();
    let mut want: Vec<String> = EXPECTED_TOOLS.iter().map(ToString::to_string).collect();
    want.sort();
    assert_eq!(got, want, "tools/list returned an unexpected tool set");
}

#[tokio::test]
async fn http_rejects_non_loopback_origin_with_403() {
    let server = HttpServer::spawn(None);
    let client = reqwest::Client::new();

    // A cross-origin request from a non-loopback site is the DNS-rebinding
    // threat — it must be refused before reaching the MCP handler.
    let resp = client
        .post(server.mcp_url())
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("origin", "http://evil.example.com")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "attacker", "version": "0.0.0" }
            }
        }))
        .send()
        .await
        .expect("send cross-origin POST");

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "non-loopback Origin must be rejected with 403"
    );

    // A loopback origin, by contrast, is accepted (handshake succeeds).
    let ok = client
        .post(server.mcp_url())
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("origin", "http://127.0.0.1")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "local", "version": "0.0.0" }
            }
        }))
        .send()
        .await
        .expect("send loopback-origin POST");
    assert_eq!(
        ok.status(),
        StatusCode::OK,
        "a loopback Origin must be accepted"
    );
}

#[tokio::test]
async fn http_static_bearer_is_enforced() {
    const TOKEN: &str = "s3cr3t-bearer-value";
    let server = HttpServer::spawn(Some(TOKEN));
    let client = reqwest::Client::new();
    let url = server.mcp_url();

    let init_body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "bearer-test", "version": "0.0.0" }
        }
    });

    // No bearer → 401.
    let missing = post(&client, &url, &init_body, None, None).await;
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED, "missing bearer");

    // Wrong bearer → 401.
    let wrong = post(&client, &url, &init_body, None, Some("not-the-token")).await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED, "wrong bearer");

    // Correct bearer → 200, and the full handshake works.
    let session = handshake(&client, &url, Some(TOKEN)).await;
    assert!(
        !session.is_empty(),
        "correct bearer should establish a session"
    );
}

// =========================================================================
// OIDC + per-user write E2E (Pattern 2). The tests above prove the read-only
// transport; these prove the *write* path end to end through the real OIDC
// resource server: a minted RS256 JWT → gate validation → identity propagation
// (request Parts → rmcp RequestContext) → vault → the caller's per-user NetBox
// token on the write. The per-user token is made **intrinsic** by header-matching
// the mock NetBox on the caller's Bearer, so the flow can only succeed if the
// identity bridged correctly — the exact thing the #122 regression got wrong (a
// hardcoded placeholder sub that never resolved). A real IdP keypair signs the
// tokens; a wiremock endpoint serves the matching JWKS.
// =========================================================================

const ISSUER: &str = "https://idp.test/";
const AUDIENCE: &str = "https://nbox.test/mcp";

/// A throwaway IdP: an RSA keypair, the JWKS it publishes (derived from the
/// public key's n/e), and a signer for minting RS256 tokens.
struct TestIdp {
    private_pem: String,
    n: String,
    e: String,
    kid: String,
}

fn make_idp() -> TestIdp {
    let mut rng = rand::thread_rng();
    let key = RsaPrivateKey::new(&mut rng, 2048).expect("generate RSA test key");
    let pem = key
        .to_pkcs1_pem(LineEnding::LF)
        .expect("encode private key PEM")
        .to_string();
    let pubkey = key.to_public_key();
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    TestIdp {
        private_pem: pem,
        n: b64.encode(pubkey.n().to_bytes_be()),
        e: b64.encode(pubkey.e().to_bytes_be()),
        kid: "nbox-e2e-key-1".to_string(),
    }
}

impl TestIdp {
    fn jwks(&self) -> Value {
        json!({ "keys": [{
            "kty": "RSA", "use": "sig", "alg": "RS256",
            "kid": self.kid, "n": self.n, "e": self.e,
        }]})
    }

    fn mint(&self, iss: &str, aud: &str, sub: &str, scope: &str) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let claims = json!({
            "iss": iss, "aud": aud, "sub": sub, "scope": scope,
            "iat": now, "exp": now + 3600, "jti": "e2e-jti-1",
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.kid.clone());
        encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(self.private_pem.as_bytes()).expect("encoding key"),
        )
        .expect("sign token")
    }
}

/// Spawn `nbox serve --http` in OIDC resource-server mode with writes enabled and
/// a one-entry vault (`vault_sub` → `NBOX_VAULT_E2E`). The profile token (the
/// read-only service credential) and the per-user token are distinct values so
/// the mock can tell which reached NetBox.
fn spawn_oidc_writes(
    netbox_url: &str,
    jwks_url: &str,
    service_token: &str,
    vault_sub: &str,
    per_user_token: &str,
) -> HttpServer {
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let mut config = NamedTempFile::new().expect("create temp config");
    write!(
        config,
        "active_profile = \"test\"\n\
         \n\
         [profiles.test]\n\
         url = \"{netbox_url}\"\n\
         token_env = \"NBOX_TOKEN\"\n\
         \n\
         [serve]\n\
         allow_writes = true\n\
         \n\
         [serve.vault.\"{vault_sub}\"]\n\
         token_env = \"NBOX_VAULT_E2E\"\n",
    )
    .expect("write temp config");
    config.flush().expect("flush temp config");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nbox"));
    cmd.arg("--config")
        .arg(config.path())
        .arg("serve")
        .arg("--http")
        .arg(&addr)
        .arg("--oidc-issuer")
        .arg(ISSUER)
        .arg("--audience")
        .arg(AUDIENCE)
        .arg("--oidc-jwks-url")
        .arg(jwks_url)
        .arg("--allow-writes")
        .arg("--allowed-host")
        .arg("127.0.0.1")
        .env("NBOX_TOKEN", service_token)
        .env("NBOX_VAULT_E2E", per_user_token)
        .env_remove("NBOX_SERVE_TOKEN")
        .env_remove("NBOX_LOG")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn nbox serve --http (oidc)");
    let server = HttpServer {
        child,
        base: format!("http://{addr}"),
        _config: config,
    };
    server.wait_until_ready();
    server
}

/// Call a tool, returning the raw JSON-RPC response message (`result` or `error`).
async fn call_tool(
    client: &reqwest::Client,
    url: &str,
    session: &str,
    bearer: Option<&str>,
    id: i64,
    name: &str,
    arguments: Value,
) -> Value {
    let resp = post(
        client,
        url,
        &json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        }),
        Some(session),
        bearer,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "tools/call {name} status");
    read_sse_for_id(resp, id).await
}

/// Extract a successful tool's structured payload (the `Json<T>` it returns).
fn tool_payload(msg: &Value) -> Value {
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

/// Mount the NetBox endpoints an `ip reserve 10.0.0.0/24` touches — each gated on
/// the caller's per-user Bearer, so a request carrying any other token (e.g. the
/// service token) fails to match and the write fails. The per-user identity
/// bridge is therefore a precondition of the test passing, not a side assertion.
async fn mount_ip_reserve(netbox: &MockServer, per_user_token: &str) {
    let auth = format!("Bearer {per_user_token}");
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(header("authorization", auth.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{ "id": 1, "url": "u", "prefix": "10.0.0.0/24" }]
        })))
        .mount(netbox)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/1/available-ips/"))
        .and(header("authorization", auth.as_str()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([{ "address": "10.0.0.1/24" }])),
        )
        .mount(netbox)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/prefixes/1/available-ips/"))
        .and(header("authorization", auth.as_str()))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 7, "url": "u", "address": "10.0.0.1/24",
            "status": { "value": "active", "label": "Active" }
        })))
        .mount(netbox)
        .await;
}

async fn mount_jwks(idp: &TestIdp) -> MockServer {
    let jwks = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(idp.jwks()))
        .mount(&jwks)
        .await;
    jwks
}

#[tokio::test]
async fn oidc_write_bridges_caller_identity_to_per_user_token() {
    let idp = make_idp();
    let jwks = mount_jwks(&idp).await;

    let per_user = "nbt_alice.aaaaaaaaaaaaaaaa";
    let service = "nbt_service.ssssssssssssssss";
    let netbox = MockServer::start().await;
    mount_ip_reserve(&netbox, per_user).await;

    let server = spawn_oidc_writes(
        &netbox.uri(),
        &format!("{}/jwks", jwks.uri()),
        service,
        "alice",
        per_user,
    );
    let client = reqwest::Client::new();
    let url = server.mcp_url();
    let jwt = idp.mint(ISSUER, AUDIENCE, "alice", "nbox:read nbox:write");

    let session = handshake(&client, &url, Some(&jwt)).await;

    // plan → a MutationPlan; the read-before-write runs under alice's token.
    let plan_msg = call_tool(
        &client,
        &url,
        &session,
        Some(&jwt),
        2,
        "nbox_plan_write",
        json!({ "operation": { "kind": "ip_reserve", "prefix": "10.0.0.0/24" } }),
    )
    .await;
    let plan = tool_payload(&plan_msg);
    assert_eq!(plan["operation"], "allocate", "plan: {plan}");
    assert!(
        !plan["confirm_token"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "plan carries a confirm token"
    );

    // apply → a MutationReceipt; the POST runs under alice's token too. Reaching
    // this point at all means every NetBox call matched alice's Bearer.
    let apply_msg = call_tool(
        &client,
        &url,
        &session,
        Some(&jwt),
        3,
        "nbox_apply_write",
        json!({ "plan": plan }),
    )
    .await;
    let receipt = tool_payload(&apply_msg);
    assert_eq!(receipt["applied"], json!(true), "receipt: {receipt}");
    assert_eq!(receipt["object"]["address"], json!("10.0.0.1/24"));
}

#[tokio::test]
async fn oidc_write_rejected_without_write_scope() {
    let idp = make_idp();
    let jwks = mount_jwks(&idp).await;
    let netbox = MockServer::start().await; // no endpoints — must never be reached

    let server = spawn_oidc_writes(
        &netbox.uri(),
        &format!("{}/jwks", jwks.uri()),
        "nbt_service.x",
        "alice",
        "nbt_alice.y",
    );
    let client = reqwest::Client::new();
    let url = server.mcp_url();
    // read scope only → handshake works, but the write tool is refused.
    let jwt = idp.mint(ISSUER, AUDIENCE, "alice", "nbox:read");

    let session = handshake(&client, &url, Some(&jwt)).await;
    let msg = call_tool(
        &client,
        &url,
        &session,
        Some(&jwt),
        2,
        "nbox_plan_write",
        json!({ "operation": { "kind": "ip_reserve", "prefix": "10.0.0.0/24" } }),
    )
    .await;
    let err = msg["error"]["message"].as_str().unwrap_or_default();
    assert!(
        err.contains("nbox:write"),
        "expected a missing-scope error: {msg}"
    );
    assert!(
        netbox
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "no NetBox call on a scope refusal"
    );
}

#[tokio::test]
async fn oidc_write_rejected_for_unmapped_sub() {
    let idp = make_idp();
    let jwks = mount_jwks(&idp).await;
    let netbox = MockServer::start().await;

    // The vault maps "alice"; the caller authenticates as "carol".
    let server = spawn_oidc_writes(
        &netbox.uri(),
        &format!("{}/jwks", jwks.uri()),
        "nbt_service.x",
        "alice",
        "nbt_alice.y",
    );
    let client = reqwest::Client::new();
    let url = server.mcp_url();
    let jwt = idp.mint(ISSUER, AUDIENCE, "carol", "nbox:read nbox:write");

    let session = handshake(&client, &url, Some(&jwt)).await;
    let msg = call_tool(
        &client,
        &url,
        &session,
        Some(&jwt),
        2,
        "nbox_plan_write",
        json!({ "operation": { "kind": "ip_reserve", "prefix": "10.0.0.0/24" } }),
    )
    .await;
    let err = msg["error"]["message"].as_str().unwrap_or_default();
    assert!(
        err.contains("no vault entry") && err.contains("carol"),
        "expected a vault-miss error for carol: {msg}"
    );
    assert!(
        netbox
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "no NetBox call on a vault miss"
    );
}
