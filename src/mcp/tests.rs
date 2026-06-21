//! Unit tests for the MCP tool adapters.
//!
//! These call the tool methods directly against a `wiremock` NetBox mock, the
//! same pattern the `tests/` integration suite uses for the query helpers.

use rmcp::ErrorData;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ErrorCode, ResourceContents};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{
    GetArgs, GetKind, InterfaceArgs, JournalArgs, ListTagsArgs, NboxMcp, NextIpArgs,
    NextPrefixArgs, SearchArgs, parse_resource_uri, percent_decode, resource_templates,
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
    // A disabled cache so each tool call hits the mock (the `.expect(N)` counts
    // assume no caching); cache behavior has its own dedicated tests.
    NboxMcp::new(
        NetBoxClient::new(&profile, None).unwrap(),
        crate::cache::Cache::disabled(),
    )
}

#[tokio::test]
async fn nbox_get_consults_cache_and_clear_busts_it() {
    // No reachable NetBox: a cache HIT must answer with no network, and after a
    // clear the next call must fall through to the (unreachable) origin and error.
    let profile = ProfileConfig {
        url: "http://127.0.0.1:9/".into(),
        ..Default::default()
    };
    let cache = crate::cache::Cache::from_settings(
        "t".into(),
        &crate::config::CacheSettings {
            enabled: true,
            ttl_secs: 30,
        },
    );
    let server = NboxMcp::new(NetBoxClient::new(&profile, None).unwrap(), cache);

    let args = get_args(GetKind::Site, "iad1");
    // Pre-seed under the exact key `get_cached` computes for these args.
    let key = crate::cache::CacheKey::object("site", "iad1", "vrf=;site=;group=");
    server
        .cache
        .put(&key, &json!({ "name": "iad1", "from_cache": true }));

    let Json(v) = server
        .get_cached(args.clone())
        .await
        .expect("served from cache without touching the network");
    assert_eq!(v["from_cache"], json!(true));

    // Clearing drops the entry, so the next get falls through to the unreachable
    // origin and errors — proving the clear tool busts the cache.
    server.cache.clear_all();
    assert!(
        server.get_cached(args).await.is_err(),
        "after clear, a miss hits NetBox (unreachable in this test)"
    );
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
    // aggregates, asns, ip-ranges, tenants, contacts, providers (q=…).
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
        "/api/tenancy/tenants/",
        "/api/tenancy/contacts/",
        "/api/circuits/providers/",
        "/api/virtualization/virtual-machines/",
        "/api/virtualization/clusters/",
        "/api/dcim/racks/",
        "/api/ipam/vrfs/",
        "/api/ipam/route-targets/",
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
            region: None,
            site_group: None,
            location: None,
            tenant: None,
            role: None,
            tag: None,
            vrf: None,
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
    // Per-surface API routing: a default (REST) profile is effective REST on both.
    assert_eq!(value["api"]["search"]["configured"], "rest");
    assert_eq!(value["api"]["search"]["effective"], "rest");
    assert_eq!(value["api"]["vrf"]["effective"], "rest");
    assert_eq!(value["capabilities"]["version"]["compatible"], true);
    assert_eq!(value["capabilities"]["rest"]["search"], true);
    // A REST profile doesn't probe GraphQL, so no surface support is reported.
    assert_eq!(value["capabilities"]["graphql"]["probed"], false);
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
        "/api/tenancy/tenants/",
        "/api/tenancy/contacts/",
        "/api/circuits/providers/",
        "/api/virtualization/virtual-machines/",
        "/api/virtualization/clusters/",
        "/api/dcim/racks/",
        "/api/ipam/vrfs/",
        "/api/ipam/route-targets/",
    ] {
        mount_empty(&mock, p).await;
    }

    let Json(report) = server_for(&mock)
        .nbox_search(Parameters(SearchArgs {
            query: "edge".to_string(),
            limit: None,
            status: None,
            site: None,
            region: None,
            site_group: None,
            location: None,
            tenant: None,
            role: None,
            tag: None,
            vrf: None,
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
async fn get_vlan_site_disambiguation_prefers_exact_slug_over_prefix_sibling() {
    // Regression: the same VID at two sites whose slugs are prefix-related
    // (`ci-site` / `ci-site2`). `--site ci-site` must resolve to ci-site's VLAN,
    // NOT stay ambiguous — the loose display-substring match would otherwise also
    // retain ci-site2 (its display contains the substring "ci-site").
    let mock = MockServer::start().await;
    let two_sited = || {
        json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 7, "url": "u", "vid": 1234, "name": "ci-vlan",
                 "site": {"id": 1, "display": "ci-site", "slug": "ci-site"}},
                {"id": 8, "url": "u", "vid": 1234, "name": "ci-vlan2",
                 "site": {"id": 2, "display": "ci-site2", "slug": "ci-site2"}}
            ]
        })
    };
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("vid", "1234"))
        .respond_with(ResponseTemplate::new(200).set_body_json(two_sited()))
        .mount(&mock)
        .await;
    // Either VLAN may be the one resolved; both prefix lookups return empty.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&mock)
        .await;

    // `--site ci-site` → exact slug match on ci-site wins, ci-site2 excluded.
    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(GetArgs {
            kind: GetKind::Vlan,
            reference: "1234".to_string(),
            vrf: None,
            site: Some("ci-site".to_string()),
            group: None,
        }))
        .await
        .expect("ci-site disambiguates to ci-vlan");
    assert_eq!(value["vid"], 1234);
    assert_eq!(value["name"], "ci-vlan");
    assert_eq!(value["scope"], "ci-site");

    // `--site ci-site2` resolves to the other VLAN.
    let Json(value2) = server_for(&mock)
        .nbox_get(Parameters(GetArgs {
            kind: GetKind::Vlan,
            reference: "1234".to_string(),
            vrf: None,
            site: Some("ci-site2".to_string()),
            group: None,
        }))
        .await
        .expect("ci-site2 disambiguates to ci-vlan2");
    assert_eq!(value2["vid"], 1234);
    assert_eq!(value2["name"], "ci-vlan2");
    assert_eq!(value2["scope"], "ci-site2");
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
async fn get_tenant_returns_tenant_view() {
    let mock = MockServer::start().await;
    // tenant_by_ref tries slug first.
    Mock::given(method("GET"))
        .and(path("/api/tenancy/tenants/"))
        .and(query_param("slug", "acme"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 4, "url": "http://nb/api/tenancy/tenants/4/",
                "name": "Acme Corp", "slug": "acme",
                "group": {"id": 2, "display": "Customers"},
                "device_count": 12, "prefix_count": 5, "site_count": 0,
                "tags": [{"id": 1, "name": "vip", "slug": "vip"}],
                "custom_fields": {"account_id": "A-100"}
            }]
        })))
        .mount(&mock)
        .await;

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Tenant, "acme")))
        .await
        .expect("tenant lookup");

    assert_eq!(value["name"], "Acme Corp");
    assert_eq!(value["slug"], "acme");
    assert_eq!(value["group"], "Customers");
    assert_eq!(value["device_count"], 12);
    assert_eq!(value["prefix_count"], 5);
    // Zero counts are dropped.
    assert!(value.get("site_count").is_none());
    assert_eq!(value["tags"][0], "vip");
    assert_eq!(value["custom_fields"]["account_id"], "A-100");
}

