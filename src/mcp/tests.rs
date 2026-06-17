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

use super::{
    GetArgs, GetKind, InterfaceArgs, JournalArgs, ListTagsArgs, NboxMcp, NextIpArgs,
    NextPrefixArgs, SearchArgs,
};
use crate::config::ProfileConfig;
use crate::netbox::client::NetBoxClient;

/// A `GetArgs` for `kind`/`ref` with no disambiguators set, to keep call sites terse.
fn get_args(kind: GetKind, reference: &str) -> GetArgs {
    GetArgs {
        kind,
        reference: reference.to_string(),
        vrf: None,
        site: None,
        group: None,
    }
}

/// Mount a GET on `p` returning a one-result paginated page wrapping `result`.
async fn mount_one(mock: &MockServer, p: &str, result: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null, "results": [result]
        })))
        .mount(mock)
        .await;
}

/// Mount a GET on `p` returning an empty page.
async fn mount_empty(mock: &MockServer, p: &str) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(mock)
        .await;
}

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
    // search fans out across devices, sites, ips, prefixes, vlans, circuits,
    // aggregates, asns, ip-ranges (q=…).
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
        "/api/circuits/circuits/",
        "/api/ipam/aggregates/",
        "/api/ipam/asns/",
        "/api/ipam/ip-ranges/",
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
            .mount(&mock)
            .await;
    }

    let Json(report) = server_for(&mock)
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

    // The tool now returns a typed `SearchReport`; serialize it to confirm the
    // JSON shape rmcp emits is unchanged.
    let value = serde_json::to_value(&report).expect("serialize report");
    let results = value["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["display"], "edge01");
    // No endpoint failed, so the errors list is present and empty.
    assert!(value["errors"].as_array().expect("errors array").is_empty());
}

#[tokio::test]
async fn status_returns_versions() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/status/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "netbox-version": "4.5.5",
            "django-version": "5.0.9",
            "python-version": "3.12.3"
        })))
        .mount(&mock)
        .await;

    let Json(report) = server_for(&mock).nbox_status().await.expect("status");
    let value = serde_json::to_value(&report).expect("serialize report");

    assert_eq!(value["netbox_version"], "4.5.5");
    assert_eq!(value["django_version"], "5.0.9");
    assert_eq!(value["python_version"], "3.12.3");
    // The configured base URL is echoed back (the mock's URI, trailing slash).
    assert_eq!(value["netbox_url"], format!("{}/", mock.uri()));
}

#[tokio::test]
async fn search_reports_partial_endpoint_errors() {
    let mock = MockServer::start().await;
    // Devices succeed; the sites endpoint fails (403). The rest are empty.
    mount_one(
        &mock,
        "/api/dcim/devices/",
        json!({"id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01"}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
        .mount(&mock)
        .await;
    for p in [
        "/api/ipam/ip-addresses/",
        "/api/ipam/prefixes/",
        "/api/ipam/vlans/",
        "/api/circuits/circuits/",
        "/api/ipam/aggregates/",
        "/api/ipam/asns/",
        "/api/ipam/ip-ranges/",
    ] {
        mount_empty(&mock, p).await;
    }

    let Json(report) = server_for(&mock)
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

    let value = serde_json::to_value(&report).expect("serialize report");
    // The device result still comes through alongside the surfaced failure.
    assert_eq!(value["results"].as_array().expect("results").len(), 1);
    let errors = value["errors"].as_array().expect("errors array");
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].as_str().unwrap().contains("sites"),
        "got: {errors:?}"
    );
}

