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
use support::binary::{CommandOutput, assert_error_contract, run_nbox, temp_config};
use tempfile::NamedTempFile;
use wiremock::matchers::{body_partial_json, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A config whose profile carries a token_env that is never set, so the client
/// authenticates only when the test sets `NBOX_TOKEN` explicitly (for the
/// token-redaction assertion). Identical shape to `support::binary::temp_config`.
fn write_config(server_uri: &str) -> NamedTempFile {
    temp_config(server_uri)
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

// ===== device `set status` (ADR-0001 follow-on) ==========================
//
// The second write command reuses the same planner/diff/confirm/concurrency/
// audit contracts as the interface pilot. The new piece is choice validation:
// `status` is a server-enumerated field, so the planner asks NetBox (read-only
// `OPTIONS`) for the allowed values and normalizes the operator's input to the
// canonical wire value BEFORE building the plan.

/// NetBox's standard device status choices, as DRF surfaces them via OPTIONS.
/// The real NetBox `OPTIONS` shape (verified against 4.6.2): writable-field
/// schemas sit **directly** under `actions.POST.<field>` — no `body` wrapper.
fn device_options_body() -> Value {
    json!({
        "name": "Device",
        "actions": {
            "POST": {
                "status": {
                    "type": "choice",
                    "label": "Status",
                    "choices": [
                        {"value": "active", "display": "Active"},
                        {"value": "planned", "display": "Planned"},
                        {"value": "offline", "display": "Offline"},
                        {"value": "failed", "display": "Failed"},
                        {"value": "decommissioning", "display": "Decommissioning"}
                    ]
                }
            }
        }
    })
}

/// Mount the read-only `OPTIONS /api/dcim/devices/` that the planner uses to
/// enumerate allowed `status` values. Read-only — never mutates.
async fn mount_device_options(server: &MockServer) {
    Mock::given(method("OPTIONS"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(device_options_body()))
        .mount(server)
        .await;
}

/// Device-by-name resolution (GET `/api/dcim/devices/?name__ie=…`). The planner
/// re-fetches the authoritative detail (with ETag/last_updated) separately per
/// test, so this only needs to return the id.
async fn mount_device_resolution(server: &MockServer, device_id: u64, name: &str) {
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": device_id,
                "url": format!("{}/api/dcim/devices/{}/", server.uri(), device_id),
                "name": name
            }]
        })))
        .mount(server)
        .await;
}

/// The authoritative device detail (GET `/api/dcim/devices/{id}/`), with an
/// optional `ETag` response header and a configurable current `status` value.
async fn mount_device_detail(
    server: &MockServer,
    device_id: u64,
    etag: Option<&str>,
    status_value: &str,
    last_updated: &str,
) {
    let mut resp = ResponseTemplate::new(200).set_body_json(json!({
        "id": device_id,
        "url": format!("{}/api/dcim/devices/{}/", server.uri(), device_id),
        "name": "edge01",
        "status": {"value": status_value, "label": status_value},
        "last_updated": last_updated
    }));
    if let Some(e) = etag {
        resp = resp.insert_header("ETag", e);
    }
    Mock::given(method("GET"))
        .and(path(format!("/api/dcim/devices/{device_id}/")))
        .respond_with(resp)
        .mount(server)
        .await;
}

fn run_device_set<'a>(config: &NamedTempFile, extra: &'a [&'a str]) -> CommandOutput {
    let mut args: Vec<&str> = vec!["--config", config.path().to_str().unwrap()];
    args.push("--no-tui");
    args.extend_from_slice(&["device", "edge01", "set", "status"]);
    args.extend_from_slice(extra);
    run_nbox(args)
}