#[tokio::test]
async fn get_contact_returns_contact_view() {
    let mock = MockServer::start().await;
    // contact_by_ref has no slug; it tries `name__ie` first.
    Mock::given(method("GET"))
        .and(path("/api/tenancy/contacts/"))
        .and(query_param("name__ie", "Jane Doe"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 7, "url": "http://nb/api/tenancy/contacts/7/",
                "name": "Jane Doe",
                "group": {"id": 3, "display": "NOC"},
                "title": "Network Engineer",
                "phone": "+1-555-0100",
                "email": "jane@example.com",
                "address": "",
                "link": "https://example.com/jane",
                "tags": [{"id": 2, "name": "oncall", "slug": "oncall"}],
                "custom_fields": {"pager": "555-9000"}
            }]
        })))
        .mount(&mock)
        .await;

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Contact, "Jane Doe")))
        .await
        .expect("contact lookup");

    assert_eq!(value["name"], "Jane Doe");
    assert_eq!(value["group"], "NOC");
    assert_eq!(value["title"], "Network Engineer");
    assert_eq!(value["email"], "jane@example.com");
    assert_eq!(value["link"], "https://example.com/jane");
    // Empty string dropped.
    assert!(value.get("address").is_none());
    assert_eq!(value["tags"][0], "oncall");
    assert_eq!(value["custom_fields"]["pager"], "555-9000");
}