#[tokio::test]
async fn get_ip_returns_ip_view_with_parent_context() {
    let mock = MockServer::start().await;
    // ip_candidates: exact address lookup.
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("address", "10.44.208.55"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 7, "url": "http://nb/api/ipam/ip-addresses/7/",
                "address": "10.44.208.55/24",
                "status": {"value": "active", "label": "Active"},
                "dns_name": "printer-55.example.com"
            }]
        })))
        .mount(&mock)
        .await;
    // prefixes_containing: enrich with the most-specific parent (global table).
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("contains", "10.44.208.55"))
        .and(query_param("vrf_id", "null"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "u", "prefix": "10.44.0.0/16"},
                {"id": 2, "url": "u", "prefix": "10.44.208.0/24",
                 "scope_type": "dcim.site", "scope": {"id": 1, "display": "iad1"},
                 "vlan": {"id": 2, "display": "208 (users)"}}
            ]
        })))
        .mount(&mock)
        .await;

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Ip, "10.44.208.55")))
        .await
        .expect("ip lookup");

    assert_eq!(value["address"], "10.44.208.55/24");
    assert_eq!(value["status"], "active");
    assert_eq!(value["dns_name"], "printer-55.example.com");
    // Enrichment chose the longest-match prefix and pulled its scope/VLAN.
    assert_eq!(value["parent_prefix"], "10.44.208.0/24");
    assert_eq!(value["scope"], "iad1");
    assert_eq!(value["scope_type"], "site");
    assert!(
        value.get("site").is_none(),
        "ip view has no site key: {value}"
    );
    assert_eq!(value["vlan"], "208 (users)");
}

#[tokio::test]
async fn get_prefix_returns_children_and_ips() {
    let mock = MockServer::start().await;
    // prefix_candidates: exact CIDR match.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("prefix", "10.44.208.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 5, "url": "u", "prefix": "10.44.208.0/24",
                "status": {"value": "active", "label": "Active"}
            }]
        })))
        .mount(&mock)
        .await;
    // prefix_children: `within` filter.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("within", "10.44.208.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 6, "url": "u", "prefix": "10.44.208.0/26"}]
        })))
        .mount(&mock)
        .await;
    // prefix_ips: `parent` filter.
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("parent", "10.44.208.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 7, "url": "u", "address": "10.44.208.1/24"}]
        })))
        .mount(&mock)
        .await;

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Prefix, "10.44.208.0/24")))
        .await
        .expect("prefix lookup");

    assert_eq!(value["prefix"], "10.44.208.0/24");
    assert_eq!(value["status"], "active");
    assert_eq!(value["child_prefixes"][0], "10.44.208.0/26");
    assert_eq!(value["ip_addresses"][0]["address"], "10.44.208.1/24");
}

#[tokio::test]
async fn get_vlan_by_vid_returns_vlan_view_with_prefixes() {
    let mock = MockServer::start().await;
    // vlan_candidates_by_vid: numeric ref → `vid` filter (one match → unique).
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("vid", "208"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "u", "vid": 208, "name": "users",
                "status": {"value": "active", "label": "Active"},
                "site": {"id": 1, "display": "iad1"}
            }]
        })))
        .mount(&mock)
        .await;
    // vlan_prefixes: prefixes that reference this VLAN id.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("vlan_id", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 9, "url": "u", "prefix": "10.44.208.0/24"}]
        })))
        .mount(&mock)
        .await;

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Vlan, "208")))
        .await
        .expect("vlan lookup");

    assert_eq!(value["vid"], 208);
    assert_eq!(value["name"], "users");
    // A directly-assigned site surfaces as the type-agnostic scope (type "site").
    assert_eq!(value["scope"], "iad1");
    assert_eq!(value["scope_type"], "site");
    assert!(
        value.get("site").is_none(),
        "vlan view has no site key: {value}"
    );
    assert_eq!(value["prefixes"][0], "10.44.208.0/24");
}

#[tokio::test]
async fn get_vlan_ambiguous_vid_is_invalid_params_with_candidates() {
    let mock = MockServer::start().await;
    // Two VLANs share VID 208 (different sites) and no `site`/`group` is given.
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("vid", "208"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 3, "url": "u", "vid": 208, "name": "users",
                 "site": {"id": 1, "display": "iad1"}},
                {"id": 4, "url": "u", "vid": 208, "name": "users",
                 "site": {"id": 2, "display": "sfo1"}}
            ]
        })))
        .mount(&mock)
        .await;

    let err: ErrorData = match server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Vlan, "208")))
        .await
    {
        Ok(_) => panic!("ambiguous VLAN should error"),
        Err(e) => e,
    };

    // Ambiguous is caller-fixable → invalid_params, and the message lists both
    // candidate VLANs (with their disambiguating site scopes).
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert!(err.message.contains("ambiguous"), "got: {}", err.message);
    assert!(
        err.message.contains("site: iad1") && err.message.contains("site: sfo1"),
        "got: {}",
        err.message
    );
}

