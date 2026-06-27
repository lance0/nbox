//! Performance baseline tests — pin the HTTP request count for key scale paths.
//!
//! These are NOT broad benchmarks. They are narrow, contract-like assertions:
//! "this command makes exactly N HTTP requests to the NetBox API." A regression
//! that adds a redundant fetch, an extra resolution round-trip, or a pagination
//! loop will flip the count and fail the test.
//!
//! Each test stands up a `wiremock` server, points the CLI binary at it via a
//! temp config, runs the real compiled `nbox` binary, then counts the requests
//! the server recorded: `server.received_requests().await.unwrap().len()`.

mod support;

use serde_json::Value;
use support::binary::{run_nbox, temp_config};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// An empty paginated NetBox page (`count=0`, no `next`).
fn empty_page() -> Value {
    serde_json::json!({ "count": 0, "next": null, "previous": null, "results": [] })
}

/// A paginated NetBox page wrapping one result.
fn one_page(result: Value) -> Value {
    serde_json::json!({ "count": 1, "next": null, "previous": null, "results": [result] })
}

/// Mount an empty page on `GET <endpoint>` (no query constraint — matches any
/// query string, so resolution + fan-out both land here).
async fn mount_empty(server: &MockServer, endpoint: &str) {
    Mock::given(method("GET"))
        .and(path(endpoint))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(server)
        .await;
}

/// Mount `body` (a JSON page) on `GET <endpoint>`.
async fn mount_page(server: &MockServer, endpoint: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(endpoint))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// Total number of requests the mock server received.
async fn request_count(server: &MockServer) -> usize {
    server.received_requests().await.unwrap().len()
}

// === 1. Search fan-out makes exactly 20 requests ============================
//
// `nbox search <q>` fans out to all 20 endpoints concurrently, one request each.
// Empty pages (no `next`) mean no pagination follow-up; no scope filters mean no
// extra resolution calls. The total is exactly 20.

/// All 20 search endpoints, mounted empty. Mirrors `mount_empty_all_except` in
/// `tests/search_tests.rs`.
async fn mount_all_search_endpoints_empty(server: &MockServer) {
    for ep in [
        "/api/dcim/devices/",
        "/api/dcim/sites/",
        "/api/ipam/ip-addresses/",
        "/api/ipam/prefixes/",
        "/api/ipam/vlans/",
        "/api/circuits/circuits/",
        "/api/circuits/virtual-circuits/",
        "/api/ipam/aggregates/",
        "/api/ipam/asns/",
        "/api/ipam/ip-ranges/",
        "/api/tenancy/tenants/",
        "/api/tenancy/contacts/",
        "/api/circuits/providers/",
        "/api/virtualization/virtual-machines/",
        "/api/virtualization/virtual-machine-types/",
        "/api/virtualization/clusters/",
        "/api/dcim/racks/",
        "/api/dcim/rack-groups/",
        "/api/ipam/vrfs/",
        "/api/ipam/route-targets/",
    ] {
        mount_empty(server, ep).await;
    }
}

#[tokio::test]
async fn search_fan_out_makes_exactly_20_requests() {
    let server = MockServer::start().await;
    mount_all_search_endpoints_empty(&server).await;

    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "--json".as_ref(),
        "search".as_ref(),
        "test".as_ref(),
    ]);

    assert_eq!(out.code, Some(0), "search failed: {}", out.stderr);
    assert_eq!(
        request_count(&server).await,
        20,
        "search must make exactly one request per endpoint (20), no pagination, no scope resolution"
    );
}

// === 2. Device detail makes exactly 4 round-trips ==========================
//
// `nbox device <name>` resolves by name (`GET /api/dcim/devices/?name__ie=…` →
// 1 result), then fans out to interfaces, IP addresses, and services — three
// concurrent `list_all` calls. One resolution + three fan-outs = 4 requests.

#[tokio::test]
async fn device_detail_makes_exactly_4_round_trips() {
    let server = MockServer::start().await;
    // Resolution: one device named edge01 (id=1).
    mount_page(
        &server,
        "/api/dcim/devices/",
        one_page(serde_json::json!({
            "id": 1, "url": "u", "name": "edge01",
            "status": {"value": "active", "label": "Active"}
        })),
    )
    .await;
    // Fan-out: interfaces, IP addresses, services — all empty.
    mount_empty(&server, "/api/dcim/interfaces/").await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/services/").await;

    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "--json".as_ref(),
        "device".as_ref(),
        "edge01".as_ref(),
    ]);

    assert_eq!(out.code, Some(0), "device failed: {}", out.stderr);
    assert_eq!(
        request_count(&server).await,
        4,
        "device detail must make exactly 4 requests: resolve + interfaces + ips + services"
    );
}