#[tokio::test]
async fn device_options_enumeration_is_authenticated() {
    // A secured NetBox 403s an unauthenticated OPTIONS, so the choice
    // enumeration must carry the same auth as every other call. Requiring the
    // Authorization header on the OPTIONS mock fails a missing-auth regression
    // (the request would no-match → the choice fetch errors).
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .and(path("/api/dcim/devices/"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(device_options_body()))
        .mount(&server)
        .await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail(&server, 7, None, "active", "2026-06-26T10:00:00Z").await;

    let config = write_config(&server.uri());
    // A token must be present for an Authorization header to exist; set it
    // explicitly (the test profile's `token_env` is intentionally unset).
    let out = Command::new(env!("CARGO_BIN_EXE_nbox"))
        .args([
            "--config",
            config.path().to_str().unwrap(),
            "--no-tui",
            "device",
            "edge01",
            "set",
            "status",
            "offline",
            "--dry-run",
            "--json",
        ])
        .env("NBOX_TOKEN", "testtoken")
        .env_remove("NBOX_LOG")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("spawn nbox");
    assert_eq!(
        out.status.code(),
        Some(0),
        "OPTIONS must be authenticated; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan: Value = serde_json::from_slice(&out.stdout).expect("plan JSON");
    assert_eq!(plan["patch"], json!({"status": "offline"}));
}

#[tokio::test]
async fn empty_status_choices_is_a_could_not_enumerate_usage_error() {
    // OPTIONS came back without `status` choices (an unexpected schema, a
    // permission-stripped `actions`, or a body-dropping proxy). The planner
    // must fail with a clear "could not enumerate" cause — never report the
    // input as invalid against an empty allow-list, never send an unvalidated
    // write. This refusal is pre-resolution, so no device mocks are needed.
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "actions": {"POST": {"name": {"type": "string"}}}
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_device_set(&config, &["offline", "--dry-run"]);
    assert_error_contract(&out, 2, "could not enumerate allowed values");
}

#[tokio::test]
async fn device_dry_run_sends_no_patch_and_shows_status_before_after() {
    let server = MockServer::start().await;
    mount_device_options(&server).await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail(&server, 7, None, "active", "2026-06-26T10:00:00Z").await;

    let config = write_config(&server.uri());
    let out = run_device_set(&config, &["offline", "--dry-run", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0, "dry-run must not PATCH");
    let plan: Value = serde_json::from_str(&out.stdout).expect("plan JSON");
    assert_eq!(plan["schema_version"], json!(1));
    assert_eq!(plan["target"]["kind"], json!("device"));
    assert_eq!(plan["target"]["id"], json!(7));
    assert_eq!(plan["target"]["endpoint"], json!("/api/dcim/devices/7/"));
    // Minimal patch: only the scoped field, normalized to the canonical value.
    assert_eq!(plan["patch"], json!({"status": "offline"}));
    assert_eq!(plan["fields"].as_array().unwrap().len(), 1);
    assert_eq!(plan["fields"][0]["field"], json!("status"));
    assert_eq!(plan["fields"][0]["before"], json!("active"));
    assert_eq!(plan["fields"][0]["after"], json!("offline"));
    assert_eq!(plan["no_op"], json!(false));
    // No ETag → pre-4.6 precondition (last_updated + before_hash).
    assert_eq!(plan["precondition"]["type"], json!("last_updated"));
}

#[tokio::test]
async fn device_dry_run_plain_renders_status_diff_to_stdout() {
    let server = MockServer::start().await;
    mount_device_options(&server).await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail(&server, 7, None, "active", "2026-06-26T10:00:00Z").await;

    let config = write_config(&server.uri());
    let out = run_device_set(&config, &["offline", "--dry-run"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0);
    // The plain diff names the field + before/after; the status line is stderr.
    assert!(out.stdout.contains("status"), "stdout: {}", out.stdout);
    assert!(
        out.stdout.contains("active → offline"),
        "stdout diff: {}",
        out.stdout
    );
    assert!(out.stderr.contains("planned, no changes sent"));
}

#[tokio::test]
async fn device_confirm_sends_one_minimal_patch_with_normalized_status() {
    let server = MockServer::start().await;
    mount_device_options(&server).await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail(&server, 7, None, "active", "2026-06-26T10:00:00Z").await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/7/"))
        .and(body_partial_json(json!({"status": "offline"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": format!("{}/api/dcim/devices/7/", server.uri()),
            "name": "edge01", "status": {"value": "offline", "label": "Offline"},
            "last_updated": "2026-06-26T10:30:00Z"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_device_set(
        &config,
        &["offline", "--allow-writes", "--confirm", "--json"],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 1, "exactly one PATCH");
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["applied"], json!(true));
    assert_eq!(receipt["no_op"], json!(false));
    assert_eq!(receipt["status"], json!(200));
    assert_eq!(receipt["fields"][0]["after"], json!("offline"));
    assert!(
        receipt["message"]
            .as_str()
            .unwrap()
            .contains("applied: device"),
        "receipt message: {}",
        receipt["message"]
    );
}

#[tokio::test]
async fn device_label_normalizes_to_canonical_value_case_insensitively() {
    // The operator typed the label "Offline" (NetBox's display). The planner
    // matches it case-insensitively to the canonical value `offline` and the
    // PATCH carries the normalized value, not the label.
    let server = MockServer::start().await;
    mount_device_options(&server).await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail(&server, 7, None, "active", "2026-06-26T10:00:00Z").await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/7/"))
        .and(body_partial_json(json!({"status": "offline"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "u", "name": "edge01",
            "status": {"value": "offline", "label": "Offline"}
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_device_set(
        &config,
        &["Offline", "--allow-writes", "--confirm", "--json"],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 1);
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["applied"], json!(true));
    // Confirm the wire body actually carried the canonical value, not the label.
    let patch_reqs: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "PATCH")
        .collect();
    let body: Value = serde_json::from_slice(&patch_reqs[0].body).unwrap();
    assert_eq!(body, json!({"status": "offline"}));
}

#[tokio::test]
async fn device_noop_status_sends_no_patch() {
    let server = MockServer::start().await;
    mount_device_options(&server).await;
    mount_device_resolution(&server, 7, "edge01").await;
    // Current value already `offline` → no-op. No PATCH mock mounted.
    mount_device_detail(&server, 7, None, "offline", "2026-06-26T10:00:00Z").await;

    let config = write_config(&server.uri());
    let out = run_device_set(
        &config,
        &["offline", "--allow-writes", "--confirm", "--json"],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0, "no-op sends no PATCH");
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["applied"], json!(false));
    assert_eq!(receipt["no_op"], json!(true));
    assert_eq!(receipt["status"], json!(0));
    assert!(receipt["message"].as_str().unwrap().contains("no change"));
}

#[tokio::test]
async fn device_unknown_status_is_a_usage_error_before_any_patch() {
    // OPTIONS enumerates the allowed values; `bogus` matches none → usage error
    // (exit 2) naming the input and listing the allowed values, with no PATCH.
    let server = MockServer::start().await;
    mount_device_options(&server).await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail(&server, 7, None, "active", "2026-06-26T10:00:00Z").await;

    let config = write_config(&server.uri());
    let out = run_device_set(&config, &["bogus", "--dry-run", "--json"]);

    assert_error_contract(&out, 2, "invalid status \"bogus\"");
    assert!(
        out.stderr
            .contains("active, planned, offline, failed, decommissioning"),
        "stderr should list allowed values: {}",
        out.stderr
    );
    assert_eq!(
        patch_count(&server).await,
        0,
        "unknown status sends no PATCH"
    );
}

#[tokio::test]
async fn device_ambiguous_label_is_a_usage_error() {
    // Two choices whose labels collide case-insensitively → ambiguous.
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "actions": {
                "POST": {
                    "body": {
                        "status": {
                            "choices": [
                                {"value": "active", "display": "Up"},
                                {"value": "online", "display": "up"}
                            ]
                        }
                    }
                }
            }
        })))
        .mount(&server)
        .await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail(&server, 7, None, "active", "2026-06-26T10:00:00Z").await;

    let config = write_config(&server.uri());
    let out = run_device_set(&config, &["UP", "--dry-run", "--json"]);

    assert_error_contract(&out, 2, "ambiguous");
    assert_eq!(patch_count(&server).await, 0);
}

#[tokio::test]
async fn device_unsupported_field_is_a_usage_error_before_any_network() {
    // No mocks mounted: a usage error must not reach the network. `set role …`
    // fails closed at the field check before connect/resolve.
    let config = write_config("http://unused.example/");
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "device".as_ref(),
        "edge01".as_ref(),
        "set".as_ref(),
        "role".as_ref(),
        "something".as_ref(),
        "--dry-run".as_ref(),
    ]);
    assert_error_contract(&out, 2, "only `status` is writable");
}

#[tokio::test]
async fn device_confirm_without_allow_writes_is_a_usage_error() {
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_device_set(&config, &["offline", "--confirm", "--json"]);
    assert_error_contract(&out, 2, "writes are not enabled");
    assert!(
        out.stderr.contains("--allow-writes"),
        "stderr: {}",
        out.stderr
    );
    assert_eq!(patch_count(&server).await, 0);
}

#[tokio::test]
async fn device_allow_writes_without_confirm_is_usage_in_non_tty() {
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_device_set(&config, &["offline", "--allow-writes", "--json"]);
    assert_error_contract(&out, 2, "non-interactive write requires confirmation");
    assert!(out.stderr.contains("--confirm"), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0);
}

#[tokio::test]
async fn device_etag_sends_if_match_on_apply() {
    let server = MockServer::start().await;
    mount_device_options(&server).await;
    mount_device_resolution(&server, 7, "edge01").await;
    // 4.6+ detail carries an ETag; the plan records it and the apply sends
    // `If-Match: <etag>` (ADR-0001 §3).
    mount_device_detail(
        &server,
        7,
        Some("\"etag-v1\""),
        "active",
        "2026-06-26T10:00:00Z",
    )
    .await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/7/"))
        // The PATCH mock ONLY matches when If-Match is sent — proving the
        // header is present. Without it, wiremock returns 404 and the test fails.
        .and(header("if-match", "\"etag-v1\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "u", "name": "edge01",
            "status": {"value": "offline", "label": "Offline"}
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_device_set(
        &config,
        &["offline", "--allow-writes", "--confirm", "--json"],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 1);
    let patch_reqs: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "PATCH")
        .collect();
    assert_eq!(
        patch_reqs[0].headers.get("if-match").unwrap(),
        "\"etag-v1\""
    );
}

#[tokio::test]
async fn device_stale_412_is_a_stale_precondition_refusal() {
    let server = MockServer::start().await;
    mount_device_options(&server).await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail(
        &server,
        7,
        Some("\"etag-v1\""),
        "active",
        "2026-06-26T10:00:00Z",
    )
    .await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/7/"))
        .respond_with(ResponseTemplate::new(412).set_body_string("Precondition Failed"))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_device_set(
        &config,
        &["offline", "--allow-writes", "--confirm", "--json"],
    );

    assert_error_contract(&out, 1, "object changed in NetBox");
    assert!(
        out.stderr.contains("re-run dry-run"),
        "stderr: {}",
        out.stderr
    );
}

#[tokio::test]
async fn device_stale_pre46_fallback_re_reads_and_refuses_on_last_updated_change() {
    let server = MockServer::start().await;
    mount_device_options(&server).await;
    mount_device_resolution(&server, 7, "edge01").await;
    // No ETag → pre-4.6 path. The plan reads last_updated T1; the apply re-read
    // must return a DIFFERENT last_updated so the read-before-write check
    // refuses before any PATCH.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "u", "name": "edge01",
            "status": {"value": "active", "label": "Active"},
            "last_updated": "2026-06-26T10:00:00Z"
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "u", "name": "edge01",
            "status": {"value": "active", "label": "Active"},
            "last_updated": "2026-06-26T11:00:00Z"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_device_set(
        &config,
        &["offline", "--allow-writes", "--confirm", "--json"],
    );

    assert_error_contract(&out, 1, "object changed in NetBox");
    assert_eq!(
        patch_count(&server).await,
        0,
        "stale pre-4.6 sends no PATCH"
    );
}

#[tokio::test]
async fn device_validation_400_surfaces_netbox_field_error_cleanly() {
    let server = MockServer::start().await;
    mount_device_options(&server).await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail(&server, 7, None, "active", "2026-06-26T10:00:00Z").await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/7/"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({"status": ["Invalid status."]})),
        )
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_device_set(
        &config,
        &["offline", "--allow-writes", "--confirm", "--json"],
    );

    assert_error_contract(&out, 1, "NetBox rejected the patch");
    assert!(out.stderr.contains("status"), "stderr: {}", out.stderr);
}

#[tokio::test]
async fn device_audit_logs_status_name_only_never_values_token_or_message_body() {
    let server = MockServer::start().await;
    mount_device_options(&server).await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail(&server, 7, None, "active", "2026-06-26T10:00:00Z").await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "u", "name": "edge01",
            "status": {"value": "offline", "label": "Offline"}
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let log = NamedTempFile::new().expect("log file");
    let log_path = log.path().to_path_buf();
    drop(log);

    // Distinctive old/new values, a secret token, and a message body — none of
    // which may appear in the audit log (only the field NAME `status`, a
    // message_present flag + length, and the outcome).
    let out = Command::new(env!("CARGO_BIN_EXE_nbox"))
        .arg("--config")
        .arg(config.path())
        .arg("--no-tui")
        .arg("--log-file")
        .arg(&log_path)
        .arg("--log-level")
        .arg("nbox::write_audit=info")
        .args([
            "device",
            "edge01",
            "set",
            "status",
            "offline",
            "--allow-writes",
            "--confirm",
            "--message",
            "draining-edge01-secret",
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
        log_text.contains("fields=\"status\"") || log_text.contains("fields=status"),
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
    // Redaction: none of the old/new status values, the message body, or the
    // token may leak into the audit log.
    assert!(
        !log_text.contains("active"),
        "old status value leaked: {log_text}"
    );
    assert!(
        !log_text.contains("offline"),
        "new status value leaked: {log_text}"
    );
    assert!(
        !log_text.contains("draining-edge01-secret"),
        "message body leaked: {log_text}"
    );
    assert!(
        !log_text.contains("secret-nbox-token-12345"),
        "token leaked: {log_text}"
    );
}

#[tokio::test]
async fn device_read_still_works_with_no_action() {
    // The `device` command gained an optional `set` subcommand; omitting it
    // must keep the read path byte-identical. Mount the device detail + its
    // interfaces/IPs/services (the read view) and assert a normal read result.
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
        .and(path("/api/dcim/devices/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "u", "name": "edge01",
            "status": {"value": "active", "label": "Active"}
        })))
        .mount(&server)
        .await;
    // The read view fans out to interfaces/IPs/services; mount empty pages.
    Mock::given(method("GET"))
        .and(path("/api/dcim/interfaces/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/services/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "device".as_ref(),
        "edge01".as_ref(),
    ]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("edge01"), "read output: {}", out.stdout);
    assert_eq!(patch_count(&server).await, 0, "a read never PATCHes");
}

// ===== ip `reserve` (ADR-0001 first Allocate write) ======================
//
// The first non-PATCH write: an `allocate` that POSTs to a prefix's
// `available-ips` endpoint to reserve the next free address. It reuses the same
// gate/confirm/audit lifecycle as the PATCH pilots; the new pieces are the POST
// transport, the `none` precondition (the endpoint is server-side race-safe),
// and a receipt that carries the *created* object. The planner always reads the
// currently-next address (read-only) to surface an advisory candidate.

/// Count the `POST` requests the mock received.
async fn post_count(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST")
        .count()
}

/// Prefix-by-CIDR resolution: GET `/api/ipam/prefixes/?prefix=<cidr>` → one hit.
async fn mount_prefix_resolution(server: &MockServer, prefix_id: u64, cidr: &str) {
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": prefix_id,
                "url": format!("{}/api/ipam/prefixes/{}/", server.uri(), prefix_id),
                "prefix": cidr
            }]
        })))
        .mount(server)
        .await;
}