#[tokio::test]
async fn get_vlan_disambiguated_by_site_resolves() {
    let mock = MockServer::start().await;
    // Same two-site collision, but `site` narrows it to exactly one.
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("vid", "208"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 3, "url": "u", "vid": 208, "name": "users",
                 "site": {"id": 1, "display": "iad1", "slug": "iad1"}},
                {"id": 4, "url": "u", "vid": 208, "name": "users",
                 "site": {"id": 2, "display": "sfo1", "slug": "sfo1"}}
            ]
        })))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("vlan_id", "4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&mock)
        .await;

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(GetArgs {
            kind: GetKind::Vlan,
            reference: "208".to_string(),
            vrf: None,
            site: Some("sfo1".to_string()),
            group: None,
        }))
        .await
        .expect("vlan disambiguated by site");

    assert_eq!(value["vid"], 208);
    assert_eq!(value["scope"], "sfo1");
    assert_eq!(value["scope_type"], "site");
}

#[tokio::test]
async fn get_site_returns_site_view() {
    let mock = MockServer::start().await;
    // site_by_ref tries slug first.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "u", "name": "IAD1", "slug": "iad1",
                "status": {"value": "active", "label": "Active"},
                "region": {"id": 2, "display": "us-east"}
            }]
        })))
        .mount(&mock)
        .await;

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Site, "iad1")))
        .await
        .expect("site lookup");

    assert_eq!(value["name"], "IAD1");
    assert_eq!(value["slug"], "iad1");
    assert_eq!(value["status"], "active");
    assert_eq!(value["region"], "us-east");
}

#[tokio::test]
async fn get_circuit_returns_circuit_view() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/circuits/circuits/"))
        .and(query_param("cid", "ACME-1234"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "http://nb/api/circuits/circuits/3/", "cid": "ACME-1234",
                "provider": {"id": 1, "display": "ACME"},
                "status": {"value": "active", "label": "Active"},
                "commit_rate": 1_000_000
            }]
        })))
        .mount(&mock)
        .await;

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Circuit, "ACME-1234")))
        .await
        .expect("circuit lookup");

    assert_eq!(value["cid"], "ACME-1234");
    assert_eq!(value["provider"], "ACME");
    assert_eq!(value["status"], "active");
    assert_eq!(value["commit_rate_kbps"], 1_000_000);
}

#[tokio::test]
async fn get_asn_returns_asn_view() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/asns/"))
        .and(query_param("asn", "64512"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 9, "url": "http://nb/api/ipam/asns/9/", "asn": 64512}]
        })))
        .mount(&mock)
        .await;

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Asn, "64512")))
        .await
        .expect("asn lookup");

    assert_eq!(value["asn"], 64512);
}

#[tokio::test]
async fn get_missing_prefix_is_invalid_params() {
    let mock = MockServer::start().await;
    // prefix_candidates returns nothing → not found.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("prefix", "10.99.99.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&mock)
        .await;

    let err: ErrorData = match server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Prefix, "10.99.99.0/24")))
        .await
    {
        Ok(_) => panic!("missing prefix should error"),
        Err(e) => e,
    };

    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert!(
        err.message.contains("10.99.99.0/24"),
        "got: {}",
        err.message
    );
}

