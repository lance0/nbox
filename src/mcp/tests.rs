//! Unit tests for the MCP tool adapters.
//!
//! These call the tool methods directly against a `wiremock` NetBox mock, the
//! same pattern the `tests/` integration suite uses for the query helpers.

use rmcp::ErrorData;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::ErrorCode;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{GetArgs, GetKind, NboxMcp, SearchArgs};
use crate::config::ProfileConfig;
use crate::netbox::client::NetBoxClient;

/// A server bound to a client pointing at the mock.
fn server_for(mock: &MockServer) -> NboxMcp {
    let profile = ProfileConfig {
        url: mock.uri(),
        ..Default::default()
    };
    NboxMcp::new(NetBoxClient::new(&profile, None).unwrap())
}

/// An empty paginated page, for endpoints a flow touches but doesn't care about.
fn empty_page() -> serde_json::Value {
    json!({ "count": 0, "next": null, "previous": null, "results": [] })
}

#[tokio::test]
async fn get_device_returns_device_view() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("name__ie", "edge01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01",
                "status": {"value": "active", "label": "Active"}
            }]
        })))
        .mount(&mock)
        .await;
    // The device-detail fan-out: interfaces, IPs, services (all empty here).
    Mock::given(method("GET"))
        .and(path("/api/dcim/interfaces/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/services/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&mock)
        .await;

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(GetArgs {
            kind: GetKind::Device,
            reference: "edge01".to_string(),
            vrf: None,
            site: None,
            group: None,
        }))
        .await
        .expect("device lookup");

    assert_eq!(value["name"], "edge01");
}

#[tokio::test]
async fn get_missing_device_is_invalid_params() {
    let mock = MockServer::start().await;
    // Both the exact and the contains lookups come back empty → not found.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&mock)
        .await;

    // `Json` doesn't impl Debug, so match rather than `expect_err`.
    let err: ErrorData = match server_for(&mock)
        .nbox_get(Parameters(GetArgs {
            kind: GetKind::Device,
            reference: "nope".to_string(),
            vrf: None,
            site: None,
            group: None,
        }))
        .await
    {
        Ok(_) => panic!("missing device should error"),
        Err(e) => e,
    };

    // Not-found is caller-fixable → invalid_params, with the ref in the message.
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert!(err.message.contains("nope"), "got: {}", err.message);
}

#[tokio::test]
async fn search_returns_results_and_errors() {
    let mock = MockServer::start().await;
    // search fans out across devices, sites, ips, prefixes, vlans (q=…).
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01"}]
        })))
        .mount(&mock)
        .await;
    for p in [
        "/api/dcim/sites/",
        "/api/ipam/ip-addresses/",
        "/api/ipam/prefixes/",
        "/api/ipam/vlans/",
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
            .mount(&mock)
            .await;
    }

    let Json(value) = server_for(&mock)
        .nbox_search(Parameters(SearchArgs {
            query: "edge".to_string(),
            limit: None,
            status: None,
            site: None,
            tenant: None,
            role: None,
            tag: None,
        }))
        .await
        .expect("search");

    let results = value["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["display"], "edge01");
    // No endpoint failed, so the errors list is present and empty.
    assert!(value["errors"].as_array().expect("errors array").is_empty());
}