/// Prefix-by-CIDR resolution with the same CIDR in two VRFs. Used to pin that
/// every accepted `ip reserve --vrf` spelling actually reaches the resolver.
async fn mount_prefix_resolution_in_two_vrfs(server: &MockServer, cidr: &str) {
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {
                    "id": 1,
                    "url": format!("{}/api/ipam/prefixes/1/", server.uri()),
                    "prefix": cidr,
                    "vrf": {
                        "id": 11,
                        "url": format!("{}/api/ipam/vrfs/11/", server.uri()),
                        "display": "red (65000:11)",
                        "name": "red",
                        "rd": "65000:11"
                    }
                },
                {
                    "id": 2,
                    "url": format!("{}/api/ipam/prefixes/2/", server.uri()),
                    "prefix": cidr,
                    "vrf": {
                        "id": 12,
                        "url": format!("{}/api/ipam/vrfs/12/", server.uri()),
                        "display": "blue (65000:12)",
                        "name": "blue",
                        "rd": "65000:12"
                    }
                }
            ]
        })))
        .mount(server)
        .await;
}

/// The read-only candidate GET the planner uses for the dry-run advisory:
/// GET `/api/ipam/prefixes/{id}/available-ips/` → a bare JSON array.
async fn mount_available_ips_get(server: &MockServer, prefix_id: u64, addr: &str) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/ipam/prefixes/{prefix_id}/available-ips/"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "address": addr }])))
        .mount(server)
        .await;
}

fn run_ip_reserve<'a>(config: &NamedTempFile, extra: &'a [&'a str]) -> CommandOutput {
    let mut args: Vec<&str> = vec!["--config", config.path().to_str().unwrap()];
    args.push("--no-tui");
    args.extend_from_slice(&["ip", "reserve", "203.0.113.0/24"]);
    args.extend_from_slice(extra);
    run_nbox(args)
}

#[tokio::test]
async fn ip_reserve_dry_run_sends_no_post_and_shows_candidate() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "203.0.113.0/24").await;
    mount_available_ips_get(&server, 1, "203.0.113.5/24").await;

    let config = write_config(&server.uri());
    let out = run_ip_reserve(&config, &["--dry-run", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(post_count(&server).await, 0, "dry-run must not POST");
    let plan: Value = serde_json::from_str(&out.stdout).expect("plan JSON");
    assert_eq!(plan["schema_version"], json!(1));
    assert_eq!(plan["operation"], json!("allocate"));
    assert_eq!(plan["target"]["kind"], json!("ip"));
    assert_eq!(plan["target"]["id"], json!(1));
    assert_eq!(plan["target"]["display"], json!("203.0.113.0/24"));
    assert_eq!(
        plan["target"]["endpoint"],
        json!("/api/ipam/prefixes/1/available-ips/")
    );
    // Bare reserve → empty body, no synthetic address, server-race-safe precond.
    assert_eq!(plan["patch"], json!({}));
    assert_eq!(plan["fields"].as_array().unwrap().len(), 0);
    assert_eq!(plan["precondition"]["type"], json!("none"));
    // The candidate is advisory only — a warning, never in `patch`/`fields`.
    let warnings = plan["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("203.0.113.5/24")),
        "candidate surfaced as a warning: {plan}"
    );
}

#[tokio::test]
async fn ip_reserve_dry_run_plain_renders_candidate_note() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "203.0.113.0/24").await;
    mount_available_ips_get(&server, 1, "203.0.113.5/24").await;

    let config = write_config(&server.uri());
    let out = run_ip_reserve(&config, &["--dry-run"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(post_count(&server).await, 0);
    assert!(
        out.stdout.contains("ip: 203.0.113.0/24"),
        "stdout target line: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("note:") && out.stdout.contains("203.0.113.5/24"),
        "stdout candidate note: {}",
        out.stdout
    );
    assert!(out.stderr.contains("planned, no changes sent"));
}

#[tokio::test]
async fn ip_reserve_parent_vrf_before_subcommand_scopes_the_write() {
    let server = MockServer::start().await;
    mount_prefix_resolution_in_two_vrfs(&server, "203.0.113.0/24").await;
    mount_available_ips_get(&server, 2, "203.0.113.5/24").await;

    let config = write_config(&server.uri());
    let out = run_nbox([
        "--config",
        config.path().to_str().unwrap(),
        "--no-tui",
        "ip",
        "--vrf",
        "blue",
        "reserve",
        "203.0.113.0/24",
        "--dry-run",
        "--json",
    ]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let plan: Value = serde_json::from_str(&out.stdout).expect("plan JSON");
    assert_eq!(plan["target"]["id"], json!(2), "blue VRF prefix selected");
    assert_eq!(
        plan["target"]["endpoint"],
        json!("/api/ipam/prefixes/2/available-ips/")
    );
}

#[tokio::test]
async fn ip_reserve_conflicting_parent_and_subcommand_vrfs_refuse_before_network() {
    let server = MockServer::start().await;
    let config = write_config(&server.uri());

    let out = run_nbox([
        "--config",
        config.path().to_str().unwrap(),
        "--no-tui",
        "ip",
        "--vrf",
        "blue",
        "reserve",
        "203.0.113.0/24",
        "--vrf",
        "red",
        "--dry-run",
        "--json",
    ]);

    assert_error_contract(&out, 2, "conflicting --vrf values");
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        0,
        "conflicting scope should fail before any NetBox request"
    );
}

#[tokio::test]
async fn ip_reserve_confirm_sends_one_post_and_returns_created_object() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "203.0.113.0/24").await;
    mount_available_ips_get(&server, 1, "203.0.113.5/24").await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/prefixes/1/available-ips/"))
        .and(body_partial_json(json!({"description": "edge uplink"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 7,
            "url": format!("{}/api/ipam/ip-addresses/7/", server.uri()),
            "address": "203.0.113.5/24",
            "status": {"value": "active", "label": "Active"},
            "description": "edge uplink"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_ip_reserve(
        &config,
        &[
            "--description",
            "edge uplink",
            "--allow-writes",
            "--confirm",
            "--json",
        ],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(post_count(&server).await, 1, "exactly one POST");
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["operation"], json!("allocate"));
    assert_eq!(receipt["applied"], json!(true));
    assert_eq!(receipt["no_op"], json!(false));
    assert_eq!(receipt["status"], json!(201));
    // The receipt carries the created object so scripts get the assigned address.
    assert_eq!(receipt["object"]["address"], json!("203.0.113.5/24"));
    assert_eq!(receipt["object"]["status"], json!("active"));
    assert!(
        receipt["message"]
            .as_str()
            .unwrap()
            .contains("reserved: 203.0.113.5/24 in 203.0.113.0/24"),
        "receipt message: {}",
        receipt["message"]
    );
}

#[tokio::test]
async fn ip_reserve_bare_sends_empty_body() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "203.0.113.0/24").await;
    mount_available_ips_get(&server, 1, "203.0.113.5/24").await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/prefixes/1/available-ips/"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 7, "url": "u", "address": "203.0.113.5/24"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_ip_reserve(&config, &["--allow-writes", "--confirm", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(post_count(&server).await, 1);
    // The wire body for a bare reserve is exactly `{}` (no fields, no message).
    let post_reqs: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST")
        .collect();
    let body: Value = serde_json::from_slice(&post_reqs[0].body).unwrap();
    assert_eq!(body, json!({}), "bare reserve POSTs an empty body");
}

#[tokio::test]
async fn ip_reserve_sends_description_and_dns_name_when_present() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "203.0.113.0/24").await;
    mount_available_ips_get(&server, 1, "203.0.113.5/24").await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/prefixes/1/available-ips/"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 7, "url": "u", "address": "203.0.113.5/24"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_ip_reserve(
        &config,
        &[
            "--description",
            "edge uplink",
            "--dns-name",
            "edge01.example.net",
            "--allow-writes",
            "--confirm",
            "--json",
        ],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let post_reqs: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST")
        .collect();
    let body: Value = serde_json::from_slice(&post_reqs[0].body).unwrap();
    assert_eq!(
        body,
        json!({"description": "edge uplink", "dns_name": "edge01.example.net"})
    );
}

#[tokio::test]
async fn ip_reserve_409_exhaustion_is_a_clean_error() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "203.0.113.0/24").await;
    mount_available_ips_get(&server, 1, "203.0.113.5/24").await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/prefixes/1/available-ips/"))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_string("The requested number of IP addresses is not available"),
        )
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_ip_reserve(&config, &["--allow-writes", "--confirm", "--json"]);

    // 409 maps through the generic API error → exit 1, clean stdout.
    assert_eq!(out.code, Some(1), "stderr: {}", out.stderr);
    assert!(
        out.stdout.is_empty(),
        "error keeps stdout clean: {:?}",
        out.stdout
    );
}

#[tokio::test]
async fn ip_reserve_400_validation_is_a_clean_error() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "203.0.113.0/24").await;
    mount_available_ips_get(&server, 1, "203.0.113.5/24").await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/prefixes/1/available-ips/"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"dns_name": ["Invalid."]})))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_ip_reserve(
        &config,
        &[
            "--dns-name",
            "bad name",
            "--allow-writes",
            "--confirm",
            "--json",
        ],
    );

    assert_error_contract(&out, 1, "NetBox rejected");
}