#[tokio::test]
async fn get_provider_returns_provider_view() {
    let mock = MockServer::start().await;
    // provider_by_ref tries slug first.
    Mock::given(method("GET"))
        .and(path("/api/circuits/providers/"))
        .and(query_param("slug", "acme-telecom"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 4, "url": "http://nb/api/circuits/providers/4/",
                "name": "ACME Telecom", "slug": "acme-telecom",
                "asns": [
                    {"id": 5, "url": "u", "asn": 64512},
                    {"id": 6, "url": "u", "asn": 64513}
                ],
                "accounts": [
                    {"id": 3, "display": "ACME-001", "name": "primary", "account": "ACME-001"}
                ],
                "description": "upstream transit",
                "circuit_count": 7,
                "tags": [{"id": 1, "name": "transit", "slug": "transit"}],
                "custom_fields": {"noc_email": "noc@acme.example"}
            }]
        })))
        .mount(&mock)
        .await;

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Provider, "acme-telecom")))
        .await
        .expect("provider lookup");

    assert_eq!(value["name"], "ACME Telecom");
    assert_eq!(value["slug"], "acme-telecom");
    assert_eq!(value["asns"][0], 64512);
    assert_eq!(value["asns"][1], 64513);
    assert_eq!(value["accounts"][0], "ACME-001");
    assert_eq!(value["circuit_count"], 7);
    assert_eq!(value["tags"][0], "transit");
    assert_eq!(value["custom_fields"]["noc_email"], "noc@acme.example");
}

#[tokio::test]
async fn get_vm_returns_vm_view() {
    let mock = MockServer::start().await;
    // vm_by_ref has no slug; it tries `name__ie` first.
    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .and(query_param("name__ie", "web-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 5, "url": "http://nb/api/virtualization/virtual-machines/5/",
                "name": "web-01",
                "status": {"value": "active", "label": "Active"},
                "cluster": {"id": 3, "display": "prod"},
                "platform": {"id": 4, "display": "Ubuntu 22.04"},
                "primary_ip4": {"id": 11, "display": "10.0.0.5/24"},
                "vcpus": 4.0, "memory": 8192, "disk": 100,
                "tags": [{"id": 1, "name": "prod", "slug": "prod"}],
                "custom_fields": {"owner": "platform"}
            }]
        })))
        .mount(&mock)
        .await;

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Vm, "web-01")))
        .await
        .expect("vm lookup");

    assert_eq!(value["name"], "web-01");
    assert_eq!(value["status"], "active");
    assert_eq!(value["cluster"], "prod");
    assert_eq!(value["platform"], "Ubuntu 22.04");
    assert_eq!(value["primary_ip4"], "10.0.0.5/24");
    assert_eq!(value["vcpus"], 4.0);
    assert_eq!(value["memory"], 8192);
    assert_eq!(value["tags"][0], "prod");
    assert_eq!(value["custom_fields"]["owner"], "platform");
}

#[tokio::test]
async fn get_cluster_returns_cluster_view() {
    let mock = MockServer::start().await;
    // cluster_by_ref has no slug; it tries `name__ie` first.
    Mock::given(method("GET"))
        .and(path("/api/virtualization/clusters/"))
        .and(query_param("name__ie", "prod"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "http://nb/api/virtualization/clusters/3/",
                "name": "prod",
                "type": {"id": 1, "display": "VMware"},
                "group": {"id": 2, "display": "us-east"},
                "status": {"value": "active", "label": "Active"},
                "scope_type": "dcim.site",
                "scope": {"id": 1, "display": "iad1"},
                "device_count": 4, "virtualmachine_count": 0,
                "tags": [{"id": 1, "name": "prod", "slug": "prod"}],
                "custom_fields": {"sla": "gold"}
            }]
        })))
        .mount(&mock)
        .await;

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(get_args(GetKind::Cluster, "prod")))
        .await
        .expect("cluster lookup");

    assert_eq!(value["name"], "prod");
    assert_eq!(value["type"], "VMware");
    assert_eq!(value["group"], "us-east");
    assert_eq!(value["status"], "active");
    assert_eq!(value["scope"], "iad1");
    assert_eq!(value["scope_type"], "site");
    assert_eq!(value["device_count"], 4);
    // Zero counts are dropped.
    assert!(value.get("virtualmachine_count").is_none());
    assert_eq!(value["tags"][0], "prod");
    assert_eq!(value["custom_fields"]["sla"], "gold");
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

// ---- MCP resources (nbox://{kind}/{ref}) ------------------------------------

/// Extract the single text content from a `read_resource_impl` result, asserting
/// the URI and JSON mime type round-trip.
fn one_text(result: &super::ReadResourceResult, expect_uri: &str) -> serde_json::Value {
    assert_eq!(result.contents.len(), 1, "exactly one content");
    match &result.contents[0] {
        ResourceContents::TextResourceContents {
            uri,
            mime_type,
            text,
            ..
        } => {
            assert_eq!(uri, expect_uri);
            assert_eq!(mime_type.as_deref(), Some("application/json"));
            serde_json::from_str(text).expect("content is JSON")
        }
        ResourceContents::BlobResourceContents { .. } => panic!("expected text content"),
    }
}

