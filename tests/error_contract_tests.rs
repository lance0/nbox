//! Binary-level error contracts.
//!
//! These drive the real compiled `nbox` binary so the public contract is pinned
//! at the process boundary: stable exit code, clean stdout, and actionable
//! stderr. Lower-level tests still cover typed error propagation in detail.

use serde_json::json;
use support::binary::{CommandOutput, run_nbox, temp_config};
use support::netbox::page;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

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