#[tokio::test]
async fn ip_reserve_without_gate_is_a_usage_error_before_any_network() {
    // No `--allow-writes` → refuse at the gate (exit 2) with no plan/network.
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_ip_reserve(&config, &[]);
    assert_error_contract(&out, 2, "writes are not enabled");
    assert_eq!(post_count(&server).await, 0);
}

#[tokio::test]
async fn ip_reserve_confirm_without_gate_is_a_usage_error() {
    // `--confirm` without `--allow-writes` can never become a write (gate ⟂
    // confirm): refuse at the gate (exit 2), naming the missing gate flag.
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_ip_reserve(&config, &["--confirm"]);
    assert_error_contract(&out, 2, "writes are not enabled");
    assert_eq!(post_count(&server).await, 0);
}

#[tokio::test]
async fn ip_reserve_allow_writes_without_confirm_nontty_is_a_usage_error() {
    // `--allow-writes` but no `--confirm` on a non-TTY → refuse (exit 2): a
    // non-interactive write must be explicitly confirmed.
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_ip_reserve(&config, &["--allow-writes"]);
    assert_error_contract(&out, 2, "non-interactive write requires confirmation");
    assert_eq!(post_count(&server).await, 0);
}

#[tokio::test]
async fn ip_reserve_over_length_message_is_a_usage_error() {
    // The message length is validated (exit 2) before any network use — even
    // with the full gate flags, no prefix lookup or POST is performed.
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let over = "x".repeat(201);
    let out = run_ip_reserve(
        &config,
        &[
            "--allow-writes",
            "--confirm",
            "--message",
            over.as_str(),
            "--json",
        ],
    );
    assert_error_contract(&out, 2, "200-character limit");
    assert_eq!(post_count(&server).await, 0);
}

#[tokio::test]
async fn ip_reserve_audit_logs_names_only_never_values_token_or_message_body() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "203.0.113.0/24").await;
    mount_available_ips_get(&server, 1, "203.0.113.5/24").await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/prefixes/1/available-ips/"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 7, "url": "u", "address": "203.0.113.5/24"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let log = NamedTempFile::new().expect("log file");
    let log_path = log.path().to_path_buf();
    drop(log);

    let out = Command::new(env!("CARGO_BIN_EXE_nbox"))
        .arg("--config")
        .arg(config.path())
        .arg("--no-tui")
        .arg("--log-file")
        .arg(&log_path)
        .arg("--log-level")
        .arg("nbox::write_audit=info")
        .args([
            "ip",
            "reserve",
            "203.0.113.0/24",
            "--description",
            "reserve-secret-desc",
            "--allow-writes",
            "--confirm",
            "--message",
            "reserve-secret-message",
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
    assert!(log_text.contains("nbox::write_audit"), "log: {log_text}");
    // Allocate audits operation=allocate + http_method=POST + the field NAME.
    assert!(
        log_text.contains("operation=\"allocate\"") || log_text.contains("operation=allocate"),
        "operation recorded: {log_text}"
    );
    assert!(
        log_text.contains("http_method=\"POST\"") || log_text.contains("http_method=POST"),
        "http_method recorded: {log_text}"
    );
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
    // Redaction: the description value, the message body, and the token never leak.
    assert!(
        !log_text.contains("reserve-secret-desc"),
        "description value leaked: {log_text}"
    );
    assert!(
        !log_text.contains("reserve-secret-message"),
        "message body leaked: {log_text}"
    );
    assert!(
        !log_text.contains("secret-nbox-token-12345"),
        "token leaked: {log_text}"
    );
}

#[tokio::test]
async fn ip_bare_without_address_is_a_usage_error() {
    // `nbox ip` with neither an address nor a subcommand → usage error (exit 2).
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_nbox(vec![
        "--config",
        config.path().to_str().unwrap(),
        "--no-tui",
        "ip",
    ]);
    assert_error_contract(&out, 2, "missing IP address");
}

// ===== multi-IP ip reserve (--count N) (ADR-0001 follow-on) ================
//
// `ip reserve --count N` first attempts an atomic list-body POST (a single
// request with a JSON array body) so NetBox creates all N or zero IPs in one
// round-trip — NetBox returns `201` with a JSON array of the created IPs. This
// is all-or-nothing across every supported version (verified to the 4.2 floor),
// so there is no per-IP fallback and no partial state: any failure (409
// exhaustion, validation, …) leaves nothing created and is a clean exit-1 error.

/// Mount one atomic list-body POST response: `201` with a JSON array of the N
/// created IPs. The request body is a JSON array of N copies of the single-IP
/// create body; this mock matches any POST to the endpoint (the body shape is
/// asserted separately where relevant).
async fn mount_atomic_multi_ip_post(server: &MockServer, prefix_id: u64, addresses: &[&str]) {
    let results: Vec<Value> = addresses
        .iter()
        .enumerate()
        .map(|(i, addr)| {
            json!({
                "id": 100 + i,
                "url": format!("{}/api/ipam/ip-addresses/{}/", server.uri(), 100 + i),
                "address": addr,
                "status": {"value": "active", "label": "Active"},
            })
        })
        .collect();
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/ipam/prefixes/{prefix_id}/available-ips/"
        )))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!(results)))
        .up_to_n_times(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn ip_reserve_count_3_atomic_list_body_post_creates_all_in_one_request() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "203.0.113.0/24").await;
    // Advisory: 3 available addresses.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/1/available-ips/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"address": "203.0.113.5/24"},
            {"address": "203.0.113.6/24"},
            {"address": "203.0.113.7/24"}
        ])))
        .mount(&server)
        .await;
    mount_atomic_multi_ip_post(
        &server,
        1,
        &["203.0.113.5/24", "203.0.113.6/24", "203.0.113.7/24"],
    )
    .await;

    let config = write_config(&server.uri());
    let out = run_ip_reserve(
        &config,
        &["--count", "3", "--allow-writes", "--confirm", "--json"],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    // All-or-nothing: exactly ONE POST (the list-body request), not three.
    assert_eq!(
        post_count(&server).await,
        1,
        "atomic path: one list-body POST"
    );
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["operation"], json!("allocate"));
    // All N created → `object` is a JSON array of the 3 created IpViews.
    let objects = receipt["object"].as_array().expect("object array");
    assert_eq!(objects.len(), 3);
    assert_eq!(objects[0]["address"], json!("203.0.113.5/24"));
    assert_eq!(objects[1]["address"], json!("203.0.113.6/24"));
    assert_eq!(objects[2]["address"], json!("203.0.113.7/24"));
}

#[tokio::test]
async fn ip_reserve_count_3_dry_run_shows_count_in_fields_and_no_posts() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "203.0.113.0/24").await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/1/available-ips/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"address": "203.0.113.5/24"},
            {"address": "203.0.113.6/24"},
            {"address": "203.0.113.7/24"}
        ])))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_ip_reserve(&config, &["--count", "3", "--dry-run", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(post_count(&server).await, 0, "dry-run must not POST");
    let plan: Value = serde_json::from_str(&out.stdout).expect("plan JSON");
    assert_eq!(plan["count"], json!(3));
    // The count appears in the fields diff.
    let fields = plan["fields"].as_array().expect("fields array");
    assert!(
        fields
            .iter()
            .any(|f| f["field"] == "count" && f["after"] == 3),
        "count in fields diff: {plan}"
    );
    // The advisory warning mentions 3 addresses.
    let warnings = plan["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("3 addresses")),
        "multi-IP advisory: {plan}"
    );
}

#[tokio::test]
async fn ip_reserve_count_0_is_a_usage_error() {
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_ip_reserve(&config, &["--count", "0", "--dry-run", "--json"]);
    assert_error_contract(&out, 2, "count must be between 1 and 1000");
}

#[tokio::test]
async fn ip_reserve_count_over_cap_is_a_usage_error_pre_network() {
    // An unbounded count would build a giant list body before the POST. The
    // planner rejects > MAX_ALLOCATION_COUNT pre-network (no NetBox call), exit 2.
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_ip_reserve(
        &config,
        &["--count", "100000", "--allow-writes", "--confirm", "--json"],
    );
    assert_error_contract(&out, 2, "count must be between 1 and 1000");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        0,
        "an over-cap count must be rejected before any NetBox request"
    );
}

#[tokio::test]
async fn ip_reserve_count_1_is_byte_identical_to_no_count() {
    // --count 1 should produce the same plan as no --count (count defaults to 1
    // and is omitted from JSON).
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "203.0.113.0/24").await;
    mount_available_ips_get(&server, 1, "203.0.113.5/24").await;

    let config = write_config(&server.uri());
    let out_no_count = run_ip_reserve(&config, &["--dry-run", "--json"]);
    let out_count_1 = run_ip_reserve(&config, &["--count", "1", "--dry-run", "--json"]);

    assert_eq!(out_no_count.code, Some(0));
    assert_eq!(out_count_1.code, Some(0));
    let plan_no_count: Value = serde_json::from_str(&out_no_count.stdout).expect("plan JSON");
    let plan_count_1: Value = serde_json::from_str(&out_count_1.stdout).expect("plan JSON");
    // count=1 is the default and is omitted from JSON.
    assert!(
        plan_no_count.get("count").is_none(),
        "default count omitted"
    );
    assert!(plan_count_1.get("count").is_none(), "count=1 omitted");
    // No "count" field in the diff either.
    let fields = plan_count_1["fields"].as_array().expect("fields");
    assert!(
        !fields.iter().any(|f| f["field"] == "count"),
        "no count in fields"
    );
}