#[test]
fn percent_decode_handles_escapes_and_passthrough() {
    assert_eq!(percent_decode("10.0.0.0%2F24"), "10.0.0.0/24");
    assert_eq!(percent_decode("edge01"), "edge01");
    assert_eq!(percent_decode("a%20b"), "a b");
    // A malformed escape is left verbatim rather than dropped.
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%zz"), "%zz");
}

#[test]
fn parse_resource_uri_round_trips_kind_and_ref() {
    let (kind, reference) = parse_resource_uri("nbox://device/edge01").expect("parse");
    assert!(matches!(kind, GetKind::Device));
    assert_eq!(reference, "edge01");

    // ip_range's underscore slug parses (distinct from the CLI's `ip-range`).
    let (kind, reference) = parse_resource_uri("nbox://ip_range/10.0.0.1").expect("parse");
    assert!(matches!(kind, GetKind::IpRange));
    assert_eq!(reference, "10.0.0.1");

    // A CIDR ref is percent-encoded so the embedded slash survives the split.
    let (kind, reference) = parse_resource_uri("nbox://prefix/10.44.208.0%2F24").expect("parse");
    assert!(matches!(kind, GetKind::Prefix));
    assert_eq!(reference, "10.44.208.0/24");
}

#[test]
fn get_accepts_the_ip_address_alias_from_search() {
    // nbox_search returns `kind = "ip_address"` (ObjectKind::IpAddress) while
    // nbox_get canonically uses `ip`. The alias lets an agent chain search → get
    // (and `nbox://ip_address/…`) without translating the kind. Every other kind
    // already matches between the two enums, so `ip_address` is the only alias.
    // Tool-arg path (serde Deserialize):
    assert!(matches!(
        serde_json::from_value::<GetKind>(serde_json::json!("ip_address")).unwrap(),
        GetKind::Ip
    ));
    assert!(matches!(
        serde_json::from_value::<GetKind>(serde_json::json!("ip")).unwrap(),
        GetKind::Ip
    ));
    // Resource-URI path:
    let (kind, reference) = parse_resource_uri("nbox://ip_address/10.0.0.1").expect("parse");
    assert!(matches!(kind, GetKind::Ip));
    assert_eq!(reference, "10.0.0.1");
}

#[test]
fn parse_resource_uri_rejects_bad_scheme_kind_and_empty_ref() {
    for uri in ["file:///etc/passwd", "device/edge01", "nbox:/device/edge01"] {
        let err = parse_resource_uri(uri).expect_err("bad scheme");
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    }

    let err = parse_resource_uri("nbox://gadget/edge01").expect_err("unknown kind");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert!(err.message.contains("gadget"), "got: {}", err.message);
    assert!(err.message.contains("device"), "lists valid kinds");

    let err = parse_resource_uri("nbox://device/").expect_err("empty ref");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);

    // No '/' after the kind at all → malformed.
    let err = parse_resource_uri("nbox://device").expect_err("no ref segment");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
}

#[test]
fn resource_templates_advertises_the_uri_template() {
    let result = resource_templates();
    assert_eq!(result.resource_templates.len(), 1);
    let t = &result.resource_templates[0];
    assert_eq!(t.uri_template, "nbox://{kind}/{ref}");
    assert_eq!(t.mime_type.as_deref(), Some("application/json"));
    // The description enumerates the kinds so a host knows what's addressable.
    let desc = t.description.as_deref().expect("description");
    assert!(desc.contains("device") && desc.contains("ip_range"));
}

#[tokio::test]
async fn read_resource_returns_site_view() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "u", "name": "IAD1", "slug": "iad1",
                "status": {"value": "active", "label": "Active"}
            }]
        })))
        .mount(&mock)
        .await;

    let result = server_for(&mock)
        .read_resource_impl("nbox://site/iad1")
        .await
        .expect("read site resource");
    let value = one_text(&result, "nbox://site/iad1");
    assert_eq!(value["name"], "IAD1");
    assert_eq!(value["slug"], "iad1");
}

#[tokio::test]
async fn read_resource_returns_prefix_view_for_encoded_cidr() {
    let mock = MockServer::start().await;
    // prefix_candidates: exact CIDR match (the slash arrives percent-decoded).
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("prefix", "10.44.208.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 5, "url": "http://nb/api/ipam/prefixes/5/",
                "prefix": "10.44.208.0/24",
                "status": {"value": "active", "label": "Active"}
            }]
        })))
        .mount(&mock)
        .await;
    // prefix detail fans out to children (`within`) and member IPs (`parent`),
    // both empty here.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("within", "10.44.208.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("parent", "10.44.208.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&mock)
        .await;

    let result = server_for(&mock)
        .read_resource_impl("nbox://prefix/10.44.208.0%2F24")
        .await
        .expect("read prefix resource");
    let value = one_text(&result, "nbox://prefix/10.44.208.0%2F24");
    assert_eq!(value["prefix"], "10.44.208.0/24");
}

