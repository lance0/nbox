//! Integration tests for the ADR-0001 safe-write foundation: the interface
//! `description` pilot. Drives the real `nbox` binary against wiremock so the
//! contracts are pinned at the process boundary (exit code, clean stdout,
//! actionable stderr) AND at the wire boundary (the exact `PATCH` body/headers,
//! and that `--dry-run` sends no `PATCH`).
//!
//! Coverage map (ADR-0001 §9):
//! - planner: minimal patch, no-op, unsupported-field failure, scoped diff.
//! - wiremock: `--dry-run` no `PATCH`; `--allow-writes --confirm` sends the
//!   minimal `PATCH`, `If-Match` when an ETag is present, `changelog_message`
//!   when `--message` is given.
//! - stale precondition: the 4.6+ `412` path and the pre-4.6 `last_updated` +
//!   before-hash mismatch.
//! - binary stdout/stderr contracts: dry-run JSON, receipt JSON, usage error,
//!   stale object, validation failure.
//! - audit redaction: the audit log carries field NAMES, outcome, and a
//!   message_present/length — never the before/after values, the token, or the
//!   `--message` body.

mod support;

use std::process::{Command, Stdio};

use serde_json::{Value, json};
use support::binary::{CommandOutput, run_nbox, temp_config};
use tempfile::NamedTempFile;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A config whose profile carries a token_env that is never set, so the client
/// authenticates only when the test sets `NBOX_TOKEN` explicitly (for the
/// token-redaction assertion). Identical shape to `support::binary::temp_config`.
fn write_config(server_uri: &str) -> NamedTempFile {
    temp_config(server_uri)
}

/// Assert the process-level error contract: a stable exit code, EMPTY stdout
/// (errors never pollute the data stream), and an actionable stderr substring.
fn assert_error_contract(output: &CommandOutput, code: i32, stderr_contains: &str) {
    assert_eq!(output.code, Some(code), "stderr: {}", output.stderr);
    assert!(
        output.stdout.is_empty(),
        "error paths must keep stdout clean, got: {:?}",
        output.stdout
    );
    assert!(
        output.stderr.contains(stderr_contains),
        "stderr should contain {:?}, got: {:?}",
        stderr_contains,
        output.stderr
    );
}

/// The device + interface-name resolution mocks `plan_interface_description_update`
/// needs: a device by name, then the interface by (device_id, name) returning
/// the interface id + its list-state. The authoritative detail (with ETag) is
/// mounted separately per test since it varies (ETag / last_updated / stale).
async fn mount_resolution(server: &MockServer, device_id: u64, iface_id: u64, name: &str) {
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": device_id,
                "url": format!("{}/api/dcim/devices/{}/", server.uri(), device_id),
                "name": "edge01"
            }]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/interfaces/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": iface_id,
                "url": format!("{}/api/dcim/interfaces/{}/", server.uri(), iface_id),
                "name": name,
                "description": "uplink-old",
                "last_updated": "2026-06-26T10:00:00Z"
            }]
        })))
        .mount(server)
        .await;
}

/// The authoritative interface detail (GET `/api/dcim/interfaces/{id}/`), with an
/// optional `ETag` response header. `description` defaults to the current
/// value; tests that need a different current value (no-op, stale) override it.
async fn mount_detail(server: &MockServer, iface_id: u64, etag: Option<&str>, description: &str) {
    let mut resp = ResponseTemplate::new(200).set_body_json(json!({
        "id": iface_id,
        "url": format!("{}/api/dcim/interfaces/{}/", server.uri(), iface_id),
        "name": "xe-0/0/1",
        "description": description,
        "last_updated": "2026-06-26T10:00:00Z"
    }));
    if let Some(e) = etag {
        resp = resp.insert_header("ETag", e);
    }
    Mock::given(method("GET"))
        .and(path(format!("/api/dcim/interfaces/{iface_id}/")))
        .respond_with(resp)
        .mount(server)
        .await;
}

/// Count the `PATCH` requests the mock received.
async fn patch_count(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method.as_str() == "PATCH")
        .count()
}