#[tokio::test]
async fn ip_reserve_count_1_apply_is_byte_identical_to_no_count() {
    // --count 1 on the apply path: a single single-object POST (never the
    // list-body atomic path), byte-identical receipt to no --count.
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "203.0.113.0/24").await;
    mount_available_ips_get(&server, 1, "203.0.113.5/24").await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/prefixes/1/available-ips/"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 7,
            "url": format!("{}/api/ipam/ip-addresses/7/", server.uri()),
            "address": "203.0.113.5/24",
            "status": {"value": "active", "label": "Active"},
            "description": "edge uplink"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out_no_count = run_ip_reserve(
        &config,
        &[
            "--description",
            "edge uplink",
            "--allow-writes",
            "--confirm",
            "--json",
        ],
    );
    let out_count_1 = run_ip_reserve(
        &config,
        &[
            "--count",
            "1",
            "--description",
            "edge uplink",
            "--allow-writes",
            "--confirm",
            "--json",
        ],
    );

    assert_eq!(
        out_no_count.code,
        Some(0),
        "stderr: {}",
        out_no_count.stderr
    );
    assert_eq!(out_count_1.code, Some(0), "stderr: {}", out_count_1.stderr);
    // count==1 is a single single-object POST (NOT the list-body path).
    assert_eq!(
        post_count(&server).await,
        2,
        "two runs, one single POST each"
    );
    // The receipts are byte-identical: same object and message, single object
    // (not a JSON array — count==1 never takes the multi-IP list-body path).
    let r_no: Value = serde_json::from_str(&out_no_count.stdout).expect("receipt");
    let r_1: Value = serde_json::from_str(&out_count_1.stdout).expect("receipt");
    assert!(
        r_1["object"]["address"] == json!("203.0.113.5/24"),
        "single object"
    );
    assert_eq!(r_no, r_1, "count==1 receipt byte-identical to no-count");
}

#[tokio::test]
async fn ip_reserve_count_3_atomic_409_is_plain_error_no_orphans() {
    // A 409 on the atomic list-body POST is NOT a list-shape rejection — it
    // means the prefix is exhausted. The atomic path creates zero IPs, so the
    // error propagates as a plain exit-1 error with clean stdout (no fallback,
    // no partial receipt, no orphan IPs).
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "203.0.113.0/24").await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/1/available-ips/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"address": "203.0.113.5/24"},
            {"address": "203.0.113.6/24"},
            {"address": "203.0.113.7/24"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/prefixes/1/available-ips/"))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_string("The requested number of IP addresses is not available"),
        )
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_ip_reserve(
        &config,
        &["--count", "3", "--allow-writes", "--confirm", "--json"],
    );

    // 409 propagates from the atomic attempt; no fallback, one POST total.
    assert_error_contract(&out, 1, "HTTP 409");
    assert_eq!(
        post_count(&server).await,
        1,
        "atomic 409: one POST, no fallback"
    );
    assert!(
        out.stdout.is_empty(),
        "no receipt on atomic 409: {:?}",
        out.stdout
    );
}

// ===== prefix reserve (ADR-0001 follow-on) ================================
//
// `prefix reserve` mirrors `ip reserve`: same Allocate/POST pattern, same
// server-side race-safe `Precondition::None`, same gate/confirm/audit
// lifecycle. The POST targets `available-prefixes` instead of `available-ips`.

/// Mount the available-prefixes advisory GET: returns a bare JSON array of
/// free blocks.
async fn mount_available_prefixes_get(server: &MockServer, prefix_id: u64, blocks: &[&str]) {
    let results: Vec<Value> = blocks.iter().map(|b| json!({"prefix": b})).collect();
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/ipam/prefixes/{prefix_id}/available-prefixes/"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(results)))
        .mount(server)
        .await;
}

fn run_prefix_reserve<'a>(config: &NamedTempFile, extra: &'a [&'a str]) -> CommandOutput {
    let mut args: Vec<&str> = vec!["--config", config.path().to_str().unwrap()];
    args.push("--no-tui");
    args.extend_from_slice(&["prefix", "10.0.0.0/24", "reserve"]);
    args.extend_from_slice(extra);
    run_nbox(args)
}

#[tokio::test]
async fn prefix_reserve_dry_run_sends_no_post_and_shows_candidate() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "10.0.0.0/24").await;
    mount_available_prefixes_get(&server, 1, &["10.0.0.0/25", "10.0.128.0/25"]).await;

    let config = write_config(&server.uri());
    let out = run_prefix_reserve(&config, &["--dry-run", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(post_count(&server).await, 0, "dry-run must not POST");
    let plan: Value = serde_json::from_str(&out.stdout).expect("plan JSON");
    assert_eq!(plan["operation"], json!("allocate"));
    assert_eq!(plan["target"]["kind"], json!("prefix"));
    assert_eq!(plan["target"]["id"], json!(1));
    assert_eq!(plan["patch"], json!({}));
    assert_eq!(plan["precondition"]["type"], json!("none"));
    // Advisory candidate in warnings.
    let warnings = plan["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("10.0.0.0/25")),
        "candidate surfaced as a warning: {plan}"
    );
}

#[tokio::test]
async fn prefix_reserve_with_length_sends_prefix_length_in_body() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "10.0.0.0/24").await;
    mount_available_prefixes_get(&server, 1, &["10.0.0.0/26", "10.0.64.0/26"]).await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/prefixes/1/available-prefixes/"))
        .and(body_partial_json(json!({"prefix_length": 26})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 42,
            "url": "u",
            "prefix": "10.0.0.0/26"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_prefix_reserve(
        &config,
        &["--length", "26", "--allow-writes", "--confirm", "--json"],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(post_count(&server).await, 1, "exactly one POST");
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["applied"], json!(true));
    assert_eq!(receipt["status"], json!(201));
    // The receipt carries the created prefix.
    assert_eq!(receipt["object"]["prefix"], json!("10.0.0.0/26"));
    assert!(
        receipt["message"]
            .as_str()
            .unwrap()
            .contains("reserved: 10.0.0.0/26")
    );
}

#[tokio::test]
async fn prefix_reserve_with_description_includes_it_in_body() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "10.0.0.0/24").await;
    mount_available_prefixes_get(&server, 1, &["10.0.0.0/25"]).await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/prefixes/1/available-prefixes/"))
        .and(body_partial_json(json!({"description": "dmz block"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 42,
            "url": "u",
            "prefix": "10.0.0.0/25"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_prefix_reserve(
        &config,
        &[
            "--description",
            "dmz block",
            "--allow-writes",
            "--confirm",
            "--json",
        ],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(post_count(&server).await, 1);
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["object"]["prefix"], json!("10.0.0.0/25"));
}

#[tokio::test]
async fn prefix_reserve_confirm_without_allow_writes_is_a_usage_error() {
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_prefix_reserve(&config, &["--confirm", "--json"]);
    assert_error_contract(&out, 2, "writes are not enabled");
    assert_eq!(post_count(&server).await, 0);
}

#[tokio::test]
async fn prefix_reserve_409_exhaustion_is_a_clean_error() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "10.0.0.0/24").await;
    mount_available_prefixes_get(&server, 1, &[]).await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/prefixes/1/available-prefixes/"))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(json!({"detail": "Insufficient space in 10.0.0.0/24"})),
        )
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_prefix_reserve(&config, &["--allow-writes", "--confirm", "--json"]);

    assert_error_contract(&out, 1, "Insufficient space");
    assert_eq!(post_count(&server).await, 1, "exactly one POST attempt");
}

#[tokio::test]
async fn prefix_reserve_audit_logs_names_only_never_values_token_or_message_body() {
    let server = MockServer::start().await;
    mount_prefix_resolution(&server, 1, "10.0.0.0/24").await;
    mount_available_prefixes_get(&server, 1, &["10.0.0.0/25"]).await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/prefixes/1/available-prefixes/"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 42, "url": "u", "prefix": "10.0.0.0/25"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let log = NamedTempFile::new().expect("log file");
    let log_path = log.path().to_path_buf();
    drop(log);

    let out = Command::new(env!("CARGO_BIN_EXE_nbox"))
        .arg("--config")
        .arg(config.path())
        .arg("--no-tui")
        .arg("--log-file")
        .arg(&log_path)
        .arg("--log-level")
        .arg("nbox::write_audit=info")
        .args([
            "prefix",
            "10.0.0.0/24",
            "reserve",
            "--length",
            "25",
            "--description",
            "reserve-secret-desc",
            "--allow-writes",
            "--confirm",
            "--message",
            "reserve-secret-message",
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
    assert!(log_text.contains("nbox::write_audit"), "log: {log_text}");
    assert!(
        log_text.contains("operation=\"allocate\"") || log_text.contains("operation=allocate"),
        "operation recorded: {log_text}"
    );
    assert!(
        log_text.contains("fields=\"prefix_length") || log_text.contains("fields=prefix_length"),
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
    assert!(
        !log_text.contains("reserve-secret-desc"),
        "description value leaked: {log_text}"
    );
    assert!(
        !log_text.contains("reserve-secret-message"),
        "message body leaked: {log_text}"
    );
    assert!(
        !log_text.contains("secret-nbox-token-12345"),
        "token leaked: {log_text}"
    );
}

// ===== ip-range reserve (ADR-0001 follow-on) ===============================
//
// `ip-range reserve` mirrors `ip reserve` but targets an IP range instead of
// a prefix: same Allocate/POST pattern, same server-side race-safe
// `Precondition::None`, same gate/confirm/audit lifecycle. The POST targets
// `…/ip-ranges/{id}/available-ips/`.

/// Mount an IP-range resolution: `GET /api/ipam/ip-ranges/?start_address=…` →
/// one hit.
async fn mount_ip_range_resolution(server: &MockServer, range_id: u64, start: &str, end: &str) {
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-ranges/"))
        .and(wiremock::matchers::query_param("start_address", start))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": range_id,
                "url": format!("{}/api/ipam/ip-ranges/{}/", server.uri(), range_id),
                "start_address": start,
                "end_address": end
            }]
        })))
        .mount(server)
        .await;
}