#[tokio::test]
async fn read_resource_unknown_kind_is_invalid_params() {
    let mock = MockServer::start().await;
    let err = server_for(&mock)
        .read_resource_impl("nbox://gadget/edge01")
        .await
        .expect_err("unknown kind errors");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert!(err.message.contains("gadget"), "got: {}", err.message);
}

#[tokio::test]
async fn read_resource_missing_object_is_invalid_params() {
    let mock = MockServer::start().await;
    // Every device lookup comes back empty → not found → invalid_params.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&mock)
        .await;

    let err = server_for(&mock)
        .read_resource_impl("nbox://device/nope")
        .await
        .expect_err("missing object errors");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert!(err.message.contains("nope"), "got: {}", err.message);
}

// ---- Response contracts -----------------------------------------------------
//
// The tests above prove each tool *works* against a representative NetBox; this
// module pins the *shape* of what they return — the JSON contract an agent (or a
// future host) depends on. They assert the full key set of each report struct
// (not just sampled fields), the partial-failure reporting shape, the resource
// path's byte-equality with `nbox_get`, and the error-category mapping AS IT IS.
// All run against direct server/tool calls over a `wiremock` NetBox — no
// JSON-RPC wire snapshots.
mod contracts {
    use super::{
        ErrorCode, GetKind, JournalArgs, Json, ListTagsArgs, Mock, MockServer, NextIpArgs,
        Parameters, ResponseTemplate, SearchArgs, empty_page, get_args, method, mount_empty,
        mount_one, one_text, path, query_param, server_for,
    };
    use rmcp::ErrorData;
    use serde_json::{Value, json};

    /// Assert a JSON object has exactly these top-level keys (order-independent).
    /// Pins the contract's key set: a removed or renamed field, or a new one not
    /// listed here, fails the test.
    fn assert_keys(value: &Value, expected: &[&str]) {
        let obj = value.as_object().expect("a JSON object");
        let mut got: Vec<&str> = obj.keys().map(String::as_str).collect();
        got.sort_unstable();
        let mut want: Vec<&str> = expected.to_vec();
        want.sort_unstable();
        assert_eq!(got, want, "key set drifted; full value: {value}");
    }

    /// The 16 search fan-out endpoints other than devices, mounted empty.
    async fn mount_search_endpoints_empty(mock: &MockServer) {
        for p in [
            "/api/dcim/sites/",
            "/api/ipam/ip-addresses/",
            "/api/ipam/prefixes/",
            "/api/ipam/vlans/",
            "/api/circuits/circuits/",
            "/api/ipam/aggregates/",
            "/api/ipam/asns/",
            "/api/ipam/ip-ranges/",
            "/api/tenancy/tenants/",
            "/api/tenancy/contacts/",
            "/api/circuits/providers/",
            "/api/virtualization/virtual-machines/",
            "/api/virtualization/clusters/",
            "/api/dcim/racks/",
            "/api/ipam/vrfs/",
            "/api/ipam/route-targets/",
        ] {
            mount_empty(mock, p).await;
        }
    }

    fn search_args(query: &str) -> SearchArgs {
        SearchArgs {
            query: query.to_string(),
            limit: None,
            status: None,
            site: None,
            region: None,
            site_group: None,
            location: None,
            tenant: None,
            role: None,
            tag: None,
            vrf: None,
        }
    }

    /// `nbox_status` → `StatusReport`: the full top-level key set and the nested
    /// `api`/`capabilities` shapes a host reads to decide reachability/routing.
    #[tokio::test]
    async fn status_report_shape_is_pinned() {
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

        // Top-level contract: every key always present (no skip_serializing).
        assert_keys(
            &value,
            &[
                "netbox_url",
                "api",
                "netbox_version",
                "django_version",
                "python_version",
                "capabilities",
            ],
        );

        // Per-surface routing: each surface reports configured + effective; the
        // optional `reason` is omitted when there is no fallback (REST profile).
        assert_keys(&value["api"], &["search", "vrf", "route_target"]);
        assert_keys(&value["api"]["search"], &["configured", "effective"]);
        assert_keys(&value["api"]["vrf"], &["configured", "effective"]);
        assert_keys(&value["api"]["route_target"], &["configured", "effective"]);

        // Capability summary: the three blocks and their stable inner keys.
        assert_keys(&value["capabilities"], &["version", "rest", "graphql"]);
        assert_keys(
            &value["capabilities"]["version"],
            &["netbox", "minimum_supported", "compatible"],
        );
        assert_keys(
            &value["capabilities"]["rest"],
            &[
                "available",
                "search",
                "detail",
                "page_size",
                "exclude_config_context",
            ],
        );
        // A REST profile never probes GraphQL, so `error`/`surfaces` stay omitted.
        assert_keys(&value["capabilities"]["graphql"], &["probed", "available"]);
    }