fn run_set<'a>(config: &NamedTempFile, extra: &'a [&'a str]) -> CommandOutput {
    let mut args: Vec<&str> = vec!["--config", config.path().to_str().unwrap()];
    // `--no-tui` makes the non-interactive guarantee explicit (the binary test
    // process has no TTY anyway, but pin it so the TTY-prompt path is never
    // accidentally exercised here).
    args.push("--no-tui");
    args.extend_from_slice(&["interface", "edge01", "xe-0/0/1", "set", "description"]);
    args.extend_from_slice(extra);
    run_nbox(args)
}

// === planner: minimal patch, no-op, scoped diff ===========================

#[tokio::test]
async fn dry_run_sends_no_patch_and_emits_the_plan() {
    let server = MockServer::start().await;
    mount_resolution(&server, 7, 42, "xe-0/0/1").await;
    mount_detail(&server, 42, None, "uplink-old").await;

    let config = write_config(&server.uri());
    let out = run_set(&config, &["uplink-new", "--dry-run", "--json"]);

    assert_eq!(
        out.code,
        Some(0),
        "code={:?} stderr={} stdout={}",
        out.code,
        out.stderr,
        out.stdout
    );
    assert_eq!(patch_count(&server).await, 0, "--dry-run must not PATCH");

    let plan: Value = serde_json::from_str(&out.stdout).expect("plan JSON");
    assert_eq!(plan["schema_version"], json!(1));
    assert_eq!(plan["operation"], json!("update"));
    assert_eq!(plan["target"]["kind"], json!("interface"));
    assert_eq!(plan["target"]["id"], json!(42));
    assert_eq!(
        plan["target"]["endpoint"],
        json!("/api/dcim/interfaces/42/")
    );
    // Minimal patch: only the scoped field, never the full object.
    assert_eq!(plan["patch"], json!({"description": "uplink-new"}));
    // Scoped diff: only `description`, no unrelated fields.
    assert_eq!(plan["fields"].as_array().unwrap().len(), 1);
    assert_eq!(plan["fields"][0]["field"], json!("description"));
    assert_eq!(plan["fields"][0]["before"], json!("uplink-old"));
    assert_eq!(plan["fields"][0]["after"], json!("uplink-new"));
    assert_eq!(plan["no_op"], json!(false));
    // Precondition: no ETag → last_updated + before_hash.
    assert_eq!(plan["precondition"]["type"], json!("last_updated"));
    assert!(plan["precondition"]["before_hash"].is_string());
    assert!(plan["confirm_token"].is_string());
    assert!(plan["expires_at"].is_string());
    // The "planned, no changes sent" status line is plain-only; with --json the
    // plan is the whole stdout payload (no status line on either stream).
    assert!(
        out.stdout.trim_start().starts_with('{'),
        "stdout is JSON: {}",
        out.stdout
    );
}