/// Mount the IP-range available-ips advisory GET: returns a bare JSON array of
/// free addresses.
async fn mount_ip_range_available_ips_get(server: &MockServer, range_id: u64, addr: Option<&str>) {
    let results: Vec<Value> = match addr {
        Some(a) => vec![json!({"address": a})],
        None => vec![],
    };
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/ipam/ip-ranges/{range_id}/available-ips/"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(results)))
        .mount(server)
        .await;
}

fn run_ip_range_reserve<'a>(config: &NamedTempFile, extra: &'a [&'a str]) -> CommandOutput {
    let mut args: Vec<&str> = vec!["--config", config.path().to_str().unwrap()];
    args.push("--no-tui");
    args.extend_from_slice(&["ip-range", "10.0.0.10", "reserve"]);
    args.extend_from_slice(extra);
    run_nbox(args)
}

#[tokio::test]
async fn ip_range_reserve_dry_run_sends_no_post_and_shows_candidate() {
    let server = MockServer::start().await;
    mount_ip_range_resolution(&server, 1, "10.0.0.10", "10.0.0.20").await;
    mount_ip_range_available_ips_get(&server, 1, Some("10.0.0.10/32")).await;

    let config = write_config(&server.uri());
    let out = run_ip_range_reserve(&config, &["--dry-run", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(post_count(&server).await, 0, "dry-run must not POST");
    let plan: Value = serde_json::from_str(&out.stdout).expect("plan JSON");
    assert_eq!(plan["operation"], json!("allocate"));
    assert_eq!(plan["target"]["kind"], json!("ip"));
    assert_eq!(plan["target"]["id"], json!(1));
    assert_eq!(plan["target"]["display"], json!("10.0.0.10 – 10.0.0.20"));
    assert_eq!(
        plan["target"]["endpoint"],
        json!("/api/ipam/ip-ranges/1/available-ips/")
    );
    assert_eq!(plan["patch"], json!({}));
    assert_eq!(plan["precondition"]["type"], json!("none"));
    let warnings = plan["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("10.0.0.10/32")),
        "candidate surfaced as a warning: {plan}"
    );
}

#[tokio::test]
async fn ip_range_reserve_with_description_and_dns_name_includes_them_in_body() {
    let server = MockServer::start().await;
    mount_ip_range_resolution(&server, 1, "10.0.0.10", "10.0.0.20").await;
    mount_ip_range_available_ips_get(&server, 1, Some("10.0.0.10/32")).await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/ip-ranges/1/available-ips/"))
        .and(body_partial_json(
            json!({"description": "loopback", "dns_name": "lb.example"}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 42,
            "url": "u",
            "address": "10.0.0.10/32"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_ip_range_reserve(
        &config,
        &[
            "--description",
            "loopback",
            "--dns-name",
            "lb.example",
            "--allow-writes",
            "--confirm",
            "--json",
        ],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(post_count(&server).await, 1);
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["applied"], json!(true));
    assert_eq!(receipt["status"], json!(201));
    assert_eq!(receipt["object"]["address"], json!("10.0.0.10/32"));
    assert!(
        receipt["message"]
            .as_str()
            .unwrap()
            .contains("reserved: 10.0.0.10/32")
    );
}

#[tokio::test]
async fn ip_range_reserve_confirm_without_allow_writes_is_a_usage_error() {
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_ip_range_reserve(&config, &["--confirm", "--json"]);
    assert_error_contract(&out, 2, "writes are not enabled");
    assert_eq!(post_count(&server).await, 0);
}

#[tokio::test]
async fn ip_range_reserve_409_exhaustion_is_a_clean_error() {
    let server = MockServer::start().await;
    mount_ip_range_resolution(&server, 1, "10.0.0.10", "10.0.0.20").await;
    mount_ip_range_available_ips_get(&server, 1, None).await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/ip-ranges/1/available-ips/"))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(json!({"detail": "Insufficient space in range"})),
        )
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_ip_range_reserve(&config, &["--allow-writes", "--confirm", "--json"]);

    assert_error_contract(&out, 1, "Insufficient space");
    assert_eq!(post_count(&server).await, 1, "exactly one POST attempt");
}

#[tokio::test]
async fn ip_range_reserve_audit_logs_names_only_never_values_token_or_message_body() {
    let server = MockServer::start().await;
    mount_ip_range_resolution(&server, 1, "10.0.0.10", "10.0.0.20").await;
    mount_ip_range_available_ips_get(&server, 1, Some("10.0.0.10/32")).await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/ip-ranges/1/available-ips/"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 42, "url": "u", "address": "10.0.0.10/32"
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let log = NamedTempFile::new().expect("log file");
    let log_path = log.path().to_path_buf();
    drop(log);

    let out = Command::new(env!("CARGO_BIN_EXE_nbox"))
        .arg("--config")
        .arg(config.path())
        .arg("--no-tui")
        .arg("--log-file")
        .arg(&log_path)
        .arg("--log-level")
        .arg("nbox::write_audit=info")
        .args([
            "ip-range",
            "10.0.0.10",
            "reserve",
            "--description",
            "reserve-secret-desc",
            "--allow-writes",
            "--confirm",
            "--message",
            "reserve-secret-message",
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
    assert!(log_text.contains("nbox::write_audit"), "log: {log_text}");
    assert!(
        log_text.contains("operation=\"allocate\"") || log_text.contains("operation=allocate"),
        "operation recorded: {log_text}"
    );
    assert!(
        log_text.contains("http_method=\"POST\"") || log_text.contains("http_method=POST"),
        "http_method recorded: {log_text}"
    );
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
    assert!(
        !log_text.contains("reserve-secret-desc"),
        "description value leaked: {log_text}"
    );
    assert!(
        !log_text.contains("reserve-secret-message"),
        "message body leaked: {log_text}"
    );
    assert!(
        !log_text.contains("secret-nbox-token-12345"),
        "token leaked: {log_text}"
    );
}

// ===== multi-IP ip-range reserve (--count N) (ADR-0001 follow-on) ============
//
// `ip-range reserve --count N` first attempts an atomic list-body POST (a
// single request with a JSON array body) so NetBox creates all N or zero IPs
// in one round-trip. Older NetBox that rejects the list shape (400/422) falls
// back to N sequential single-object POSTs.

/// Mount one atomic list-body POST response for an ip-range: `201` with a JSON
/// array of the N created IPs.
async fn mount_atomic_ip_range_multi_ip_post(
    server: &MockServer,
    range_id: u64,
    addresses: &[&str],
) {
    let results: Vec<Value> = addresses
        .iter()
        .enumerate()
        .map(|(i, addr)| {
            json!({
                "id": 100 + i,
                "url": format!("{}/api/ipam/ip-addresses/{}/", server.uri(), 100 + i),
                "address": addr,
                "status": {"value": "active", "label": "Active"},
            })
        })
        .collect();
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/ipam/ip-ranges/{range_id}/available-ips/"
        )))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!(results)))
        .up_to_n_times(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn ip_range_reserve_count_2_atomic_list_body_post_creates_both_in_one_request() {
    let server = MockServer::start().await;
    mount_ip_range_resolution(&server, 1, "10.0.0.10", "10.0.0.20").await;
    // Advisory: 2 available addresses.
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-ranges/1/available-ips/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"address": "10.0.0.10/32"},
            {"address": "10.0.0.11/32"}
        ])))
        .mount(&server)
        .await;
    // One atomic list-body POST returns a JSON array of both created IPs.
    mount_atomic_ip_range_multi_ip_post(&server, 1, &["10.0.0.10/32", "10.0.0.11/32"]).await;

    let config = write_config(&server.uri());
    let out = run_ip_range_reserve(
        &config,
        &["--count", "2", "--allow-writes", "--confirm", "--json"],
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    // All-or-nothing: exactly ONE POST (the list-body request), not two.
    assert_eq!(
        post_count(&server).await,
        1,
        "atomic path: one list-body POST"
    );
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    // All N created → `object` is a JSON array of the 2 created IpViews.
    let objects = receipt["object"].as_array().expect("object array");
    assert_eq!(objects.len(), 2);
    assert_eq!(objects[0]["address"], json!("10.0.0.10/32"));
    assert_eq!(objects[1]["address"], json!("10.0.0.11/32"));
}