#[tokio::test]
async fn get_interface_includes_cable_path_trace() {
    let mock = MockServer::start().await;
    // device_by_ref (exact name).
    mount_one(
        &mock,
        "/api/dcim/devices/",
        json!({"id": 1, "url": "u", "name": "edge01"}),
    )
    .await;
    // device_interface: exact (device_id + name) match.
    Mock::given(method("GET"))
        .and(path("/api/dcim/interfaces/"))
        .and(query_param("device_id", "1"))
        .and(query_param("name", "xe-0/0/0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 42, "url": "u", "name": "xe-0/0/0",
                "device": {"id": 1, "display": "edge01"},
                "enabled": true,
                "type": {"value": "10gbase-x-sfpp", "label": "SFP+ (10GE)"}
            }]
        })))
        .mount(&mock)
        .await;
    // interface_ips: addresses on the interface.
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("interface_id", "42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 8, "url": "u", "address": "10.0.0.1/31"}]
        })))
        .mount(&mock)
        .await;
    // interface_trace: one cable-path hop.
    Mock::given(method("GET"))
        .and(path("/api/dcim/interfaces/42/trace/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            [
                [{"display": "xe-0/0/0", "device": {"display": "edge01"}}],
                {"display": "Cable #3"},
                [{"display": "xe-1/0/0", "device": {"display": "core01"}}]
            ]
        ])))
        .mount(&mock)
        .await;

    let Json(view) = server_for(&mock)
        .nbox_get_interface(Parameters(InterfaceArgs {
            device: "edge01".to_string(),
            interface: "xe-0/0/0".to_string(),
        }))
        .await
        .expect("interface lookup");

    let value = serde_json::to_value(&view).expect("serialize view");
    assert_eq!(value["name"], "xe-0/0/0");
    assert_eq!(value["device"], "edge01");
    assert_eq!(value["ip_addresses"][0], "10.0.0.1/31");
    // The cable-path trace is rendered into the `trace` field.
    assert_eq!(
        value["trace"][0],
        "edge01 xe-0/0/0 --[Cable #3]-- core01 xe-1/0/0"
    );
}

#[tokio::test]
async fn next_ip_returns_available_addresses() {
    let mock = MockServer::start().await;
    // resolve_prefix → prefix_candidates (exact CIDR).
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("prefix", "10.44.208.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 5, "url": "u", "prefix": "10.44.208.0/24"}]
        })))
        .mount(&mock)
        .await;
    // available-ips: a bare JSON array, not a page.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/5/available-ips/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"family": 4, "address": "10.44.208.1/24"},
            {"family": 4, "address": "10.44.208.2/24"}
        ])))
        .mount(&mock)
        .await;

    let Json(report) = server_for(&mock)
        .nbox_next_ip(Parameters(NextIpArgs {
            prefix: "10.44.208.0/24".to_string(),
            count: Some(2),
            vrf: None,
        }))
        .await
        .expect("next ip");

    let value = serde_json::to_value(&report).expect("serialize report");
    assert_eq!(value["prefix"], "10.44.208.0/24");
    let available = value["available"].as_array().expect("available array");
    assert_eq!(available.len(), 2);
    assert_eq!(available[0], "10.44.208.1/24");
    assert_eq!(available[1], "10.44.208.2/24");
}

#[tokio::test]
async fn next_prefix_with_length_returns_first_block() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("prefix", "10.44.208.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 5, "url": "u", "prefix": "10.44.208.0/24"}]
        })))
        .mount(&mock)
        .await;
    // available-prefixes: two free blocks; only the /26 is requested.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/5/available-prefixes/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"family": 4, "prefix": "10.44.208.0/25"},
            {"family": 4, "prefix": "10.44.208.128/25"}
        ])))
        .mount(&mock)
        .await;

    let Json(report) = server_for(&mock)
        .nbox_next_prefix(Parameters(NextPrefixArgs {
            prefix: "10.44.208.0/24".to_string(),
            length: Some(26),
            vrf: None,
        }))
        .await
        .expect("next prefix");

    let value = serde_json::to_value(&report).expect("serialize report");
    assert_eq!(value["prefix"], "10.44.208.0/24");
    // The first free /26 carved out of the free space.
    let available = value["available"].as_array().expect("available array");
    assert_eq!(available.len(), 1);
    assert_eq!(available[0], "10.44.208.0/26");
}