#[tokio::test]
async fn dry_run_noop_marks_no_change_and_sends_no_patch() {
    let server = MockServer::start().await;
    mount_resolution(&server, 7, 42, "xe-0/0/1").await;
    // Current value == requested value → no-op.
    mount_detail(&server, 42, None, "uplink-old").await;

    let config = write_config(&server.uri());
    let out = run_set(&config, &["uplink-old", "--dry-run", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0);
    let plan: Value = serde_json::from_str(&out.stdout).expect("plan JSON");
    assert_eq!(plan["no_op"], json!(true));
    assert_eq!(plan["patch"], json!({}));
}

#[tokio::test]
async fn dry_run_plain_renders_diff_to_stdout_and_status_to_stderr() {
    let server = MockServer::start().await;
    mount_resolution(&server, 7, 42, "xe-0/0/1").await;
    mount_detail(&server, 42, None, "uplink-old").await;

    let config = write_config(&server.uri());
    let out = run_set(&config, &["uplink-new", "--dry-run"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0);
    // Plain diff on stdout; the "planned" status line on stderr.
    assert!(out.stdout.contains("description"), "stdout: {}", out.stdout);
    assert!(
        out.stdout.contains("uplink-old → uplink-new"),
        "stdout diff: {}",
        out.stdout
    );
    assert!(out.stderr.contains("planned, no changes sent"));
}

// === wiremock: apply sends the minimal PATCH with the right body/headers ===

#[tokio::test]
async fn apply_sends_minimal_patch_and_emits_receipt() {
    let server = MockServer::start().await;
    mount_resolution(&server, 7, 42, "xe-0/0/1").await;
    mount_detail(&server, 42, None, "uplink-old").await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/interfaces/42/"))
        // The minimal patch body — only `description`, never the full object.
        .and(body_partial_json(json!({"description": "uplink-new"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 42, "url": format!("{}/api/dcim/interfaces/42/", server.uri()),
            "name": "xe-0/0/1", "description": "uplink-new",
            "last_updated": "2026-06-26T10:30:00Z"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_set(
        &config,
        &["uplink-new", "--allow-writes", "--confirm", "--json"],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 1, "exactly one PATCH");
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["schema_version"], json!(1));
    assert_eq!(receipt["applied"], json!(true));
    assert_eq!(receipt["no_op"], json!(false));
    assert_eq!(receipt["status"], json!(200));
    assert_eq!(receipt["fields"][0]["after"], json!("uplink-new"));
    assert!(
        receipt["message"]
            .as_str()
            .unwrap()
            .contains("applied: interface"),
        "receipt message: {}",
        receipt["message"]
    );
}

#[tokio::test]
async fn apply_sends_if_match_when_an_etag_is_present() {
    let server = MockServer::start().await;
    mount_resolution(&server, 7, 42, "xe-0/0/1").await;
    // 4.6+ detail carries an ETag; the plan records it and the apply sends
    // `If-Match: <etag>` (ADR-0001 §3).
    mount_detail(&server, 42, Some("\"etag-v1\""), "uplink-old").await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/interfaces/42/"))
        // The PATCH mock ONLY matches when If-Match is sent — proving the
        // header is present. Without it, wiremock returns 404 and the test fails.
        .and(header("if-match", "\"etag-v1\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 42, "url": "u", "name": "xe-0/0/1", "description": "uplink-new"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_set(
        &config,
        &["uplink-new", "--allow-writes", "--confirm", "--json"],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 1);
    let plan_reqs: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "PATCH")
        .collect();
    assert_eq!(plan_reqs[0].headers.get("if-match").unwrap(), "\"etag-v1\"");
}

#[tokio::test]
async fn apply_includes_changelog_message_when_message_given() {
    let server = MockServer::start().await;
    mount_resolution(&server, 7, 42, "xe-0/0/1").await;
    mount_detail(&server, 42, None, "uplink-old").await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/interfaces/42/"))
        .and(body_partial_json(json!({
            "description": "uplink-new",
            "changelog_message": "rotating uplink xe-0/0/1"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 42, "url": "u", "name": "xe-0/0/1", "description": "uplink-new"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_set(
        &config,
        &[
            "uplink-new",
            "--allow-writes",
            "--confirm",
            "--message",
            "rotating uplink xe-0/0/1",
            "--json",
        ],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 1);
}

#[tokio::test]
async fn apply_noop_sends_no_patch_and_reports_no_change() {
    let server = MockServer::start().await;
    mount_resolution(&server, 7, 42, "xe-0/0/1").await;
    mount_detail(&server, 42, None, "uplink-old").await;
    // No PATCH mock mounted — a no-op must send none, and any PATCH would 404.

    let config = write_config(&server.uri());
    let out = run_set(
        &config,
        &["uplink-old", "--allow-writes", "--confirm", "--json"],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0, "no-op sends no PATCH");
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["applied"], json!(false));
    assert_eq!(receipt["no_op"], json!(true));
    assert_eq!(receipt["status"], json!(0));
    assert!(
        receipt["message"].as_str().unwrap().contains("no change"),
        "receipt message: {}",
        receipt["message"]
    );
}

// === stale precondition: 412 path + pre-4.6 fallback =====================

#[tokio::test]
async fn stale_412_is_a_stale_precondition_refusal() {
    let server = MockServer::start().await;
    mount_resolution(&server, 7, 42, "xe-0/0/1").await;
    mount_detail(&server, 42, Some("\"etag-v1\""), "uplink-old").await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/interfaces/42/"))
        .respond_with(ResponseTemplate::new(412).set_body_string("Precondition Failed"))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_set(
        &config,
        &["uplink-new", "--allow-writes", "--confirm", "--json"],
    );

    assert_error_contract(&out, 1, "object changed in NetBox");
    assert!(
        out.stderr.contains("re-run dry-run"),
        "stderr: {}",
        out.stderr
    );
}

#[tokio::test]
async fn stale_pre46_fallback_re_reads_and_refuses_on_last_updated_change() {
    let server = MockServer::start().await;
    mount_resolution(&server, 7, 42, "xe-0/0/1").await;
    // No ETag → pre-4.6 path. The plan reads last_updated T1; the apply re-read
    // must return a DIFFERENT last_updated (the object changed) so the
    // read-before-write check refuses. Mount T1 for exactly the first detail
    // GET (the plan), then T2 for the apply re-read.
    Mock::given(method("GET"))
        .and(path("/api/dcim/interfaces/42/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 42, "url": "u", "name": "xe-0/0/1",
            "description": "uplink-old",
            "last_updated": "2026-06-26T10:00:00Z"
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/interfaces/42/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 42, "url": "u", "name": "xe-0/0/1",
            "description": "uplink-old",
            "last_updated": "2026-06-26T11:00:00Z"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_set(
        &config,
        &["uplink-new", "--allow-writes", "--confirm", "--json"],
    );

    // The pre-4.6 read-before-write caught the change BEFORE any PATCH: no
    // mutation was attempted.
    assert_error_contract(&out, 1, "object changed in NetBox");
    assert_eq!(
        patch_count(&server).await,
        0,
        "stale pre-4.6 sends no PATCH"
    );
}

// === validation failure ===================================================

#[tokio::test]
async fn validation_400_surfaces_netbox_field_error_cleanly() {
    let server = MockServer::start().await;
    mount_resolution(&server, 7, 42, "xe-0/0/1").await;
    mount_detail(&server, 42, None, "uplink-old").await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/interfaces/42/"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"description": ["This field may not be blank."]})),
        )
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_set(
        &config,
        &["uplink-new", "--allow-writes", "--confirm", "--json"],
    );

    assert_error_contract(&out, 1, "NetBox rejected the patch");
    // The field-level detail NetBox returned is surfaced (with field context).
    assert!(out.stderr.contains("description"), "stderr: {}", out.stderr);
}

// === usage errors (exit 2, empty stdout) ==================================

#[tokio::test]
async fn confirm_without_allow_writes_is_a_usage_error() {
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_set(&config, &["uplink-new", "--confirm", "--json"]);
    assert_error_contract(&out, 2, "writes are not enabled");
    assert!(
        out.stderr.contains("--allow-writes"),
        "stderr: {}",
        out.stderr
    );
    assert_eq!(patch_count(&server).await, 0);
}

#[tokio::test]
async fn no_flags_is_a_usage_error_naming_allow_writes() {
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_set(&config, &["uplink-new", "--json"]);
    assert_error_contract(&out, 2, "writes are not enabled");
    assert!(out.stderr.contains("--dry-run"), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0);
}

#[tokio::test]
async fn allow_writes_without_confirm_is_usage_in_non_tty() {
    // `--allow-writes` but no `--confirm` in a non-TTY (the test process) → no
    // prompt is allowed, so it's a usage error naming `--confirm`. (On a TTY in
    // plain output this path would instead prompt; that TTY branch is not
    // exercisable from a piped test process.)
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_set(&config, &["uplink-new", "--allow-writes", "--json"]);
    assert_error_contract(&out, 2, "non-interactive write requires confirmation");
    assert!(out.stderr.contains("--confirm"), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0);
}

#[tokio::test]
async fn unsupported_field_is_a_usage_error_before_any_network() {
    // No mocks mounted: a usage error must not reach the network. `set status
    // active` is field-specific: the planner accepts only `description` in v1
    // (ADR-0001 §6), failing closed before resolve/connect.
    let config = write_config("http://unused.example/");
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "interface".as_ref(),
        "edge01".as_ref(),
        "xe-0/0/1".as_ref(),
        "set".as_ref(),
        "status".as_ref(),
        "active".as_ref(),
        "--dry-run".as_ref(),
    ]);
    assert_error_contract(&out, 2, "only `description` is writable");
}

#[tokio::test]
async fn overlength_message_is_a_usage_error_before_any_network() {
    // `--dry-run` bypasses the gate, so the planner runs and validates the
    // message length first — before resolving the interface (no network use).
    let config = write_config("http://unused.example/");
    let over = "x".repeat(201);
    let out = run_set(
        &config,
        &["uplink-new", "--dry-run", "--message", over.as_str()],
    );
    assert_error_contract(&out, 2, "200-character limit");
}

// === audit redaction (ADR-0001 §8) ========================================

#[tokio::test]
async fn audit_log_records_names_and_outcome_but_never_values_token_or_message_body() {
    let server = MockServer::start().await;
    mount_resolution(&server, 7, 42, "xe-0/0/1").await;
    mount_detail(&server, 42, None, "uplink-old").await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/interfaces/42/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 42, "url": "u", "name": "xe-0/0/1", "description": "uplink-new"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let log = NamedTempFile::new().expect("log file");
    let log_path = log.path().to_path_buf();
    drop(log); // let the binary own the file

    // A distinctive secret token, before/after values, and message body — none
    // of which may appear in the audit log (only field NAMES, a message_present
    // flag + length, and the outcome).
    let out = Command::new(env!("CARGO_BIN_EXE_nbox"))
        .arg("--config")
        .arg(config.path())
        .arg("--no-tui")
        .arg("--log-file")
        .arg(&log_path)
        .arg("--log-level")
        .arg("nbox::write_audit=info")
        .args([
            "interface",
            "edge01",
            "xe-0/0/1",
            "set",
            "description",
            "uplink-new",
            "--allow-writes",
            "--confirm",
            "--message",
            "rotating-uplink-secret",
            "--json",
        ])
        .env("NBOX_TOKEN", "secret-nbox-token-12345")
        .env_remove("NBOX_LOG")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn nbox");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let log_text = std::fs::read_to_string(&log_path).expect("read log file");
    // Allow-list fields that MUST appear.
    assert!(log_text.contains("nbox::write_audit"), "log: {log_text}");
    assert!(
        log_text.contains("fields=\"description\"") || log_text.contains("fields=description"),
        "field NAME recorded: {log_text}"
    );
    assert!(
        log_text.contains("outcome=\"applied\"") || log_text.contains("outcome=applied"),
        "outcome recorded: {log_text}"
    );
    assert!(
        log_text.contains("message_present=true"),
        "message_present flag: {log_text}"
    );
    // Redaction: none of these may leak into the audit log.
    assert!(
        !log_text.contains("uplink-old"),
        "before value leaked: {log_text}"
    );
    assert!(
        !log_text.contains("uplink-new"),
        "after value leaked: {log_text}"
    );
    assert!(
        !log_text.contains("rotating-uplink-secret"),
        "message body leaked: {log_text}"
    );
    assert!(
        !log_text.contains("secret-nbox-token-12345"),
        "token leaked: {log_text}"
    );
}

// === read path unchanged: interface read still works with the new subcommand ==

#[tokio::test]
async fn interface_read_still_works_with_no_action() {
    // The `interface` command gained an optional `set` subcommand; omitting it
    // must keep the read path byte-identical. Mount the interface detail + its
    // addresses/trace (the read view) and assert a normal read result.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 7, "url": "u", "name": "edge01"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/interfaces/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 42, "url": format!("{}/api/dcim/interfaces/42/", server.uri()),
                "name": "xe-0/0/1", "description": "uplink-old",
                "last_updated": "2026-06-26T10:00:00Z"
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/interfaces/42/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 42, "url": "u", "name": "xe-0/0/1", "description": "uplink-old",
            "last_updated": "2026-06-26T10:00:00Z"
        })))
        .mount(&server)
        .await;
    // The read view fans out to IPs + trace; mount empty pages so they resolve.
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/interfaces/42/trace/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "interface".as_ref(),
        "edge01".as_ref(),
        "xe-0/0/1".as_ref(),
    ]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("xe-0/0/1"),
        "read output: {}",
        out.stdout
    );
    assert_eq!(patch_count(&server).await, 0, "a read never PATCHes");
}