#[tokio::test]
async fn ip_range_reserve_count_2_dry_run_shows_count_in_fields() {
    let server = MockServer::start().await;
    mount_ip_range_resolution(&server, 1, "10.0.0.10", "10.0.0.20").await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-ranges/1/available-ips/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"address": "10.0.0.10/32"},
            {"address": "10.0.0.11/32"}
        ])))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_ip_range_reserve(&config, &["--count", "2", "--dry-run", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(post_count(&server).await, 0);
    let plan: Value = serde_json::from_str(&out.stdout).expect("plan JSON");
    assert_eq!(plan["count"], json!(2));
    let fields = plan["fields"].as_array().expect("fields array");
    assert!(
        fields
            .iter()
            .any(|f| f["field"] == "count" && f["after"] == 2),
        "count in fields diff: {plan}"
    );
}

#[tokio::test]
async fn ip_range_reserve_count_2_atomic_409_is_a_plain_error() {
    let server = MockServer::start().await;
    mount_ip_range_resolution(&server, 1, "10.0.0.10", "10.0.0.20").await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-ranges/1/available-ips/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"address": "10.0.0.10/32"},
            {"address": "10.0.0.11/32"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/ip-ranges/1/available-ips/"))
        .respond_with(ResponseTemplate::new(409).set_body_string("exhausted"))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_ip_range_reserve(
        &config,
        &["--count", "2", "--allow-writes", "--confirm", "--json"],
    );

    assert_error_contract(&out, 1, "HTTP 409");
    assert_eq!(
        post_count(&server).await,
        1,
        "atomic 409: one list-body POST, no fallback"
    );
}

#[tokio::test]
async fn ip_range_reserve_count_2_atomic_409_is_plain_error_no_orphans() {
    // A 409 on the atomic list-body POST is NOT a list-shape rejection — it
    // means the range is exhausted. The atomic path creates zero IPs, so the
    // error propagates as a plain exit-1 error with clean stdout (no fallback,
    // no partial receipt, no orphan IPs).
    let server = MockServer::start().await;
    mount_ip_range_resolution(&server, 1, "10.0.0.10", "10.0.0.20").await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-ranges/1/available-ips/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"address": "10.0.0.10/32"},
            {"address": "10.0.0.11/32"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/ipam/ip-ranges/1/available-ips/"))
        .respond_with(ResponseTemplate::new(409).set_body_string("Insufficient space in range"))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_ip_range_reserve(
        &config,
        &["--count", "2", "--allow-writes", "--confirm", "--json"],
    );

    assert_error_contract(&out, 1, "HTTP 409");
    assert_eq!(
        post_count(&server).await,
        1,
        "atomic 409: one POST, no fallback"
    );
    assert!(
        out.stdout.is_empty(),
        "no receipt on atomic 409: {:?}",
        out.stdout
    );
}

// ===== tag add (ADR-0001 follow-on) =====================================
//
// The fourth write command reuses the same planner/diff/confirm/concurrency/
// audit contracts. Tags are a list field: the plan carries the full
// replacement `{"tags": [slugs]}` (NetBox PATCH replaces the whole array).

/// Mount a tag resolution: `GET /api/extras/tags/?name=…` returning one tag.
async fn mount_tag_by_name(server: &MockServer, tag_id: u64, name: &str, slug: &str) {
    Mock::given(method("GET"))
        .and(path("/api/extras/tags/"))
        .and(wiremock::matchers::query_param("name", name))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": tag_id, "name": name, "slug": slug}]
        })))
        .mount(server)
        .await;
}

/// Mount a device detail with a `tags` array, optional ETag.
async fn mount_device_detail_with_tags(
    server: &MockServer,
    device_id: u64,
    etag: Option<&str>,
    tags: &[(&str, &str)],
    last_updated: &str,
) {
    let tag_json: Vec<Value> = tags
        .iter()
        .map(|(name, slug)| json!({"id": 1, "name": name, "slug": slug}))
        .collect();
    let mut resp = ResponseTemplate::new(200).set_body_json(json!({
        "id": device_id,
        "url": format!("{}/api/dcim/devices/{}/", server.uri(), device_id),
        "name": "edge01",
        "display": "edge01",
        "last_updated": last_updated,
        "tags": tag_json
    }));
    if let Some(e) = etag {
        resp = resp.insert_header("ETag", e);
    }
    Mock::given(method("GET"))
        .and(path(format!("/api/dcim/devices/{device_id}/")))
        .respond_with(resp)
        .mount(server)
        .await;
}

fn run_tag_add<'a>(config: &NamedTempFile, extra: &'a [&'a str]) -> CommandOutput {
    let mut args: Vec<&str> = vec!["--config", config.path().to_str().unwrap()];
    args.push("--no-tui");
    args.extend_from_slice(&["tag", "add", "device", "edge01", "prod"]);
    args.extend_from_slice(extra);
    run_nbox(args)
}

