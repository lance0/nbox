//! Binary-level error contracts.
//!
//! These drive the real compiled `nbox` binary so the public contract is pinned
//! at the process boundary: stable exit code, clean stdout, and actionable
//! stderr. Lower-level tests still cover typed error propagation in detail.

use serde_json::json;
use support::binary::{
    CommandOutput, assert_error_contract, assert_json_stdout, assert_success, run_nbox, temp_config,
};
use support::netbox::page;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

fn run_device(config: &tempfile::NamedTempFile, value: &str) -> CommandOutput {
    run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "device".as_ref(),
        value.as_ref(),
    ])
}

#[test]
fn usage_error_exits_2_and_keeps_stdout_clean() {
    let output = run_nbox(["--no-tui"]);

    assert_error_contract(&output, 2, "no command given");
    assert!(
        output.stderr.contains("--no-tui"),
        "stderr should explain the guard: {:?}",
        output.stderr
    );
}

#[tokio::test]
async fn authentication_error_exits_3_and_keeps_stdout_clean() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .mount(&server)
        .await;

    let config = temp_config(&server.uri());
    let output = run_device(&config, "edge01");

    assert_error_contract(&output, 3, "authentication failed");
}

#[tokio::test]
async fn not_found_error_exits_4_and_keeps_stdout_clean() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("name__ie", "missing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(Vec::new())))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("name__ic", "missing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(Vec::new())))
        .mount(&server)
        .await;

    let config = temp_config(&server.uri());
    let output = run_device(&config, "missing");

    assert_error_contract(&output, 4, "no device matched \"missing\"");
}

#[tokio::test]
async fn ambiguous_error_exits_5_and_keeps_stdout_clean() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("name__ie", "edge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(Vec::new())))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("name__ic", "edge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({
                "id": 7,
                "url": "http://nb/api/dcim/devices/7/",
                "name": "edge01"
            }),
            json!({
                "id": 8,
                "url": "http://nb/api/dcim/devices/8/",
                "name": "edge02"
            }),
        ])))
        .mount(&server)
        .await;

    let config = temp_config(&server.uri());
    let output = run_device(&config, "edge");

    assert_error_contract(&output, 5, "device \"edge\" is ambiguous");
    assert!(
        output.stderr.contains("edge01") && output.stderr.contains("edge02"),
        "stderr should list candidates: {:?}",
        output.stderr
    );
}

#[tokio::test]
async fn generic_api_error_exits_1_and_keeps_stdout_clean() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let config = temp_config(&server.uri());
    let output = run_device(&config, "edge01");

    assert_error_contract(&output, 1, "NetBox API request failed");
}

/// Run `nbox mac <value>` against a mock NetBox at `config`.
fn run_mac(config: &tempfile::NamedTempFile, value: &str) -> CommandOutput {
    run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "mac".as_ref(),
        value.as_ref(),
    ])
}

#[tokio::test]
async fn mac_invalid_input_exits_2_without_a_netbox_round_trip() {
    // A non-MAC is a usage error (exit 2), normalized locally — no request is
    // sent. (No mock is mounted, so a round trip would 404 and exit 1 here.)
    let config = temp_config("http://unused.example/");
    let output = run_mac(&config, "not-a-mac");
    assert_error_contract(&output, 2, "invalid MAC address");
}

#[tokio::test]
async fn mac_not_found_exits_4_and_keeps_stdout_clean() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/mac-addresses/"))
        .and(query_param("mac_address", "aa:bb:cc:dd:ee:ff"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(Vec::new())))
        .mount(&server)
        .await;

    let config = temp_config(&server.uri());
    // Any accepted form normalizes to the same canonical MAC before the request.
    let output = run_mac(&config, "AABB.CCDD.EEFF");
    assert_error_contract(&output, 4, "no MAC matched");
}

#[tokio::test]
async fn mac_ambiguous_exits_5_and_lists_the_interfaces() {
    // The same MAC on two interfaces → ambiguous (exit 5), naming both so the
    // operator can disambiguate.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/mac-addresses/"))
        .and(query_param("mac_address", "aa:bb:cc:dd:ee:ff"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 7, "url": "u", "mac_address": "aa:bb:cc:dd:ee:ff",
                  "assigned_object": {"display": "xe-0/0/1", "device": {"display": "edge01"}}}),
            json!({"id": 8, "url": "u", "mac_address": "aa:bb:cc:dd:ee:ff",
                  "assigned_object": {"display": "xe-0/0/2", "device": {"display": "edge01"}}}),
        ])))
        .mount(&server)
        .await;

    let config = temp_config(&server.uri());
    let output = run_mac(&config, "aa:bb:cc:dd:ee:ff");
    assert_error_contract(&output, 5, "MAC \"aa:bb:cc:dd:ee:ff\" is ambiguous");
    assert!(
        output.stderr.contains("edge01 xe-0/0/1") && output.stderr.contains("edge01 xe-0/0/2"),
        "stderr should name the carrying interfaces: {:?}",
        output.stderr
    );
}

/// Contract test for the `tests/support/binary` harness itself: prove the
/// three canonical helpers accept the shapes they promise (`impl AsRef<str>`
/// for the error substring, a success `CommandOutput` for `assert_success`, and
/// valid JSON stdout for `assert_json_stdout`) and panic on the wrong shape.
/// These build `CommandOutput` directly (no binary spawn, no network) so the
/// harness is pinned independently of any CLI behavior change.
#[test]
fn harness_helpers_accept_the_documented_shapes() {
    // `assert_error_contract`: a `&str` (the common call-site shape) satisfies
    // `impl AsRef<str>`, and a `String` does too.
    let err = CommandOutput {
        code: Some(2),
        stdout: String::new(),
        stderr: "no command given: pass --help".into(),
    };
    assert_error_contract(&err, 2, "no command given");
    assert_error_contract(&err, 2, String::from("no command given"));

    // `assert_success`: exit 0 with a non-empty stderr (warnings allowed on a
    // success path) is accepted — only the code is constrained.
    let ok = CommandOutput {
        code: Some(0),
        stdout: String::new(),
        stderr: "warning: stale cache".into(),
    };
    assert_success(&ok);

    // `assert_json_stdout`: exit 0 with valid JSON parses and returns the Value.
    let json_out = CommandOutput {
        code: Some(0),
        stdout: r#"{"schema_version":1,"applied":true}"#.into(),
        stderr: String::new(),
    };
    let value = assert_json_stdout(&json_out);
    assert_eq!(value["schema_version"], json!(1));
    assert_eq!(value["applied"], json!(true));
}

/// The error-contract helper rejects a polluted stdout on the error path (the
/// whole point of the contract — errors never touch the data stream).
#[test]
#[should_panic(expected = "error paths must keep stdout clean")]
fn harness_error_contract_rejects_polluted_stdout() {
    let err = CommandOutput {
        code: Some(2),
        stdout: "leaked data".into(),
        stderr: "no command given".into(),
    };
    assert_error_contract(&err, 2, "no command given");
}

/// `assert_json_stdout` panics (not a silent `unwrap`) when stdout is not valid
/// JSON, so a malformed-data regression is loud.
#[test]
#[should_panic(expected = "stdout is not valid JSON")]
fn harness_json_stdout_panics_on_non_json() {
    let bad = CommandOutput {
        code: Some(0),
        stdout: "not json at all".into(),
        stderr: String::new(),
    };
    let _ = assert_json_stdout(&bad);
}
