//! Observability contracts for `nbox status` at the process boundary.
//!
//! These drive the real compiled `nbox` binary against a `wiremock` NetBox so
//! the public `nbox status --json` shape and exit-code contract are pinned where
//! an agent (or script) actually consumes them: stable top-level key set, nested
//! `api`/`capabilities`/`token` shapes, and exit 3 on authentication failure with
//! clean stdout. The mirror contract for the MCP `nbox_status` tool lives in
//! `src/mcp/tests.rs::contracts::status_report_shape_is_pinned`.

use std::process::{Command, Stdio};

use serde_json::Value;
use serde_json::json;
use support::binary::CommandOutput;
use tempfile::NamedTempFile;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

/// Assert a JSON object has exactly these top-level keys (order-independent).
/// Pins the contract's key set: a removed or renamed field, or a new one not
/// listed here, fails the test. Mirrors the `assert_keys` helper in the MCP
/// contracts module so the CLI and MCP contracts read identically.
fn assert_keys(value: &Value, expected: &[&str]) {
    let obj = value.as_object().expect("a JSON object");
    let mut got: Vec<&str> = obj.keys().map(String::as_str).collect();
    got.sort_unstable();
    let mut want: Vec<&str> = expected.to_vec();
    want.sort_unstable();
    assert_eq!(got, want, "key set drifted; full value: {value}");
}

/// Assert the binary error contract: stable exit code, clean stdout, and an
/// actionable substring on stderr. Mirrors `assert_error_contract` in
/// `error_contract_tests.rs`.
fn assert_error_contract(output: &CommandOutput, code: i32, expected_stderr: impl AsRef<str>) {
    assert_eq!(output.code, Some(code), "stderr: {}", output.stderr);
    assert!(
        output.stdout.is_empty(),
        "error paths must keep stdout clean, got: {:?}",
        output.stdout
    );
    assert!(
        output.stderr.contains(expected_stderr.as_ref()),
        "stderr should contain {:?}, got: {:?}",
        expected_stderr.as_ref(),
        output.stderr
    );
}

/// A temp config whose `token_env` is deliberately never exported, so token
/// resolution falls through to `NBOX_TOKEN` (set per Command). Mirrors the
/// shared `support::binary::temp_config` plus an explicit token env name.
fn temp_config(url: &str) -> NamedTempFile {
    support::binary::temp_config(url)
}

/// Run `nbox --config <config> --no-tui --json status` with `NBOX_TOKEN` set.
/// Token precedence is `token_env` → `NBOX_TOKEN` → config token; the profile's
/// `token_env` (`NBOX_TEST_TOKEN_UNUSED`) is never exported, so resolution lands
/// on the `NBOX_TOKEN` we set here (matching the pattern in `it_netbox.rs`).
fn run_status(config: &NamedTempFile, token: &str) -> CommandOutput {
    let out = Command::new(env!("CARGO_BIN_EXE_nbox"))
        .arg("--config")
        .arg(config.path())
        .arg("--no-tui")
        .arg("--json")
        .arg("status")
        .env("NBOX_TOKEN", token)
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

/// `nbox status --json` key-set contract — mirrors the MCP
/// `status_report_shape_is_pinned` test at the process boundary, so an agent
/// reading stdout sees the same shape a host reading the MCP tool gets.
#[tokio::test]
async fn cli_status_key_set_is_pinned() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/status/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "netbox-version": "4.5.5",
            "django-version": "5.1",
            "python-version": "3.12"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/authentication-check/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "username": "admin"
        })))
        .mount(&server)
        .await;
    // A REST-only profile never probes GraphQL, but mount a 404 defensively so a
    // future preference change can't make this test flap on an unmocked path.
    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let config = temp_config(&server.uri());
    let out = run_status(&config, "test-token");

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let value: Value = serde_json::from_str(&out.stdout).expect("parse status JSON");

    // Top-level contract: every key always present (no skip_serializing).
    assert_keys(
        &value,
        &[
            "api",
            "capabilities",
            "django_version",
            "netbox_url",
            "netbox_version",
            "python_version",
            "token",
        ],
    );

    // Per-surface routing: each surface reports configured + effective; the
    // optional `reason` is omitted when there is no fallback (REST profile).
    assert_keys(&value["api"], &["search", "vrf", "route_target"]);
    assert_keys(&value["api"]["search"], &["configured", "effective"]);
    assert_keys(&value["api"]["vrf"], &["configured", "effective"]);
    assert_keys(&value["api"]["route_target"], &["configured", "effective"]);

    // Capability summary: the three blocks.
    assert_keys(&value["capabilities"], &["graphql", "rest", "version"]);

    // Credential preflight: the `token` verdict carries the discriminator and,
    // on `valid`, the resolved username. `display` is omitted when NetBox
    // reports none distinct from the username (the mock body has no `display`).
    assert_eq!(value["token"]["status"], "valid");
    assert_keys(&value["token"], &["status", "username"]);
    assert_eq!(value["token"]["username"], "admin");
}

/// `nbox status` exits 3 with clean stdout and an "authentication failed"
/// message when `/api/status/` rejects the token (401). Pins the exit-code
/// contract a script relies on to distinguish auth failure from a generic error.
#[tokio::test]
async fn cli_status_exits_3_on_auth_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/status/"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let config = temp_config(&server.uri());
    let out = run_status(&config, "test-token");

    assert_error_contract(&out, 3, "authentication failed");
}