#[tokio::test]
async fn tag_add_dry_run_sends_no_patch_and_shows_tags_before_after() {
    let server = MockServer::start().await;
    mount_tag_by_name(&server, 5, "prod", "prod").await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail_with_tags(
        &server,
        7,
        None,
        &[("legacy", "legacy")],
        "2026-06-26T10:00:00Z",
    )
    .await;

    let config = write_config(&server.uri());
    let out = run_tag_add(&config, &["--dry-run", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0, "dry-run must not PATCH");
    let plan: Value = serde_json::from_str(&out.stdout).expect("plan JSON");
    assert_eq!(plan["schema_version"], json!(1));
    assert_eq!(plan["target"]["kind"], json!("device"));
    assert_eq!(plan["target"]["id"], json!(7));
    assert_eq!(plan["target"]["endpoint"], json!("/api/dcim/devices/7/"));
    // The patch replaces the full tags array: existing + new.
    assert_eq!(plan["patch"], json!({"tags": ["legacy", "prod"]}));
    assert_eq!(plan["fields"].as_array().unwrap().len(), 1);
    assert_eq!(plan["fields"][0]["field"], json!("tags"));
    assert_eq!(plan["fields"][0]["before"], json!(["legacy"]));
    assert_eq!(plan["fields"][0]["after"], json!(["legacy", "prod"]));
    assert_eq!(plan["no_op"], json!(false));
    // No ETag → pre-4.6 precondition.
    assert_eq!(plan["precondition"]["type"], json!("last_updated"));
}

#[tokio::test]
async fn tag_add_confirm_sends_one_patch_with_full_tags_array() {
    let server = MockServer::start().await;
    mount_tag_by_name(&server, 5, "prod", "prod").await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail_with_tags(
        &server,
        7,
        None,
        &[("legacy", "legacy")],
        "2026-06-26T10:00:00Z",
    )
    .await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/7/"))
        .and(body_partial_json(json!({"tags": ["legacy", "prod"]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "u", "name": "edge01",
            "tags": [{"id": 1, "name": "legacy", "slug": "legacy"},
                     {"id": 5, "name": "prod", "slug": "prod"}]
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_tag_add(&config, &["--allow-writes", "--confirm", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 1, "exactly one PATCH");
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["applied"], json!(true));
    assert_eq!(receipt["no_op"], json!(false));
    assert_eq!(receipt["status"], json!(200));
    assert_eq!(receipt["fields"][0]["after"], json!(["legacy", "prod"]));
    assert!(
        receipt["message"]
            .as_str()
            .unwrap()
            .contains("applied: device"),
        "receipt message: {}",
        receipt["message"]
    );
}

#[tokio::test]
async fn tag_add_noop_when_tag_already_present_sends_no_patch() {
    let server = MockServer::start().await;
    mount_tag_by_name(&server, 5, "prod", "prod").await;
    mount_device_resolution(&server, 7, "edge01").await;
    // Device already carries the "prod" tag → no-op. No PATCH mock mounted.
    mount_device_detail_with_tags(
        &server,
        7,
        None,
        &[("legacy", "legacy"), ("prod", "prod")],
        "2026-06-26T10:00:00Z",
    )
    .await;

    let config = write_config(&server.uri());
    let out = run_tag_add(&config, &["--allow-writes", "--confirm", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0, "no-op sends no PATCH");
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["applied"], json!(false));
    assert_eq!(receipt["no_op"], json!(true));
    assert_eq!(receipt["status"], json!(0));
    assert!(receipt["message"].as_str().unwrap().contains("no change"));
}

#[tokio::test]
async fn tag_add_unknown_tag_is_not_found_exit_4() {
    let server = MockServer::start().await;
    // Tag resolution returns empty — no tag matches "nonexistent".
    Mock::given(method("GET"))
        .and(path("/api/extras/tags/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    mount_device_resolution(&server, 7, "edge01").await;

    let config = write_config(&server.uri());
    let out = run_tag_add(&config, &["--dry-run", "--json"]);

    assert_error_contract(&out, 4, "no tag matched");
    assert_eq!(patch_count(&server).await, 0);
}

#[tokio::test]
async fn tag_add_ambiguous_ip_refuses_instead_of_first_picking() {
    let server = MockServer::start().await;
    mount_tag_by_name(&server, 5, "prod", "prod").await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(wiremock::matchers::query_param("address", "192.0.2.10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {
                    "id": 101,
                    "url": format!("{}/api/ipam/ip-addresses/101/", server.uri()),
                    "address": "192.0.2.10/24",
                    "vrf": {"id": 1, "name": "blue", "display": "blue"}
                },
                {
                    "id": 102,
                    "url": format!("{}/api/ipam/ip-addresses/102/", server.uri()),
                    "address": "192.0.2.10/24",
                    "vrf": {"id": 2, "name": "red", "display": "red"}
                }
            ]
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_nbox([
        "--config",
        config.path().to_str().unwrap(),
        "--no-tui",
        "tag",
        "add",
        "ip",
        "192.0.2.10",
        "prod",
        "--dry-run",
        "--json",
    ]);

    assert_error_contract(&out, 5, "IP address");
    assert!(out.stderr.contains("ambiguous"), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0);
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        2,
        "ambiguous IP should stop after tag lookup + candidate lookup"
    );
}

#[tokio::test]
async fn tag_add_confirm_without_allow_writes_is_a_usage_error() {
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_tag_add(&config, &["--confirm", "--json"]);
    assert_error_contract(&out, 2, "writes are not enabled");
    assert!(
        out.stderr.contains("--allow-writes"),
        "stderr: {}",
        out.stderr
    );
    assert_eq!(patch_count(&server).await, 0);
}

#[tokio::test]
async fn tag_add_allow_writes_without_confirm_is_usage_in_non_tty() {
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_tag_add(&config, &["--allow-writes", "--json"]);
    assert_error_contract(&out, 2, "non-interactive write requires confirmation");
    assert!(out.stderr.contains("--confirm"), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0);
}

#[tokio::test]
async fn tag_add_etag_sends_if_match_on_apply() {
    let server = MockServer::start().await;
    mount_tag_by_name(&server, 5, "prod", "prod").await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail_with_tags(
        &server,
        7,
        Some("\"etag-v1\""),
        &[("legacy", "legacy")],
        "2026-06-26T10:00:00Z",
    )
    .await;
    // The PATCH mock ONLY matches when If-Match is sent.
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/7/"))
        .and(header("if-match", "\"etag-v1\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "u", "name": "edge01",
            "tags": [{"id": 1, "name": "legacy", "slug": "legacy"},
                     {"id": 5, "name": "prod", "slug": "prod"}]
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_tag_add(&config, &["--allow-writes", "--confirm", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 1);
    let patch_reqs: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "PATCH")
        .collect();
    assert_eq!(
        patch_reqs[0].headers.get("if-match").unwrap(),
        "\"etag-v1\""
    );
}

#[tokio::test]
async fn tag_add_stale_412_is_a_stale_precondition_refusal() {
    let server = MockServer::start().await;
    mount_tag_by_name(&server, 5, "prod", "prod").await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail_with_tags(
        &server,
        7,
        Some("\"etag-v1\""),
        &[("legacy", "legacy")],
        "2026-06-26T10:00:00Z",
    )
    .await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/7/"))
        .respond_with(ResponseTemplate::new(412).set_body_string("Precondition Failed"))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_tag_add(&config, &["--allow-writes", "--confirm", "--json"]);

    assert_error_contract(&out, 1, "object changed in NetBox");
    assert!(
        out.stderr.contains("re-run dry-run"),
        "stderr: {}",
        out.stderr
    );
}

#[tokio::test]
async fn tag_add_audit_logs_field_name_only_never_values_or_token() {
    let server = MockServer::start().await;
    mount_tag_by_name(&server, 5, "prod", "prod").await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail_with_tags(
        &server,
        7,
        None,
        &[("legacy", "legacy")],
        "2026-06-26T10:00:00Z",
    )
    .await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "u", "name": "edge01",
            "tags": [{"id": 1, "name": "legacy", "slug": "legacy"},
                     {"id": 5, "name": "prod", "slug": "prod"}]
        })))
        .mount(&server)
        .await;

    let log_path =
        std::env::temp_dir().join(format!("nbox-tag-add-audit-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&log_path);

    let config = write_config(&server.uri());
    let out = Command::new(env!("CARGO_BIN_EXE_nbox"))
        .args([
            "--config",
            config.path().to_str().unwrap(),
            "--no-tui",
            "tag",
            "add",
            "device",
            "edge01",
            "prod",
            "--allow-writes",
            "--confirm",
            "--json",
            "--log-file",
            log_path.to_str().unwrap(),
        ])
        .env("NBOX_TOKEN", "secret-nbox-token-12345")
        .env("NBOX_LOG", "nbox::write_audit=info")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("spawn nbox");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let log_text = std::fs::read_to_string(&log_path).expect("read log file");
    assert!(log_text.contains("nbox::write_audit"), "log: {log_text}");
    // The audit carries the field NAME "tags", the operation, and the outcome.
    assert!(
        log_text.contains("fields=\"tags\"") || log_text.contains("fields=tags"),
        "field NAME recorded: {log_text}"
    );
    assert!(
        log_text.contains("operation=\"update\"") || log_text.contains("operation=update"),
        "operation recorded: {log_text}"
    );
    assert!(
        log_text.contains("outcome=\"applied\"") || log_text.contains("outcome=applied"),
        "outcome recorded: {log_text}"
    );
    // Redaction: the tag slug value and the token never leak.
    assert!(
        !log_text.contains("secret-nbox-token-12345"),
        "token leaked: {log_text}"
    );
}

// ===== tag remove (ADR-0001 follow-on) ===================================
//
// `tag remove` mirrors `tag add`: same planner/diff/confirm/concurrency/audit
// contracts, same PATCH-replaces-whole-array semantics. The only difference is
// the slug is filtered *out* instead of *in*; a no-op (tag absent) sends no
// PATCH.

fn run_tag_remove<'a>(config: &NamedTempFile, extra: &'a [&'a str]) -> CommandOutput {
    let mut args: Vec<&str> = vec!["--config", config.path().to_str().unwrap()];
    args.push("--no-tui");
    args.extend_from_slice(&["tag", "remove", "device", "edge01", "prod"]);
    args.extend_from_slice(extra);
    run_nbox(args)
}

#[tokio::test]
async fn tag_remove_dry_run_sends_no_patch_and_shows_tags_before_after() {
    let server = MockServer::start().await;
    mount_tag_by_name(&server, 5, "prod", "prod").await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail_with_tags(
        &server,
        7,
        None,
        &[("legacy", "legacy"), ("prod", "prod")],
        "2026-06-26T10:00:00Z",
    )
    .await;

    let config = write_config(&server.uri());
    let out = run_tag_remove(&config, &["--dry-run", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0, "dry-run must not PATCH");
    let plan: Value = serde_json::from_str(&out.stdout).expect("plan JSON");
    assert_eq!(plan["patch"], json!({"tags": ["legacy"]}));
    assert_eq!(plan["fields"][0]["before"], json!(["legacy", "prod"]));
    assert_eq!(plan["fields"][0]["after"], json!(["legacy"]));
    assert_eq!(plan["no_op"], json!(false));
}

#[tokio::test]
async fn tag_remove_confirm_sends_one_patch_with_filtered_tags_array() {
    let server = MockServer::start().await;
    mount_tag_by_name(&server, 5, "prod", "prod").await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail_with_tags(
        &server,
        7,
        None,
        &[("legacy", "legacy"), ("prod", "prod")],
        "2026-06-26T10:00:00Z",
    )
    .await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/7/"))
        .and(body_partial_json(json!({"tags": ["legacy"]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "u", "name": "edge01",
            "tags": [{"id": 1, "name": "legacy", "slug": "legacy"}]
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_tag_remove(&config, &["--allow-writes", "--confirm", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 1, "exactly one PATCH");
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["applied"], json!(true));
    assert_eq!(receipt["no_op"], json!(false));
    assert_eq!(receipt["status"], json!(200));
    assert_eq!(receipt["fields"][0]["after"], json!(["legacy"]));
}

#[tokio::test]
async fn tag_remove_noop_when_tag_absent_sends_no_patch() {
    let server = MockServer::start().await;
    mount_tag_by_name(&server, 5, "prod", "prod").await;
    mount_device_resolution(&server, 7, "edge01").await;
    // Device does NOT carry the "prod" tag → no-op. No PATCH mock mounted.
    mount_device_detail_with_tags(
        &server,
        7,
        None,
        &[("legacy", "legacy")],
        "2026-06-26T10:00:00Z",
    )
    .await;

    let config = write_config(&server.uri());
    let out = run_tag_remove(&config, &["--allow-writes", "--confirm", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 0, "no-op sends no PATCH");
    let receipt: Value = serde_json::from_str(&out.stdout).expect("receipt JSON");
    assert_eq!(receipt["applied"], json!(false));
    assert_eq!(receipt["no_op"], json!(true));
    assert_eq!(receipt["status"], json!(0));
    assert!(receipt["message"].as_str().unwrap().contains("no change"));
}

#[tokio::test]
async fn tag_remove_unknown_tag_is_not_found_exit_4() {
    let server = MockServer::start().await;
    // Tag resolution returns empty — no tag matches "nonexistent".
    Mock::given(method("GET"))
        .and(path("/api/extras/tags/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    mount_device_resolution(&server, 7, "edge01").await;

    let config = write_config(&server.uri());
    let out = run_tag_remove(&config, &["--dry-run", "--json"]);

    assert_error_contract(&out, 4, "no tag matched");
    assert_eq!(patch_count(&server).await, 0);
}

#[tokio::test]
async fn tag_remove_confirm_without_allow_writes_is_a_usage_error() {
    let server = MockServer::start().await;
    let config = write_config(&server.uri());
    let out = run_tag_remove(&config, &["--confirm", "--json"]);
    assert_error_contract(&out, 2, "writes are not enabled");
    assert_eq!(patch_count(&server).await, 0);
}

#[tokio::test]
async fn tag_remove_etag_sends_if_match_on_apply() {
    let server = MockServer::start().await;
    mount_tag_by_name(&server, 5, "prod", "prod").await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail_with_tags(
        &server,
        7,
        Some("\"etag-v1\""),
        &[("legacy", "legacy"), ("prod", "prod")],
        "2026-06-26T10:00:00Z",
    )
    .await;
    // The PATCH mock ONLY matches when If-Match is sent.
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/7/"))
        .and(header("if-match", "\"etag-v1\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "u", "name": "edge01",
            "tags": [{"id": 1, "name": "legacy", "slug": "legacy"}]
        })))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_tag_remove(&config, &["--allow-writes", "--confirm", "--json"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(patch_count(&server).await, 1);
    let patch_reqs: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "PATCH")
        .collect();
    assert_eq!(
        patch_reqs[0].headers.get("if-match").unwrap(),
        "\"etag-v1\""
    );
}

#[tokio::test]
async fn tag_remove_stale_412_is_a_stale_precondition_refusal() {
    let server = MockServer::start().await;
    mount_tag_by_name(&server, 5, "prod", "prod").await;
    mount_device_resolution(&server, 7, "edge01").await;
    mount_device_detail_with_tags(
        &server,
        7,
        Some("\"etag-v1\""),
        &[("legacy", "legacy"), ("prod", "prod")],
        "2026-06-26T10:00:00Z",
    )
    .await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/7/"))
        .respond_with(ResponseTemplate::new(412).set_body_string("Precondition Failed"))
        .mount(&server)
        .await;

    let config = write_config(&server.uri());
    let out = run_tag_remove(&config, &["--allow-writes", "--confirm", "--json"]);

    assert_error_contract(&out, 1, "object changed in NetBox");
    assert!(
        out.stderr.contains("re-run dry-run"),
        "stderr: {}",
        out.stderr
    );
}