#[tokio::test]
async fn next_prefix_with_length_finds_fitting_block_beyond_first_50() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("prefix", "10.44.0.0/16"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 5, "url": "u", "prefix": "10.44.0.0/16"}]
        })))
        .mount(&mock)
        .await;
    // 60 free /26 blocks: a /26 can't be subnetted to a /25, so the first 59 are
    // all too small. Only the 60th block — a /24, past NetBox's 50-default — can
    // yield the requested /25. The query layer must send `limit=1000` to surface
    // it; the matcher enforces that.
    let mut blocks: Vec<_> = (0..59)
        .map(|i| json!({"family": 4, "prefix": format!("10.44.{i}.0/26")}))
        .collect();
    blocks.push(json!({"family": 4, "prefix": "10.44.200.0/24"}));
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/5/available-prefixes/"))
        .and(query_param("limit", "1000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(blocks)))
        .mount(&mock)
        .await;

    let Json(report) = server_for(&mock)
        .nbox_next_prefix(Parameters(NextPrefixArgs {
            prefix: "10.44.0.0/16".to_string(),
            length: Some(25),
            vrf: None,
        }))
        .await
        .expect("next prefix");

    let value = serde_json::to_value(&report).expect("serialize report");
    let available = value["available"].as_array().expect("available array");
    assert_eq!(available.len(), 1);
    // The first /25 carved from the fitting block that sits past the 50th candidate.
    assert_eq!(available[0], "10.44.200.0/25");
}

#[tokio::test]
async fn next_prefix_without_length_lists_all_free_blocks() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("prefix", "10.44.208.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 5, "url": "u", "prefix": "10.44.208.0/24"}]
        })))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/5/available-prefixes/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"family": 4, "prefix": "10.44.208.0/25"},
            {"family": 4, "prefix": "10.44.208.128/25"}
        ])))
        .mount(&mock)
        .await;

    let Json(report) = server_for(&mock)
        .nbox_next_prefix(Parameters(NextPrefixArgs {
            prefix: "10.44.208.0/24".to_string(),
            length: None,
            vrf: None,
        }))
        .await
        .expect("next prefix");

    let value = serde_json::to_value(&report).expect("serialize report");
    let available = value["available"].as_array().expect("available array");
    assert_eq!(available.len(), 2);
    assert_eq!(available[0], "10.44.208.0/25");
    assert_eq!(available[1], "10.44.208.128/25");
}

#[tokio::test]
async fn journal_returns_entries_for_device() {
    let mock = MockServer::start().await;
    // resolve_content_type_id(Device) → device_by_ref (exact name).
    mount_one(
        &mock,
        "/api/dcim/devices/",
        json!({"id": 1, "url": "u", "name": "edge01"}),
    )
    .await;
    // journal_entries filtered by assigned object.
    Mock::given(method("GET"))
        .and(path("/api/extras/journal-entries/"))
        .and(query_param("assigned_object_type", "dcim.device"))
        .and(query_param("assigned_object_id", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 5, "created": "2024-01-02",
                "kind": {"value": "info", "label": "Info"}, "comments": "rebooted"
            }]
        })))
        .mount(&mock)
        .await;

    let Json(view) = server_for(&mock)
        .nbox_journal(Parameters(JournalArgs {
            kind: GetKind::Device,
            reference: "edge01".to_string(),
            limit: None,
        }))
        .await
        .expect("journal");

    let value = serde_json::to_value(&view).expect("serialize view");
    let entries = value["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["comments"], "rebooted");
    assert_eq!(entries[0]["kind"], "info");
    assert_eq!(entries[0]["created"], "2024-01-02");
}

#[tokio::test]
async fn journal_returns_entries_for_aggregate() {
    let mock = MockServer::start().await;
    // resolve_content_type_id(Aggregate) → aggregate_by_ref (filtered by prefix).
    mount_one(
        &mock,
        "/api/ipam/aggregates/",
        json!({"id": 7, "url": "u", "prefix": "10.0.0.0/8"}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/extras/journal-entries/"))
        .and(query_param("assigned_object_type", "ipam.aggregate"))
        .and(query_param("assigned_object_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 11, "created": "2024-03-04",
                "kind": {"value": "info", "label": "Info"}, "comments": "registered with RIR"
            }]
        })))
        .mount(&mock)
        .await;

    let Json(view) = server_for(&mock)
        .nbox_journal(Parameters(JournalArgs {
            kind: GetKind::Aggregate,
            reference: "10.0.0.0/8".to_string(),
            limit: None,
        }))
        .await
        .expect("journal");

    let value = serde_json::to_value(&view).expect("serialize view");
    let entries = value["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["comments"], "registered with RIR");
}