    /// `nbox_search` → `SearchReport`: a ranked hit's key set, the `kind`
    /// serialization (snake_case enum, NOT the CLI short label), and the always-
    /// present `errors` array.
    #[tokio::test]
    async fn search_report_hit_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/dcim/devices/",
            json!({
                "id": 7, "url": "http://nb/api/dcim/devices/7/", "name": "edge01",
                "site": {"id": 1, "display": "den1"}
            }),
        )
        .await;
        mount_search_endpoints_empty(&mock).await;

        let Json(report) = server_for(&mock)
            .nbox_search(Parameters(search_args("edge01")))
            .await
            .expect("search");
        let value = serde_json::to_value(&report).expect("serialize report");

        assert_keys(&value, &["results", "errors"]);
        let hit = &value["results"][0];
        // A hit always carries kind/id/display/url/score; `subtitle` is present
        // here because the device has a site, but is skip-if-none in general.
        assert_keys(hit, &["kind", "id", "display", "url", "score", "subtitle"]);
        // `kind` is the snake_case ObjectKind discriminant. A device's stays
        // "device", but this pins the serde representation as the contract.
        assert_eq!(hit["kind"], "device");
        assert_eq!(hit["id"], 7);
        assert_eq!(hit["display"], "edge01");
        assert_eq!(hit["subtitle"], "den1");
        // A hit's `url` is the web-UI URL (the `/api` segment stripped), not the
        // raw NetBox API URL — pinned here as the contract a host links to.
        assert_eq!(hit["url"], "http://nb/dcim/devices/7/");
        // Exact-match query ranks highest.
        assert_eq!(hit["score"], 100);
        // Every endpoint succeeded → errors present and empty (fail-closed shape).
        assert!(value["errors"].as_array().expect("errors array").is_empty());
    }

    /// `nbox_search` hit `kind` for an IP address serializes as `ip_address`
    /// (the snake_case enum variant), distinct from the CLI's `ip` short label —
    /// a JSON-shape fact a host must not assume away. Pinned here as the contract.
    #[tokio::test]
    async fn search_report_ip_kind_serializes_snake_case() {
        let mock = MockServer::start().await;
        mount_empty(&mock, "/api/dcim/devices/").await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/ip-addresses/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{
                    "id": 3, "url": "http://nb/api/ipam/ip-addresses/3/",
                    "address": "10.0.0.1/24"
                }]
            })))
            .mount(&mock)
            .await;
        for p in [
            "/api/dcim/sites/",
            "/api/ipam/prefixes/",
            "/api/ipam/vlans/",
            "/api/circuits/circuits/",
            "/api/ipam/aggregates/",
            "/api/ipam/asns/",
            "/api/ipam/ip-ranges/",
            "/api/tenancy/tenants/",
            "/api/tenancy/contacts/",
            "/api/circuits/providers/",
            "/api/virtualization/virtual-machines/",
            "/api/virtualization/clusters/",
            "/api/dcim/racks/",
            "/api/ipam/vrfs/",
            "/api/ipam/route-targets/",
        ] {
            mount_empty(&mock, p).await;
        }

        let Json(report) = server_for(&mock)
            .nbox_search(Parameters(search_args("10.0.0.1")))
            .await
            .expect("search");
        let value = serde_json::to_value(&report).expect("serialize report");
        assert_eq!(value["results"][0]["kind"], "ip_address");
    }

    /// The partial-failure contract: a failed endpoint surfaces in `errors` while
    /// the successful endpoints' hits still come through in `results`.
    #[tokio::test]
    async fn search_report_partial_failure_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/dcim/devices/",
            json!({"id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01"}),
        )
        .await;
        // The sites endpoint fails; the rest are empty.
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
            "/api/tenancy/tenants/",
            "/api/tenancy/contacts/",
            "/api/circuits/providers/",
            "/api/virtualization/virtual-machines/",
            "/api/virtualization/clusters/",
            "/api/dcim/racks/",
            "/api/ipam/vrfs/",
            "/api/ipam/route-targets/",
        ] {
            mount_empty(&mock, p).await;
        }

        let Json(report) = server_for(&mock)
            .nbox_search(Parameters(search_args("edge")))
            .await
            .expect("search succeeds despite a per-endpoint failure");
        let value = serde_json::to_value(&report).expect("serialize report");

        assert_keys(&value, &["results", "errors"]);
        // Results survive the partial failure.
        assert_eq!(value["results"].as_array().expect("results").len(), 1);
        // `errors` is a flat array of human-readable strings naming the endpoint.
        let errors = value["errors"].as_array().expect("errors array");
        assert_eq!(errors.len(), 1);
        let msg = errors[0].as_str().expect("error is a string");
        assert!(msg.contains("sites"), "error names the endpoint: {msg}");
    }

    /// `nbox_get prefix` view shape: the prefix detail keys a host relies on. A
    /// representative populated prefix (with a child + a member IP) pins both the
    /// scalar contract and the two list sections.
    #[tokio::test]
    async fn get_prefix_view_shape_is_pinned() {
        let mock = MockServer::start().await;
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
        Mock::given(method("GET"))
            .and(path("/api/ipam/prefixes/"))
            .and(query_param("within", "10.44.208.0/24"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 6, "url": "u", "prefix": "10.44.208.0/26"}]
            })))
            .mount(&mock)
            .await;
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

        // The contract's mandatory keys plus the two populated list sections.
        // Optional scalars (vrf/vlan/scope/role/…) are skip-if-none and absent
        // here; their presence-when-populated is pinned by the golden contract.
        assert_keys(
            &value,
            &["prefix", "status", "child_prefixes", "ip_addresses"],
        );
        assert_eq!(value["prefix"], "10.44.208.0/24");
        assert_eq!(value["status"], "active");
        assert_eq!(value["child_prefixes"][0], "10.44.208.0/26");
        // A member IP row is itself an object with a stable key set.
        assert_keys(&value["ip_addresses"][0], &["address"]);
        assert_eq!(value["ip_addresses"][0]["address"], "10.44.208.1/24");
    }

    /// Contract item: a resource read of `nbox://{kind}/{ref}` returns byte-for-
    /// byte the same JSON view as `nbox_get` for that kind. The resource path is
    /// only a URI veneer over `get_impl`, and this pins that they cannot drift.
    #[tokio::test]
    async fn resource_read_matches_nbox_get() {
        // Two servers with identical mounts: one drives `nbox_get`, the other the
        // resource path. (A single disabled-cache server re-issues the fetch, so
        // separate servers keep the two reads independent and the mounts simple.)
        async fn mount_prefix(mock: &MockServer) {
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
                .mount(mock)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/ipam/prefixes/"))
                .and(query_param("within", "10.44.208.0/24"))
                .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
                .mount(mock)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/ipam/ip-addresses/"))
                .and(query_param("parent", "10.44.208.0/24"))
                .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
                .mount(mock)
                .await;
        }

        let get_mock = MockServer::start().await;
        mount_prefix(&get_mock).await;
        let Json(via_get) = server_for(&get_mock)
            .nbox_get(Parameters(get_args(GetKind::Prefix, "10.44.208.0/24")))
            .await
            .expect("prefix via nbox_get");

        let res_mock = MockServer::start().await;
        mount_prefix(&res_mock).await;
        let result = server_for(&res_mock)
            .read_resource_impl("nbox://prefix/10.44.208.0%2F24")
            .await
            .expect("prefix via resource");
        let via_resource = one_text(&result, "nbox://prefix/10.44.208.0%2F24");

        assert_eq!(
            via_get, via_resource,
            "resource read must return the same view as nbox_get"
        );
    }

    /// Error-mapping contract, pinned AS IT IS: not-found and ambiguous are
    /// caller-fixable → `invalid_params`; an upstream NetBox failure (HTTP 500)
    /// is not → `internal_error`. The not-found message stays actionable.
    #[tokio::test]
    async fn error_mapping_categories_are_pinned() {
        // Not found → invalid_params, with the ref echoed in the message.
        let nf = MockServer::start().await;
        mount_empty(&nf, "/api/dcim/devices/").await;
        let err: ErrorData = match server_for(&nf)
            .nbox_get(Parameters(get_args(GetKind::Device, "nope")))
            .await
        {
            Ok(_) => panic!("missing device should error"),
            Err(e) => e,
        };
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("nope"), "got: {}", err.message);

        // Ambiguous → invalid_params, candidate scopes listed for the caller.
        let amb = MockServer::start().await;
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
            .mount(&amb)
            .await;
        let err: ErrorData = match server_for(&amb)
            .nbox_get(Parameters(get_args(GetKind::Ip, "10.0.0.1")))
            .await
        {
            Ok(_) => panic!("ambiguous IP should error"),
            Err(e) => e,
        };
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("ambiguous"), "got: {}", err.message);

        // Upstream failure (HTTP 500) → internal_error, NOT invalid_params.
        let boom = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/prefixes/"))
            .and(query_param("prefix", "10.0.0.0/24"))
            .respond_with(ResponseTemplate::new(500).set_body_string("kaboom"))
            .mount(&boom)
            .await;
        let err: ErrorData = match server_for(&boom)
            .nbox_get(Parameters(get_args(GetKind::Prefix, "10.0.0.0/24")))
            .await
        {
            Ok(_) => panic!("a 500 should error"),
            Err(e) => e,
        };
        assert_eq!(
            err.code,
            ErrorCode::INTERNAL_ERROR,
            "an upstream failure is not caller-fixable: {}",
            err.message
        );
    }

    /// `nbox_next_ip` / `nbox_next_prefix` → `AvailableReport`: exactly the source
    /// prefix plus the available CIDR list. Both tools return this same report, so
    /// one pin fixes the schema for both. (The functional tests above check the
    /// values; this locks the *key set* so an added field can't drift silently.)
    #[tokio::test]
    async fn available_report_shape_is_pinned() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/prefixes/"))
            .and(query_param("prefix", "10.0.0.0/24"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 5, "url": "u", "prefix": "10.0.0.0/24"}]
            })))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/prefixes/5/available-ips/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"family": 4, "address": "10.0.0.1/24"}
            ])))
            .mount(&mock)
            .await;

        let Json(report) = server_for(&mock)
            .nbox_next_ip(Parameters(NextIpArgs {
                prefix: "10.0.0.0/24".to_string(),
                count: Some(1),
                vrf: None,
            }))
            .await
            .expect("next ip");
        let value = serde_json::to_value(&report).expect("serialize report");

        assert_keys(&value, &["prefix", "available"]);
        assert_eq!(value["prefix"], "10.0.0.0/24");
        assert_eq!(value["available"][0], "10.0.0.1/24");
    }

    /// `nbox_cache_clear` → `CacheClearReport`: a single confirmation field, so the
    /// tool advertises a concrete object output schema (not a bare string).
    #[tokio::test]
    async fn cache_clear_report_shape_is_pinned() {
        let mock = MockServer::start().await;
        let Json(report) = server_for(&mock)
            .nbox_cache_clear()
            .await
            .expect("cache clear");
        let value = serde_json::to_value(&report).expect("serialize report");

        assert_keys(&value, &["status"]);
        assert!(value["status"].is_string(), "status is a string");
    }

    /// `nbox_journal` → `JournalView`: a top-level `entries` array whose rows carry
    /// the flattened entry fields. `comments` is always present; `created`/`kind`/
    /// `author` are skip-if-none. This fixture carries all four (the `created_by`
    /// user resolves to the `author` label) so the FULL row key set is pinned —
    /// catching a drop/rename of `author`, which a minimal fixture would miss.
    #[tokio::test]
    async fn journal_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/dcim/devices/",
            json!({"id": 1, "url": "u", "name": "edge01"}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/api/extras/journal-entries/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{
                    "id": 5, "created": "2024-01-02",
                    "kind": {"value": "info", "label": "Info"},
                    "created_by": {"id": 1, "username": "neteng", "display": "neteng"},
                    "comments": "rebooted"
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

        assert_keys(&value, &["entries"]);
        let entry = &value["entries"][0];
        assert_keys(entry, &["created", "kind", "author", "comments"]);
        // `author` is the resolved user label (display), not the raw user object.
        assert_eq!(entry["author"], "neteng");
    }

    /// `nbox_list_tags` → `TagsView`: a top-level `tags` array of rows. `name`/
    /// `slug` are always present; `color`/`count` are skip-if-none and present in
    /// this fixture, so all four keys are pinned here.
    #[tokio::test]
    async fn tags_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/extras/tags/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 1, "name": "Critical", "slug": "critical",
                             "color": "ff0000", "tagged_items": 3}]
            })))
            .mount(&mock)
            .await;

        let Json(view) = server_for(&mock)
            .nbox_list_tags(Parameters(ListTagsArgs { limit: None }))
            .await
            .expect("list tags");
        let value = serde_json::to_value(&view).expect("serialize view");

        assert_keys(&value, &["tags"]);
        assert_keys(&value["tags"][0], &["name", "slug", "color", "count"]);
    }
}