// === 3. Rack detail makes exactly 1 round-trip (resolve-only) ==============
//
// The plain `nbox rack <name>` command resolves the rack by name and renders
// `RackView` — a flat summary with NO fan-out. The elevation + contained-devices
// fan-out is a TUI/`load_detail` path (`ObjectKind::Rack` drill-in), not the
// CLI command. So the CLI command makes exactly 1 request (the name lookup).
//
// The mocks below also stand up the elevation + devices endpoints so a future
// regression that pulls fan-out into the CLI command path is caught: today
// they receive ZERO requests, and the count assertion pins that.

#[tokio::test]
async fn rack_detail_makes_exactly_1_round_trip() {
    let server = MockServer::start().await;
    // Resolution: one rack named rack1 (id=1, u_height=42).
    mount_page(
        &server,
        "/api/dcim/racks/",
        one_page(serde_json::json!({
            "id": 1, "url": "u", "name": "rack1",
            "u_height": 42,
            "status": {"value": "active", "label": "Active"}
        })),
    )
    .await;
    // TUI-only fan-out endpoints — mounted so a regression that drags them into
    // the CLI command is caught. Today the CLI command must NOT touch them.
    mount_empty(&server, "/api/dcim/racks/1/elevation/").await;
    mount_empty(&server, "/api/dcim/devices/").await;

    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "--json".as_ref(),
        "rack".as_ref(),
        "rack1".as_ref(),
    ]);

    assert_eq!(out.code, Some(0), "rack failed: {}", out.stderr);
    assert_eq!(
        request_count(&server).await,
        1,
        "rack CLI command must make exactly 1 request (resolve-only); \
         elevation + contained-devices fan-out is TUI-only"
    );
}

// === 4. Site detail makes exactly 1 round-trip (resolve-only) =============
//
// Symmetric with rack: `nbox site <slug>` resolves by slug and renders
// `SiteView` — no fan-out. The contained-devices + site-racks fan-out is the
// TUI/`load_detail` path, not the CLI command. Exactly 1 request (slug lookup).

#[tokio::test]
async fn site_detail_makes_exactly_1_round_trip() {
    let server = MockServer::start().await;
    // Resolution: one site with slug dc1 (id=1).
    mount_page(
        &server,
        "/api/dcim/sites/",
        one_page(serde_json::json!({
            "id": 1, "url": "u", "name": "DC1", "slug": "dc1",
            "status": {"value": "active", "label": "Active"}
        })),
    )
    .await;
    // TUI-only fan-out endpoints — mounted so a regression that drags them into
    // the CLI command is caught. Today the CLI command must NOT touch them.
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/dcim/racks/").await;

    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "--json".as_ref(),
        "site".as_ref(),
        "dc1".as_ref(),
    ]);

    assert_eq!(out.code, Some(0), "site failed: {}", out.stderr);
    assert_eq!(
        request_count(&server).await,
        1,
        "site CLI command must make exactly 1 request (resolve-only); \
         contained-devices + site-racks fan-out is TUI-only"
    );
}

// === 6. JSON output is one complete value ================================
//
// `nbox device <name> --json` must emit exactly one JSON value on stdout. This
// test pins the consumer contract (one parseable document, no second JSON value
// or non-whitespace trailing bytes); process capture cannot observe syscall/write
// boundaries.

#[tokio::test]
async fn json_stdout_is_one_complete_value() {
    let server = MockServer::start().await;
    mount_page(
        &server,
        "/api/dcim/devices/",
        one_page(serde_json::json!({
            "id": 1, "url": "u", "name": "edge01",
            "status": {"value": "active", "label": "Active"}
        })),
    )
    .await;
    mount_empty(&server, "/api/dcim/interfaces/").await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/services/").await;

    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "--json".as_ref(),
        "device".as_ref(),
        "edge01".as_ref(),
    ]);

    assert_eq!(out.code, Some(0), "device failed: {}", out.stderr);

    // stdout must parse as exactly one JSON value, allowing the trailing newline
    // printed by the JSON output helper.
    let parsed: Value = serde_json::from_str(out.stdout.trim())
        .expect("stdout must be a single materialized JSON object");
    assert_eq!(parsed["name"], Value::String("edge01".into()));
}