#[tokio::test]
async fn journal_returns_entries_for_asn() {
    let mock = MockServer::start().await;
    // resolve_content_type_id(Asn) → asn_by_ref (the ref is parsed to a u32).
    mount_one(
        &mock,
        "/api/ipam/asns/",
        json!({"id": 9, "url": "u", "asn": 64512}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/extras/journal-entries/"))
        .and(query_param("assigned_object_type", "ipam.asn"))
        .and(query_param("assigned_object_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 12, "created": "2024-03-05",
                "kind": {"value": "info", "label": "Info"}, "comments": "assigned to tenant"
            }]
        })))
        .mount(&mock)
        .await;

    let Json(view) = server_for(&mock)
        .nbox_journal(Parameters(JournalArgs {
            kind: GetKind::Asn,
            reference: "64512".to_string(),
            limit: None,
        }))
        .await
        .expect("journal");

    let value = serde_json::to_value(&view).expect("serialize view");
    let entries = value["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["comments"], "assigned to tenant");
}

#[tokio::test]
async fn journal_returns_entries_for_ip_range() {
    let mock = MockServer::start().await;
    // resolve_content_type_id(IpRange) → ip_range_by_ref (filtered by start_address).
    mount_one(
        &mock,
        "/api/ipam/ip-ranges/",
        json!({"id": 4, "url": "u", "start_address": "10.0.0.1/24", "end_address": "10.0.0.50/24"}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/extras/journal-entries/"))
        .and(query_param("assigned_object_type", "ipam.iprange"))
        .and(query_param("assigned_object_id", "4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 13, "created": "2024-03-06",
                "kind": {"value": "info", "label": "Info"}, "comments": "DHCP pool"
            }]
        })))
        .mount(&mock)
        .await;

    let Json(view) = server_for(&mock)
        .nbox_journal(Parameters(JournalArgs {
            kind: GetKind::IpRange,
            reference: "10.0.0.1".to_string(),
            limit: None,
        }))
        .await
        .expect("journal");

    let value = serde_json::to_value(&view).expect("serialize view");
    let entries = value["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["comments"], "DHCP pool");
}

#[tokio::test]
async fn journal_for_unknown_kind_errors() {
    // Every modeled MCP `GetKind` is now journal-able (CLI parity), so the
    // not-supported path can only be reached with a genuinely-unknown kind
    // string. Exercise the shared source-of-truth resolver directly to prove it
    // still rejects an unmodeled kind rather than silently resolving it.
    let mock = MockServer::start().await;
    let client = server_for(&mock).client;
    let err = crate::resolve_content_type_id(&client, "teapot", "anything")
        .await
        .expect_err("unknown kind should error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unknown object kind") && msg.contains("teapot"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn list_tags_returns_tag_rows() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/extras/tags/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "name": "Critical", "slug": "critical",
                 "color": "ff0000", "tagged_items": 3},
                {"id": 2, "name": "Edge", "slug": "edge",
                 "color": "00ff00", "tagged_items": 0}
            ]
        })))
        .mount(&mock)
        .await;

    let Json(view) = server_for(&mock)
        .nbox_list_tags(Parameters(ListTagsArgs { limit: None }))
        .await
        .expect("list tags");

    let value = serde_json::to_value(&view).expect("serialize view");
    let tags = value["tags"].as_array().expect("tags array");
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0]["slug"], "critical");
    assert_eq!(tags[0]["name"], "Critical");
    assert_eq!(tags[0]["count"], 3);
    assert_eq!(tags[1]["slug"], "edge");
}

#[tokio::test]
async fn get_ambiguous_ip_is_invalid_params_with_candidates() {
    let mock = MockServer::start().await;
    // Two IPs share the same host address across VRFs; no `vrf` is supplied.
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("address", "10.0.0.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "u", "address": "10.0.0.1/24",
                 "vrf": {"id": 1, "display": "blue"}},
                {"id": 2, "url": "u", "address": "10.0.0.1/24",
                 "vrf": {"id": 2, "display": "green"}}
            ]
        })))
        .mount(&mock)
        .await;

    let err: ErrorData = match server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Ip, "10.0.0.1")))
        .await
    {
        Ok(_) => panic!("ambiguous IP should error"),
        Err(e) => e,
    };

    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert!(err.message.contains("ambiguous"), "got: {}", err.message);
    // The candidate list names both VRF scopes so the caller can pass `vrf`.
    assert!(
        err.message.contains("vrf: blue") && err.message.contains("vrf: green"),
        "got: {}",
        err.message
    );
}
