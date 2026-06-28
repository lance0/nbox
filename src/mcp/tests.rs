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
    GetArgs, GetKind, HistoryArgs, InterfaceArgs, JournalArgs, ListTagsArgs, NboxMcp, NextIpArgs,
    NextPrefixArgs, SearchArgs, TaggedArgs, parse_resource_uri, percent_decode, resource_templates,
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
        fields: None,
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
        None,
        String::new(),
    )
}

/// A server with the MCP read cache enabled for cache-behavior tests.
fn cached_server_for(mock: &MockServer) -> NboxMcp {
    let profile = ProfileConfig {
        url: mock.uri(),
        ..Default::default()
    };
    let cache = crate::cache::Cache::from_settings(
        "t".into(),
        &crate::config::CacheSettings {
            enabled: true,
            ttl_secs: 30,
        },
    );
    NboxMcp::new(
        NetBoxClient::new(&profile, None).unwrap(),
        cache,
        None,
        String::new(),
    )
}

/// A cached server whose entries expire immediately. This keeps TTL-expiry tests
/// deterministic without sleeping for the production minimum TTL.
fn zero_ttl_cached_server_for(mock: &MockServer) -> NboxMcp {
    let profile = ProfileConfig {
        url: mock.uri(),
        ..Default::default()
    };
    let cache = crate::cache::Cache::new(
        std::sync::Arc::new(crate::cache::MemoryStore::new()),
        "t".into(),
        crate::cache::CacheConfig {
            enabled: true,
            ttl_secs: 0,
        },
    );
    NboxMcp::new(
        NetBoxClient::new(&profile, None).unwrap(),
        cache,
        None,
        String::new(),
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
    let server = NboxMcp::new(
        NetBoxClient::new(&profile, None).unwrap(),
        cache,
        None,
        String::new(),
    );

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

#[tokio::test]
async fn read_resource_consults_nbox_get_cache() {
    // No reachable NetBox: a resource cache HIT must answer with no network.
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
    let server = NboxMcp::new(
        NetBoxClient::new(&profile, None).unwrap(),
        cache,
        None,
        String::new(),
    );

    let key = crate::cache::CacheKey::object("site", "iad1", "vrf=;site=;group=");
    server
        .cache
        .put(&key, &json!({ "name": "IAD1", "from_cache": true }));

    let result = server
        .read_resource_impl("nbox://site/iad1")
        .await
        .expect("served from cache without touching the network");
    let value = one_text(&result, "nbox://site/iad1");
    assert_eq!(value["name"], json!("IAD1"));
    assert_eq!(value["from_cache"], json!(true));
}

#[tokio::test]
async fn nbox_get_and_resource_share_cache_entry() {
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
        .expect(1)
        .mount(&mock)
        .await;

    let server = cached_server_for(&mock);
    let Json(via_get) = server
        .nbox_get(Parameters(get_args(GetKind::Site, "iad1")))
        .await
        .expect("site via nbox_get");
    let result = server
        .read_resource_impl("nbox://site/iad1")
        .await
        .expect("site via resource");
    let via_resource = one_text(&result, "nbox://site/iad1");

    assert_eq!(
        via_get, via_resource,
        "resource read must reuse the same cached view as nbox_get"
    );
    mock.verify().await;
}

#[tokio::test]
async fn nbox_get_cache_hit_makes_zero_origin_requests() {
    // A repeat `nbox_get` for the same object hits the read cache, not the
    // origin. The mock is mounted with `.expect(1)`: the second call must NOT
    // reach the wiremock server, or `verify()` fails.
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
        .expect(1)
        .mount(&mock)
        .await;

    let server = cached_server_for(&mock);
    let args = get_args(GetKind::Site, "iad1");
    let Json(first) = server
        .nbox_get(Parameters(args.clone()))
        .await
        .expect("first nbox_get fills the cache");
    assert_eq!(first["name"], json!("IAD1"));

    // Second read — same object, same args — must be a cache HIT: zero origin
    // requests. `.expect(1)` on the mock would fail if this touched the wire.
    let Json(second) = server
        .nbox_get(Parameters(args))
        .await
        .expect("second nbox_get served from cache");
    assert_eq!(second["name"], json!("IAD1"));

    mock.verify().await;
}

#[tokio::test]
async fn cache_clear_busts_resource_cache_entries() {
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
        .expect(2)
        .mount(&mock)
        .await;

    let server = cached_server_for(&mock);
    let first = server
        .read_resource_impl("nbox://site/iad1")
        .await
        .expect("first resource read fills cache");
    assert_eq!(one_text(&first, "nbox://site/iad1")["name"], json!("IAD1"));

    server.nbox_cache_clear().await.expect("cache clear");

    let second = server
        .read_resource_impl("nbox://site/iad1")
        .await
        .expect("second resource read refetches after clear");
    assert_eq!(one_text(&second, "nbox://site/iad1")["name"], json!("IAD1"));
    mock.verify().await;
}

#[tokio::test]
async fn resource_cache_refetches_after_ttl_expiry() {
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
        .expect(2)
        .mount(&mock)
        .await;

    let server = zero_ttl_cached_server_for(&mock);
    let first = server
        .read_resource_impl("nbox://site/iad1")
        .await
        .expect("first resource read fills cache");
    assert_eq!(one_text(&first, "nbox://site/iad1")["name"], json!("IAD1"));

    let second = server
        .read_resource_impl("nbox://site/iad1")
        .await
        .expect("second resource read refetches after immediate expiry");
    assert_eq!(one_text(&second, "nbox://site/iad1")["name"], json!("IAD1"));
    mock.verify().await;
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
            fields: None,
        }))
        .await
        .expect("device lookup");

    assert_eq!(value["name"], "edge01");
}

#[tokio::test]
async fn get_with_fields_projects_to_requested_keys() {
    // `fields` trims the returned object to the requested top-level keys (token
    // economy); an unknown key is silently ignored, and the projection happens
    // after the cache so it doesn't change what's fetched.
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
    for p in [
        "/api/dcim/interfaces/",
        "/api/ipam/ip-addresses/",
        "/api/ipam/services/",
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
            .mount(&mock)
            .await;
    }

    let Json(value) = server_for(&mock)
        .nbox_get(Parameters(GetArgs {
            kind: GetKind::Device,
            reference: "edge01".to_string(),
            vrf: None,
            site: None,
            group: None,
            fields: Some(vec!["name".to_string(), "nope".to_string()]),
        }))
        .await
        .expect("device lookup");

    // Only the requested, present key survives; the unknown `nope` is dropped.
    assert_eq!(value, json!({"name": "edge01"}));
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
            fields: None,
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
            owner: None,
            owner_group: None,
            vrf: None,
            fields: None,
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
    // `/api/authentication-check/` (4.5+) returns the flat `UserSerializer` body;
    // mount it so the `token` preflight resolves to `valid`.
    Mock::given(method("GET"))
        .and(path("/api/authentication-check/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1,
            "username": "admin",
            "display": "admin",
            "first_name": "",
            "last_name": "",
            "email": "admin@example.com"
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
    // The credential preflight resolves to the mocked user.
    assert_eq!(value["token"]["status"], "valid");
    assert_eq!(value["token"]["username"], "admin");
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
            owner: None,
            owner_group: None,
            vrf: None,
            fields: None,
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
    let ip_results: Vec<_> = (1u16..=254u16)
        .map(|host| {
            json!({
                "id": u64::from(1000 + host),
                "url": "u",
                "address": format!("10.44.208.{host}/24")
            })
        })
        .collect();
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
        .and(query_param("vrf_id", "null"))
        .and(query_param("limit", "200"))
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
        .and(query_param("vrf_id", "null"))
        .and(query_param("limit", "512"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 254, "next": null, "previous": null,
            "results": ip_results
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
    let addresses = value["ip_addresses"].as_array().expect("ip addresses");
    assert_eq!(addresses.len(), 254);
    assert_eq!(addresses[253]["address"], "10.44.208.254/24");
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
                "site": {"id": 1, "display": "iad1"},
                "group": {"id": 9, "display": "campus"}
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
    // vlan_group_scope: the independent follow-up that enriches group_scope.
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlan-groups/9/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 9, "name": "campus", "slug": "campus",
            "scope_type": "dcim.region",
            "scope": {"id": 5, "display": "us-east"}
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
    assert_eq!(value["group_scope"], "us-east");
    assert_eq!(value["group_scope_type"], "region");
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
            fields: None,
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
            fields: None,
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
            fields: None,
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
    // The circuit view also resolves its A/Z terminations.
    Mock::given(method("GET"))
        .and(path("/api/circuits/circuit-terminations/"))
        .and(query_param("circuit_id", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {
                    "id": 10, "term_side": "A",
                    "termination": {"id": 1, "display": "DC1", "name": "DC1"},
                    "termination_type": "dcim.site",
                    "link_peers": []
                },
                {
                    "id": 11, "term_side": "Z",
                    "termination": {"id": 2, "display": "ACME Cloud"},
                    "termination_type": "circuits.providernetwork",
                    "link_peers": []
                }
            ]
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
    // The A/Z terminations are surfaced, A first.
    assert_eq!(value["terminations"][0]["side"], "A");
    assert_eq!(value["terminations"][0]["endpoint"], "DC1");
    assert_eq!(value["terminations"][0]["endpoint_kind"], "site");
    assert_eq!(value["terminations"][1]["side"], "Z");
    assert_eq!(
        value["terminations"][1]["endpoint_kind"],
        "provider network"
    );
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
async fn history_returns_changes_for_device() {
    let mock = MockServer::start().await;
    // resolve_content_type_id(Device) → device_by_ref (exact name).
    mount_one(
        &mock,
        "/api/dcim/devices/",
        json!({"id": 1, "url": "u", "name": "edge01"}),
    )
    .await;
    // object_changes filtered by changed_object_type + changed_object_id.
    Mock::given(method("GET"))
        .and(path("/api/core/object-changes/"))
        .and(query_param("changed_object_type", "dcim.device"))
        .and(query_param("changed_object_id", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [{
                "id": 10, "action": {"value": "update", "label": "Updated"},
                "time": "2025-12-08T23:56:49Z", "user_name": "neteng",
                "object_repr": "edge01", "message": "",
                "request_id": "bc44-0001",
                "prechange_data": {"name": "edge01", "status": "active"},
                "postchange_data": {"name": "edge01", "status": "decommissioned", "site_id": 2}
            }, {
                "id": 9, "action": {"value": "create", "label": "Created"},
                "time": "2025-11-01T00:00:00Z", "user_name": "fleetops",
                "object_repr": "edge01", "message": "initial provisioning",
                "request_id": "bc44-0002",
                "prechange_data": null,
                "postchange_data": null
            }]
        })))
        .mount(&mock)
        .await;

    let Json(view) = server_for(&mock)
        .nbox_history(Parameters(HistoryArgs {
            kind: GetKind::Device,
            reference: "edge01".to_string(),
            limit: None,
            diff: None,
        }))
        .await
        .expect("history");

    let value = serde_json::to_value(&view).expect("serialize view");
    let entries = value["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 2);
    // Update row: fields changed are site_id (added) + status (changed); name unchanged.
    assert_eq!(entries[0]["action"], "update");
    assert_eq!(entries[0]["user"], "neteng");
    assert_eq!(entries[0]["fields_changed"], json!(["site_id", "status"]));
    // Create row: no pre/post data → no fields_changed; message surfaces.
    assert_eq!(entries[1]["action"], "create");
    assert_eq!(entries[1]["message"], "initial provisioning");
    // fields_changed is empty for the create row → omitted (skip_serializing_if).
    assert!(entries[1].get("fields_changed").is_none() || entries[1]["fields_changed"].is_null());
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
async fn tagged_returns_objects_carrying_a_tag() {
    // `nbox_tagged` resolves the tag (here by name) then lists the polymorphic
    // tagged-objects rows. Addresses are RFC-reserved; names are synthetic.
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/extras/tags/"))
        .and(query_param("name", "prod:us-east"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 42, "name": "prod:us-east", "slug": "produs-east"}]
        })))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/extras/tagged-objects/"))
        .and(query_param("tag_id", "42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "http://nb/api/extras/tagged-objects/1/",
                 "object_type": "dcim.device", "object_id": 7,
                 "object": {"id": 7, "name": "edge01", "display": "edge01",
                            "url": "http://nb/api/dcim/devices/7/"},
                 "tag": {"id": 42, "name": "prod:us-east", "slug": "produs-east"},
                 "display": "edge01 tagged with prod:us-east"},
                {"id": 2, "url": "http://nb/api/extras/tagged-objects/2/",
                 "object_type": "ipam.prefix", "object_id": 9,
                 "object": {"id": 9, "display": "10.0.0.0/24",
                            "url": "http://nb/api/ipam/prefixes/9/"},
                 "tag": {"id": 42, "name": "prod:us-east", "slug": "produs-east"},
                 "display": "10.0.0.0/24 tagged with prod:us-east"}
            ]
        })))
        .mount(&mock)
        .await;

    let Json(report) = server_for(&mock)
        .nbox_tagged(Parameters(TaggedArgs {
            tag: "prod:us-east".into(),
            limit: None,
        }))
        .await
        .expect("tagged");

    assert_eq!(report.tag.id, 42);
    assert_eq!(report.tag.name, "prod:us-east");
    assert_eq!(report.results.len(), 2);
    // The friendly `kind` is mapped to nbox's labels (device/prefix), not the
    // raw dotted type.
    assert_eq!(report.results[0].kind, "device");
    assert_eq!(report.results[0].display, "edge01");
    assert_eq!(report.results[1].kind, "prefix");
    assert_eq!(report.results[1].display, "10.0.0.0/24");
}

#[tokio::test]
async fn tagged_returns_invalid_params_for_a_no_match_tag() {
    // A tag that resolves to nothing is caller-fixable → invalid_params, matching
    // the CLI's not-found (exit 4).
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/extras/tags/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&mock)
        .await;

    let err: ErrorData = match server_for(&mock)
        .nbox_tagged(Parameters(TaggedArgs {
            tag: "nonexistent".into(),
            limit: None,
        }))
        .await
    {
        Ok(_) => panic!("no-match tag should error"),
        Err(e) => e,
    };
    assert!(
        err.message.contains("no tag matched \"nonexistent\""),
        "got: {}",
        err.message
    );
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
async fn read_resource_returns_virtual_circuit_view() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/circuits/virtual-circuits/"))
        .and(query_param("cid", "VC-100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 7, "url": "u", "cid": "VC-100",
                "status": {"value": "active", "label": "Active"}
            }]
        })))
        .mount(&mock)
        .await;
    // terminations fan-out (empty).
    Mock::given(method("GET"))
        .and(path("/api/circuits/virtual-circuit-terminations/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&mock)
        .await;

    let result = server_for(&mock)
        .read_resource_impl("nbox://virtual_circuit/VC-100")
        .await
        .expect("read virtual-circuit resource");
    let value = one_text(&result, "nbox://virtual_circuit/VC-100");
    assert_eq!(value["cid"], "VC-100");
    // The resource view matches the nbox_get view (cid + status; empty
    // terminations omitted).
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["cid", "status"]);
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
        ErrorCode, GetKind, HistoryArgs, JournalArgs, Json, ListTagsArgs, Mock, MockServer,
        NextIpArgs, Parameters, ResponseTemplate, SearchArgs, TaggedArgs, empty_page, get_args,
        method, mount_empty, mount_one, one_text, path, query_param, server_for,
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
            owner: None,
            owner_group: None,
            vrf: None,
            fields: None,
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
        Mock::given(method("GET"))
            .and(path("/api/authentication-check/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "username": "admin",
                "display": "admin (Alice Admin)",
                "first_name": "Alice",
                "last_name": "Admin",
                "email": "admin@example.com"
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
                "token",
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

        // Credential preflight: the `token` verdict carries the discriminator and,
        // on `valid`, the resolved identity (`display` is distinct from `username`).
        assert_eq!(value["token"]["status"], "valid");
        assert_keys(&value["token"], &["status", "username", "display"]);
        assert_eq!(value["token"]["username"], "admin");
        assert_eq!(value["token"]["display"], "admin (Alice Admin)");
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

    /// `nbox_search` with `fields` trims each hit in `results` to the requested
    /// top-level keys (token economy), while leaving the `errors` array intact so
    /// partial-failure reporting still comes through.
    #[tokio::test]
    async fn search_with_fields_projects_each_hit() {
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

        let mut args = search_args("edge01");
        args.fields = Some(vec!["kind".to_string(), "display".to_string()]);
        let Json(value) = server_for(&mock)
            .nbox_search(Parameters(args))
            .await
            .expect("search");

        // The hit is trimmed to exactly the two requested keys.
        let hit = &value["results"][0];
        assert_keys(hit, &["kind", "display"]);
        assert_eq!(hit["kind"], "device");
        assert_eq!(hit["display"], "edge01");
        // `errors` is not projected away — fail-closed reporting survives.
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

    #[tokio::test]
    async fn prefix_contained_ip_section_stops_at_dedicated_cap() {
        let mock = MockServer::start().await;
        let addresses: Vec<_> = (0..512)
            .map(|i| {
                json!({
                    "id": i + 1,
                    "url": "u",
                    "address": format!("10.44.{}.{}/22", i / 256, i % 256),
                })
            })
            .collect();

        Mock::given(method("GET"))
            .and(path("/api/ipam/prefixes/"))
            .and(query_param("prefix", "10.44.0.0/22"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{
                    "id": 5, "url": "u", "prefix": "10.44.0.0/22",
                    "status": {"value": "active", "label": "Active"}
                }]
            })))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/prefixes/"))
            .and(query_param("within", "10.44.0.0/22"))
            .and(query_param("limit", "200"))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/ip-addresses/"))
            .and(query_param("parent", "10.44.0.0/22"))
            .and(query_param("limit", "512"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 513,
                "next": "http://nb/api/ipam/ip-addresses/?offset=512",
                "previous": null,
                "results": addresses
            })))
            .mount(&mock)
            .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Prefix, "10.44.0.0/22")))
            .await
            .expect("prefix lookup");

        let ips = value["ip_addresses"]
            .as_array()
            .expect("ip_addresses is an array");
        assert_eq!(ips.len(), 512, "contained-IP section cap drifted");
        assert_eq!(ips.last().unwrap()["address"], "10.44.1.255/22");
        assert!(
            !ips.iter().any(|ip| ip["address"] == "10.44.2.0/22"),
            "row 513 should remain outside the capped detail section"
        );
    }

    /// Contract item: a resource read of `nbox://{kind}/{ref}` returns byte-for-
    /// byte the same JSON view as `nbox_get` for that kind. The resource path is
    /// only a URI veneer over `get_cached`, and this pins that they cannot drift.
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

    /// `nbox_history` → `HistoryView`: a top-level `entries` array whose rows
    /// carry `time`, `action`, `action_label`, `user`, `object`, `message`,
    /// `fields_changed`, and `request_id`. The empty/skip-if-none fields are
    /// omitted here so the present key set is exact, and `fields_changed` lists
    /// the top-level fields whose values differ (pre vs post) — not the full
    /// before/after JSON.
    #[tokio::test]
    async fn history_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/dcim/devices/",
            json!({"id": 1, "url": "u", "name": "edge01"}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/api/core/object-changes/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{
                    "id": 10,
                    "action": {"value": "update", "label": "Updated"},
                    "time": "2025-12-08T23:56:49Z",
                    "user_name": "neteng",
                    "object_repr": "edge01",
                    "message": "",
                    "request_id": "bc44-0001",
                    "prechange_data": {"name": "edge01", "status": "active"},
                    "postchange_data": {"name": "edge01", "status": "decommissioned", "site_id": 2}
                }]
            })))
            .mount(&mock)
            .await;

        let Json(view) = server_for(&mock)
            .nbox_history(Parameters(HistoryArgs {
                kind: GetKind::Device,
                reference: "edge01".to_string(),
                limit: None,
                diff: None,
            }))
            .await
            .expect("history");
        let value = serde_json::to_value(&view).expect("serialize view");

        assert_keys(&value, &["entries"]);
        let entry = &value["entries"][0];
        // message is empty here so it's skip-if-empty → omitted.
        assert_keys(
            entry,
            &[
                "time",
                "action",
                "action_label",
                "user",
                "object",
                "fields_changed",
                "request_id",
            ],
        );
        assert_eq!(entry["action"], "update");
        assert_eq!(entry["action_label"], "Updated");
        assert_eq!(entry["user"], "neteng");
        assert_eq!(entry["object"], "edge01");
        // status (changed), site_id (added); name unchanged → not listed.
        assert_eq!(entry["fields_changed"], json!(["site_id", "status"]));
        assert_eq!(entry["request_id"], "bc44-0001");
    }

    /// `nbox_history` with `diff=true` includes the full `before`/`after` change
    /// payloads (absent otherwise), so an agent can inspect one change in full.
    #[tokio::test]
    async fn history_diff_includes_before_after_payloads() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/dcim/devices/",
            json!({"id": 1, "url": "u", "name": "edge01"}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/api/core/object-changes/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{
                    "id": 10,
                    "action": {"value": "update", "label": "Updated"},
                    "time": "2025-12-08T23:56:49Z",
                    "user_name": "neteng",
                    "object_repr": "edge01",
                    "message": "",
                    "request_id": "bc44-0001",
                    "prechange_data": {"name": "edge01", "status": "active"},
                    "postchange_data": {"name": "edge01", "status": "decommissioned", "site_id": 2}
                }]
            })))
            .mount(&mock)
            .await;

        // diff=false (default): compact row, no before/after keys.
        let Json(compact) = server_for(&mock)
            .nbox_history(Parameters(HistoryArgs {
                kind: GetKind::Device,
                reference: "edge01".to_string(),
                limit: Some(1),
                diff: Some(false),
            }))
            .await
            .expect("compact history");
        let compact_val = serde_json::to_value(&compact).expect("serialize");
        assert!(compact_val["entries"][0].get("before").is_none());
        assert!(compact_val["entries"][0].get("after").is_none());

        // diff=true: the full pre/post payloads surface as before/after.
        let Json(full) = server_for(&mock)
            .nbox_history(Parameters(HistoryArgs {
                kind: GetKind::Device,
                reference: "edge01".to_string(),
                limit: Some(1),
                diff: Some(true),
            }))
            .await
            .expect("diff history");
        let full_val = serde_json::to_value(&full).expect("serialize");
        let entry = &full_val["entries"][0];
        assert_eq!(
            entry["before"],
            json!({"name": "edge01", "status": "active"})
        );
        assert_eq!(
            entry["after"],
            json!({"name": "edge01", "status": "decommissioned", "site_id": 2})
        );
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

    /// `nbox_tagged` → `TaggedReport`: a top-level `tag` (the resolved
    /// reference) + a `results` array. Each row carries the friendly `kind`, the
    /// dotted `object_type`, and the object's `id`/`display`/`url` — the keys an
    /// agent reads to answer "what has tag X". Two rows of different kinds pin
    /// the row shape for both the device and a non-device content type.
    #[tokio::test]
    async fn tagged_report_shape_is_pinned() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/extras/tags/"))
            .and(query_param("name", "prod:us-east"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 42, "name": "prod:us-east", "slug": "produs-east"}]
            })))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/extras/tagged-objects/"))
            .and(query_param("tag_id", "42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 2, "next": null, "previous": null,
                "results": [
                    {"id": 1, "url": "http://nb/api/extras/tagged-objects/1/",
                     "object_type": "dcim.device", "object_id": 7,
                     "object": {"id": 7, "display": "edge01",
                                "url": "http://nb/api/dcim/devices/7/"},
                     "tag": {"id": 42, "name": "prod:us-east", "slug": "produs-east"},
                     "display": "edge01 tagged with prod:us-east"},
                    {"id": 2, "url": "http://nb/api/extras/tagged-objects/2/",
                     "object_type": "ipam.prefix", "object_id": 9,
                     "object": {"id": 9, "display": "10.0.0.0/24",
                                "url": "http://nb/api/ipam/prefixes/9/"},
                     "tag": {"id": 42, "name": "prod:us-east", "slug": "produs-east"},
                     "display": "10.0.0.0/24 tagged with prod:us-east"}
                ]
            })))
            .mount(&mock)
            .await;

        let Json(report) = server_for(&mock)
            .nbox_tagged(Parameters(TaggedArgs {
                tag: "prod:us-east".to_string(),
                limit: None,
            }))
            .await
            .expect("tagged");
        let value = serde_json::to_value(&report).expect("serialize report");

        assert_keys(&value, &["tag", "results"]);
        assert_keys(&value["tag"], &["id", "name", "slug"]);
        assert_eq!(value["tag"]["id"], 42);
        assert_keys(
            &value["results"][0],
            &["kind", "object_type", "id", "display", "url"],
        );
        // The friendly `kind` is mapped from the dotted type (device, not
        // dcim.device), for both a device and a non-device content type.
        assert_eq!(value["results"][0]["kind"], "device");
        assert_eq!(value["results"][0]["object_type"], "dcim.device");
        assert_eq!(value["results"][1]["kind"], "prefix");
        assert_eq!(value["results"][1]["object_type"], "ipam.prefix");
    }

    /// `nbox_get interface` view shape: the `InterfaceView` keys a host reads.
    /// `name`/`type` are mandatory (always present); optional scalars
    /// (`device`/`enabled`/`mtu`/`mac_address`/`mode`/… ) are skip-if-none and
    /// present in this fixture, so the FULL scalar set is pinned. The `ref` is
    /// `<device>/<name>` — the interface kind's compound reference.
    #[tokio::test]
    async fn get_interface_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        // resolve_interface: device lookup, then interface by device_id+name.
        mount_one(
            &mock,
            "/api/dcim/devices/",
            json!({"id": 1, "url": "u", "name": "edge01"}),
        )
        .await;
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
                    "type": {"value": "10gbase-x-sfpp", "label": "SFP+ (10GE)"},
                    "mtu": 9000, "mac_address": "aa:bb:cc:dd:ee:ff",
                    "mode": {"value": "access", "label": "Access"}
                }]
            })))
            .mount(&mock)
            .await;
        mount_empty(&mock, "/api/ipam/ip-addresses/").await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/interfaces/42/trace/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&mock)
            .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Interface, "edge01/xe-0/0/0")))
            .await
            .expect("interface lookup");

        // `name` is the only mandatory scalar; the optional scalars present in
        // this fixture (device/enabled/type/mtu/mac_address/mode) pin them. The
        // list sections (tagged_vlans/ip_addresses/trace/connected_to) are
        // skip-if-empty and absent here (empty fixture) — their presence-when-
        // populated is exercised by the dedicated interface tests.
        assert_keys(
            &value,
            &[
                "name",
                "device",
                "enabled",
                "type",
                "mtu",
                "mac_address",
                "mode",
            ],
        );
        assert_eq!(value["name"], "xe-0/0/0");
        assert_eq!(value["type"], "SFP+ (10GE)");
    }

    /// `nbox_journal` with an interface `kind`/`ref` (`<device>/<name>`): pins
    /// that the journal resolver accepts the compound interface reference — the
    /// one kind without a single-string ref — and surfaces the same `JournalView`
    /// shape as the other kinds.
    #[tokio::test]
    async fn journal_interface_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/dcim/devices/",
            json!({"id": 1, "url": "u", "name": "edge01"}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/interfaces/"))
            .and(query_param("device_id", "1"))
            .and(query_param("name", "xe-0/0/0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 42, "url": "u", "name": "xe-0/0/0",
                              "device": {"id": 1, "display": "edge01"}}]
            })))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/extras/journal-entries/"))
            .and(query_param("assigned_object_type", "dcim.interface"))
            .and(query_param("assigned_object_id", "42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{
                    "id": 5, "created": "2024-01-02",
                    "kind": {"value": "info", "label": "Info"},
                    "comments": "flapped overnight"
                }]
            })))
            .mount(&mock)
            .await;

        let Json(view) = server_for(&mock)
            .nbox_journal(Parameters(JournalArgs {
                kind: GetKind::Interface,
                reference: "edge01/xe-0/0/0".to_string(),
                limit: None,
            }))
            .await
            .expect("interface journal");
        let value = serde_json::to_value(&view).expect("serialize view");

        assert_keys(&value, &["entries"]);
        // `comments` always present; `created`/`kind` present here; `author`
        // skip-if-none (no `created_by` in this fixture).
        assert_keys(&value["entries"][0], &["created", "kind", "comments"]);
        assert_eq!(value["entries"][0]["comments"], "flapped overnight");
    }

    /// `nbox_journal virtual_circuit` resolves the VC by CID, then fetches
    /// journal entries by the `circuits.virtualcircuit` content type — this
    /// pins that the resolver emits the right content-type string (the one
    /// path not live-verifiable without `view_virtualcircuit` permission).
    #[tokio::test]
    async fn journal_virtual_circuit_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/circuits/virtual-circuits/",
            json!({"id": 7, "url": "u", "cid": "VC-100"}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/api/extras/journal-entries/"))
            .and(query_param(
                "assigned_object_type",
                "circuits.virtualcircuit",
            ))
            .and(query_param("assigned_object_id", "7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{
                    "id": 5, "created": "2024-01-02",
                    "kind": {"value": "info", "label": "Info"},
                    "comments": "vc provisioned"
                }]
            })))
            .mount(&mock)
            .await;

        let Json(view) = server_for(&mock)
            .nbox_journal(Parameters(JournalArgs {
                kind: GetKind::VirtualCircuit,
                reference: "VC-100".to_string(),
                limit: None,
            }))
            .await
            .expect("virtual-circuit journal");
        let value = serde_json::to_value(&view).expect("serialize view");
        assert_keys(&value, &["entries"]);
        assert_eq!(value["entries"][0]["comments"], "vc provisioned");
    }

    // ---- per-kind `nbox_get` view shapes ---------------------------------
    //
    // Each pins the JSON key set a host reads for one object of that kind. The
    // fixtures carry the optional scalars populated, so the FULL scalar set is
    // pinned (catching a rename/drop). List sections that are skip-if-empty are
    // left empty here (mounting one of each row is the dedicated-view tests'
    // job) — a populated list section would just pin the row shape, already
    // covered for prefix/ip by the prefix-view test above. Addresses are
    // RFC-reserved; names are synthetic.

    /// `nbox_get device` → `DeviceDetail`: a `summary` (the `DeviceView`) +
    /// skip-if-empty list sections (interfaces/ip_addresses/cables/vlans/
    /// services). A device with no fan-out pins just `summary` — the row shapes
    /// are exercised by the dedicated interface/IP tests.
    #[tokio::test]
    async fn get_device_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/dcim/devices/",
            json!({
                "id": 7, "url": "u", "name": "edge01",
                "status": {"value": "active", "label": "Active"},
                "owner": {"id": 7, "display": "netops"},
                "custom_fields": {}
            }),
        )
        .await;
        for ep in [
            "/api/dcim/interfaces/",
            "/api/ipam/ip-addresses/",
            "/api/ipam/services/",
        ] {
            mount_empty(&mock, ep).await;
        }

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Device, "edge01")))
            .await
            .expect("device lookup");
        assert_keys(&value, &["id", "name", "status", "owner"]);
        assert_eq!(value["name"], "edge01");
        assert_eq!(value["owner"], "netops");
    }

    /// `nbox_get ip` → `IpView`: address + the optional parent-prefix/assigned/
    /// scope scalars. `parent_prefix` is populated here (the most-specific
    /// containing prefix) so it's pinned too.
    #[tokio::test]
    async fn get_ip_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/ipam/ip-addresses/",
            json!({
                "id": 9, "url": "u", "address": "10.0.0.1/24",
                "status": {"value": "active", "label": "Active"},
                "custom_fields": {}
            }),
        )
        .await;
        // most-specific containing prefix → parent_prefix populated.
        mount_one(
            &mock,
            "/api/ipam/prefixes/",
            json!({"id": 5, "url": "u", "prefix": "10.0.0.0/24"}),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Ip, "10.0.0.1")))
            .await
            .expect("ip lookup");
        assert_keys(&value, &["address", "status", "parent_prefix"]);
        assert_eq!(value["address"], "10.0.0.1/24");
        assert_eq!(value["parent_prefix"], "10.0.0.0/24");
    }

    /// `nbox_get vlan` → `VlanView`: vid + name + the scope/role scalars. The
    /// `prefixes` list is always present (not skip-if-empty).
    #[tokio::test]
    async fn get_vlan_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/ipam/vlans/",
            json!({
                "id": 4, "url": "u", "vid": 208, "name": "mgmt",
                "status": {"value": "active", "label": "Active"},
                "custom_fields": {}
            }),
        )
        .await;
        // vlan_prefixes fan-out (empty here).
        mount_empty(&mock, "/api/ipam/prefixes/").await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Vlan, "208")))
            .await
            .expect("vlan lookup");
        assert_keys(&value, &["vid", "name", "status", "prefixes"]);
        assert_eq!(value["vid"], 208);
    }

    /// `nbox_get site` → `SiteView`: id + name + slug (mandatory) + the optional
    /// status/region/group/tenant scalars.
    #[tokio::test]
    async fn get_site_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/dcim/sites/",
            json!({
                "id": 1, "url": "u", "name": "iad1", "slug": "iad1",
                "status": {"value": "active", "label": "Active"},
                "custom_fields": {}
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Site, "iad1")))
            .await
            .expect("site lookup");
        assert_keys(&value, &["id", "name", "slug", "status"]);
        assert_eq!(value["name"], "iad1");
    }

    /// `nbox_get rack` → `RackView`: id + name + the optional site/status/
    /// role/tenant scalars.
    #[tokio::test]
    async fn get_rack_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/dcim/racks/",
            json!({
                "id": 3, "url": "u", "name": "R1",
                "status": {"value": "active", "label": "Active"},
                "custom_fields": {}
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Rack, "R1")))
            .await
            .expect("rack lookup");
        assert_keys(&value, &["id", "name", "status"]);
    }

    /// `nbox_get rack_group` → `RackGroupView`: id/name/slug + the optional
    /// description/owner scalars + the skip-if-none `rack_count` relation
    /// count. Pins the present-`owner` case (4.5+) for the new 4.6 kind.
    #[tokio::test]
    async fn get_rack_group_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/dcim/rack-groups/",
            json!({
                "id": 4, "url": "u", "name": "Row A", "slug": "row-a",
                "description": "Aisle A racks",
                "owner": {"id": 7, "display": "netops"},
                "rack_count": 12,
                "custom_fields": {}
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::RackGroup, "row-a")))
            .await
            .expect("rack-group lookup");
        assert_keys(
            &value,
            &["id", "name", "slug", "description", "owner", "rack_count"],
        );
        assert_eq!(value["slug"], "row-a");
        assert_eq!(value["owner"], "netops");
        assert_eq!(value["rack_count"], 12);
    }

    /// `nbox_get circuit` → `CircuitView`: cid + the optional provider/type/
    /// status scalars + the skip-if-empty `terminations` list.
    #[tokio::test]
    async fn get_circuit_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/circuits/circuits/",
            json!({
                "id": 2, "url": "u", "cid": "C-100",
                "status": {"value": "active", "label": "Active"},
                "custom_fields": {}
            }),
        )
        .await;
        // terminations fan-out (empty here).
        mount_empty(&mock, "/api/circuits/circuit-terminations/").await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Circuit, "C-100")))
            .await
            .expect("circuit lookup");
        assert_keys(&value, &["cid", "status"]);
        assert_eq!(value["cid"], "C-100");
    }

    /// `nbox_get virtual_circuit` → `VirtualCircuitView`: cid + the optional
    /// provider_network/provider_account/type/status/tenant/owner/description
    /// scalars + the skip-if-empty `terminations`/`tags`/`custom_fields`. The
    /// termination row carries `endpoint` + the skip-if-none `device`/
    /// `interface`/`role`/`description` refs — the structured form an agent
    /// navigates by (no A/Z sides, no cable diagram: virtual circuits are
    /// multi-point overlays on interfaces).
    #[tokio::test]
    async fn get_virtual_circuit_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/circuits/virtual-circuits/",
            json!({
                "id": 7, "url": "u", "cid": "VC-100",
                "provider_network": {"id": 3, "name": "ACME Cloud"},
                "provider_account": {"id": 9, "name": "primary", "account": "ACME-001"},
                "type": {"id": 2, "name": "MPLS", "slug": "mpls"},
                "status": {"value": "active", "label": "Active"},
                "tenant": {"id": 4, "name": "acme"},
                "owner": {"id": 1, "name": "netops"},
                "description": "east-west overlay",
                "tags": [{"id": 1, "name": "transit", "slug": "transit"}],
                "custom_fields": {"sla": "gold"}
            }),
        )
        .await;
        // One termination landing on a device interface (with a role).
        mount_one(
            &mock,
            "/api/circuits/virtual-circuit-terminations/",
            json!({
                "id": 10,
                "interface": {
                    "id": 50, "name": "xe-0/0/0",
                    "device": {"id": 8, "name": "edge01"}
                },
                "role": {"value": "hub", "label": "Hub"},
                "description": "a-side"
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::VirtualCircuit, "VC-100")))
            .await
            .expect("virtual-circuit lookup");
        assert_keys(
            &value,
            &[
                "cid",
                "provider_network",
                "provider_account",
                "type",
                "status",
                "tenant",
                "owner",
                "description",
                "terminations",
                "tags",
                "custom_fields",
            ],
        );
        assert_eq!(value["cid"], "VC-100");
        // The termination row's full key set is pinned too.
        assert_keys(
            &value["terminations"][0],
            &["endpoint", "device", "interface", "role", "description"],
        );
        assert_eq!(value["terminations"][0]["endpoint"], "edge01 xe-0/0/0");
        assert_eq!(value["terminations"][0]["device"]["id"], 8);
        assert_eq!(value["terminations"][0]["interface"]["name"], "xe-0/0/0");
    }

    /// `nbox_get aggregate` → `AggregateView`: prefix + the optional rir/tenant
    /// scalars.
    #[tokio::test]
    async fn get_aggregate_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/ipam/aggregates/",
            json!({
                "id": 1, "url": "u", "prefix": "10.0.0.0/8",
                "custom_fields": {}
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Aggregate, "10.0.0.0/8")))
            .await
            .expect("aggregate lookup");
        assert_keys(&value, &["prefix"]);
        assert_eq!(value["prefix"], "10.0.0.0/8");
    }

    /// `nbox_get asn` → `AsnView`: asn (the integer, not an id) + the optional
    /// rir/tenant scalars.
    #[tokio::test]
    async fn get_asn_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/ipam/asns/",
            json!({
                "id": 1, "url": "u", "asn": 65000,
                "custom_fields": {}
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Asn, "65000")))
            .await
            .expect("asn lookup");
        assert_keys(&value, &["asn"]);
        assert_eq!(value["asn"], 65000);
    }

    /// `nbox_get ip_range` → `IpRangeView`: start/end (mandatory) + the optional
    /// size/status/vrf scalars.
    #[tokio::test]
    async fn get_ip_range_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/ipam/ip-ranges/",
            json!({
                "id": 1, "url": "u", "start_address": "10.0.0.10",
                "end_address": "10.0.0.20", "size": 11,
                "custom_fields": {}
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::IpRange, "10.0.0.10")))
            .await
            .expect("ip-range lookup");
        assert_keys(&value, &["start_address", "end_address", "size"]);
        assert_eq!(value["size"], 11);
    }

    /// `nbox_get tenant` → `TenantView`: id + name + slug + the optional
    /// description/group scalars.
    #[tokio::test]
    async fn get_tenant_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/tenancy/tenants/",
            json!({
                "id": 1, "url": "u", "name": "Engineering", "slug": "eng",
                "custom_fields": {}
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Tenant, "eng")))
            .await
            .expect("tenant lookup");
        assert_keys(&value, &["id", "name", "slug"]);
        assert_eq!(value["slug"], "eng");
    }

    /// `nbox_get contact` → `ContactView`: id + name + the optional title/email/
    /// group scalars.
    #[tokio::test]
    async fn get_contact_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/tenancy/contacts/",
            json!({
                "id": 1, "url": "u", "name": "Jane Q.",
                "custom_fields": {}
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Contact, "Jane Q.")))
            .await
            .expect("contact lookup");
        assert_keys(&value, &["id", "name"]);
    }

    /// `nbox_get provider` → `ProviderView`: id + name + slug + the skip-if-empty
    /// `asns`/`accounts` lists.
    #[tokio::test]
    async fn get_provider_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/circuits/providers/",
            json!({
                "id": 1, "url": "u", "name": "Acme", "slug": "acme",
                "custom_fields": {}
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Provider, "acme")))
            .await
            .expect("provider lookup");
        assert_keys(&value, &["id", "name", "slug"]);
        assert_eq!(value["slug"], "acme");
    }

    /// `nbox_get vm` → `VmView`: id + name + the optional status/cluster/tenant/
    /// primary_ip scalars.
    #[tokio::test]
    async fn get_vm_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/virtualization/virtual-machines/",
            json!({
                "id": 1, "url": "u", "name": "vm01",
                "status": {"value": "active", "label": "Active"},
                "custom_fields": {}
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Vm, "vm01")))
            .await
            .expect("vm lookup");
        assert_keys(&value, &["id", "name", "status"]);
    }

    /// `nbox_get vm_type` → `VirtualMachineTypeView`: id/name/slug + the
    /// optional default_platform/default_vcpus/default_memory/description/owner
    /// scalars + the skip-if-none `virtual_machine_count` relation count. Pins
    /// the present-`owner` case (4.5+) and the numeric defaults for the new 4.6
    /// kind.
    #[tokio::test]
    async fn get_vm_type_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/virtualization/virtual-machine-types/",
            json!({
                "id": 5, "url": "u", "name": "web-tier", "slug": "web-tier",
                "default_platform": {"id": 2, "display": "debian"},
                "default_vcpus": 4.0,
                "default_memory": 8192,
                "description": "2 vCPU / 8 GiB web frontend",
                "owner": {"id": 7, "display": "netops"},
                "virtual_machine_count": 18,
                "custom_fields": {}
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::VmType, "web-tier")))
            .await
            .expect("vm-type lookup");
        assert_keys(
            &value,
            &[
                "id",
                "name",
                "slug",
                "default_platform",
                "default_vcpus",
                "default_memory",
                "description",
                "owner",
                "virtual_machine_count",
            ],
        );
        assert_eq!(value["slug"], "web-tier");
        assert_eq!(value["default_platform"], "debian");
        assert_eq!(value["owner"], "netops");
        assert_eq!(value["virtual_machine_count"], 18);
    }

    /// `nbox_get cluster` → `ClusterView`: id + name + the optional type/group/
    /// status/scope scalars.
    #[tokio::test]
    async fn get_cluster_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/virtualization/clusters/",
            json!({
                "id": 1, "url": "u", "name": "prod-cluster",
                "custom_fields": {}
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Cluster, "prod-cluster")))
            .await
            .expect("cluster lookup");
        assert_keys(&value, &["id", "name"]);
    }

    /// `nbox_get vrf` → `VrfDetail`: a `summary` (`VrfView`) + the always-
    /// present `prefixes`/`addresses` lists + `prefix_total`/`address_total`.
    /// The REST backend (default profile — no GraphQL configured) fetches the
    /// two child collections; both empty here pins `summary` + the totals.
    #[tokio::test]
    async fn get_vrf_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/ipam/vrfs/",
            json!({
                "id": 5, "url": "u", "name": "customer-prod", "rd": "65000:100",
                "custom_fields": {}
            }),
        )
        .await;
        // REST child fetches (default backend — no GraphQL configured).
        for ep in ["/api/ipam/prefixes/", "/api/ipam/ip-addresses/"] {
            mount_empty(&mock, ep).await;
        }

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Vrf, "65000:100")))
            .await
            .expect("vrf lookup");
        assert_keys(
            &value,
            &[
                "summary",
                "prefixes",
                "addresses",
                "prefix_total",
                "address_total",
            ],
        );
        assert_keys(&value["summary"], &["id", "name", "rd"]);
        assert_eq!(value["summary"]["rd"], "65000:100");
    }

    /// `nbox_get route_target` → `RouteTargetDetail`: a `summary`
    /// (`RouteTargetView`) + the skip-if-empty `importing_vrfs`/`exporting_vrfs`
    /// lists. The REST backend fetches the two VRF collections (import/export
    /// targets); both empty here pins `summary` only.
    #[tokio::test]
    async fn get_route_target_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/ipam/route-targets/",
            json!({
                "id": 5, "url": "u", "name": "65000:100",
                "custom_fields": {}
            }),
        )
        .await;
        // REST VRF fan-out (import/export targets) — empty here.
        mount_empty(&mock, "/api/ipam/vrfs/").await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::RouteTarget, "65000:100")))
            .await
            .expect("route-target lookup");
        assert_keys(&value, &["summary", "importing_vrfs", "exporting_vrfs"]);
        assert_keys(&value["summary"], &["id", "name"]);
        assert_eq!(value["summary"]["name"], "65000:100");
    }

    /// `nbox_get mac` → `MacView`: id + mac_address (mandatory) + the optional
    /// assigned_object_type/assigned/device scalars. The resolver reverse-
    /// resolves the MAC to its carrying interface(s); the first match's view is
    /// pinned here.
    #[tokio::test]
    async fn get_mac_view_shape_is_pinned() {
        let mock = MockServer::start().await;
        mount_one(
            &mock,
            "/api/dcim/mac-addresses/",
            json!({
                "id": 1, "url": "u", "mac_address": "aa:bb:cc:dd:ee:ff",
                "assigned_object_type": "dcim.interface",
                "assigned_object": {"id": 42, "name": "xe-0/0/0",
                                     "device": {"id": 7, "name": "edge01"}},
                "custom_fields": {}
            }),
        )
        .await;

        let Json(value) = server_for(&mock)
            .nbox_get(Parameters(get_args(GetKind::Mac, "aa:bb:cc:dd:ee:ff")))
            .await
            .expect("mac lookup");
        assert_keys(
            &value,
            &[
                "id",
                "mac_address",
                "assigned_object_type",
                "assigned",
                "device",
            ],
        );
        assert_eq!(value["mac_address"], "aa:bb:cc:dd:ee:ff");
    }
}

// --- MCP write tool tests (Pattern 2) ---------------------------------------

/// A server with writes enabled and a vault entry for the test user `alice`
/// (sub `alice` → env var `NBOX_VAULT_TEST_WRITE`).
fn write_server_for(mock: &MockServer) -> NboxMcp {
    let profile = ProfileConfig {
        url: mock.uri(),
        ..Default::default()
    };
    let mut entries = std::collections::BTreeMap::new();
    entries.insert(
        "alice".to_string(),
        crate::mcp::vault::VaultEntry {
            token_env: "NBOX_VAULT_TEST_WRITE".to_string(),
        },
    );
    let vault = crate::mcp::vault::CredentialVault::new(entries, true);
    NboxMcp::new(
        NetBoxClient::new(&profile, None).unwrap(),
        crate::cache::Cache::disabled(),
        Some(vault),
        "test-profile".to_string(),
    )
}

/// A write caller (the per-request authz facts the tool layer extracts from the
/// OIDC identity), for driving `plan_write_impl`/`apply_write_impl` directly.
fn caller(sub: &str, has_write_scope: bool) -> crate::mcp::write::WriteCaller {
    crate::mcp::write::WriteCaller {
        sub: sub.to_string(),
        has_write_scope,
    }
}

/// The caller-identity extraction (the riskiest new wiring): the auth gate puts
/// a validated `Identity` into the HTTP request `Parts` extensions, and the
/// Streamable-HTTP transport nests those `Parts` into the rmcp request
/// `Extensions`. This reproduces that exact nesting and asserts the `sub` +
/// `nbox:write` scope are pulled out — and that an absent/empty identity yields
/// `None` (→ the no-identity reject path), never a fabricated caller.
#[cfg(feature = "http")]
#[test]
fn write_caller_extracts_sub_and_scope_from_nested_request_parts() {
    use crate::mcp::oidc::{Identity, SCOPE_WRITE};

    let identity = |sub: Option<&str>, scopes: Vec<String>| Identity {
        sub: sub.map(str::to_string),
        client_id: None,
        scopes,
        jti: None,
        iss: None,
    };
    let nest = |id: Option<Identity>| {
        let mut req = axum::http::Request::builder().body(()).unwrap();
        if let Some(id) = id {
            req.extensions_mut().insert(id);
        }
        let (parts, ()) = req.into_parts();
        let mut ext = rmcp::model::Extensions::new();
        ext.insert(parts);
        ext
    };

    // sub + write scope → extracted.
    let ext = nest(Some(identity(Some("alice"), vec![SCOPE_WRITE.to_string()])));
    let c = crate::mcp::write_caller_from_extensions(&ext).expect("identity extracted");
    assert_eq!(c.sub, "alice");
    assert!(c.has_write_scope);

    // sub but no write scope → extracted with the flag false (bridged_client rejects).
    let ext = nest(Some(identity(Some("bob"), vec!["nbox:read".to_string()])));
    let c = crate::mcp::write_caller_from_extensions(&ext).expect("identity extracted");
    assert_eq!(c.sub, "bob");
    assert!(!c.has_write_scope);

    // No Identity in the parts → None.
    assert!(crate::mcp::write_caller_from_extensions(&nest(None)).is_none());

    // Identity present but empty/absent sub → None (no usable identity).
    let ext = nest(Some(identity(None, vec![SCOPE_WRITE.to_string()])));
    assert!(crate::mcp::write_caller_from_extensions(&ext).is_none());

    // No Parts at all (e.g. stdio) → None.
    assert!(crate::mcp::write_caller_from_extensions(&rmcp::model::Extensions::new()).is_none());
}

#[tokio::test]
async fn plan_write_rejects_when_vault_absent() {
    // A read-only server (vault = None) must reject write planning with a
    // clear "writes not enabled" error, not fall through to the service token.
    let mock = MockServer::start().await;
    let server = server_for(&mock); // vault = None
    // Even with a fully valid caller (sub + write scope), the disabled gate is
    // checked first: "writes not enabled", never a service-token fallthrough.
    let result = server
        .plan_write_impl(
            crate::mcp::write::PlanWriteArgs {
                operation: crate::mcp::write::WriteOperation::DeviceStatus {
                    device: "edge01".into(),
                    status: "active".into(),
                },
            },
            Some(caller("alice", true)),
        )
        .await;
    assert!(result.is_err(), "should reject writes when vault is absent");
    let err = result.err().unwrap();

    assert!(
        err.message.contains("writes are not enabled"),
        "error should mention writes not enabled: {}",
        err.message
    );
}

#[tokio::test]
async fn plan_write_rejects_when_no_caller_identity() {
    // Writes enabled + a vault, but the request carried no identity (stdio /
    // loopback). Must reject with a distinct "requires an authenticated OIDC
    // caller" error — never the placeholder-sub path, never the service token.
    let mock = MockServer::start().await;
    let server = write_server_for(&mock);
    let result = server
        .plan_write_impl(
            crate::mcp::write::PlanWriteArgs {
                operation: crate::mcp::write::WriteOperation::DeviceStatus {
                    device: "edge01".into(),
                    status: "active".into(),
                },
            },
            None,
        )
        .await;
    let err = result.err().expect("no identity must be rejected");
    assert!(
        err.message.contains("authenticated OIDC caller"),
        "error should name the missing identity: {}",
        err.message
    );
}

#[tokio::test]
async fn plan_write_rejects_when_missing_write_scope() {
    // A caller with a valid sub + vault entry but no `nbox:write` scope is
    // rejected before any NetBox call (ADR-0001 §7).
    let mock = MockServer::start().await;
    let server = write_server_for(&mock);
    let result = server
        .plan_write_impl(
            crate::mcp::write::PlanWriteArgs {
                operation: crate::mcp::write::WriteOperation::DeviceStatus {
                    device: "edge01".into(),
                    status: "active".into(),
                },
            },
            Some(caller("alice", false)),
        )
        .await;
    let err = result.err().expect("missing write scope must be rejected");
    assert!(
        err.message.contains("nbox:write"),
        "error should name the missing scope: {}",
        err.message
    );
}

#[tokio::test]
async fn plan_write_rejects_when_sub_not_in_vault() {
    // Writes enabled, caller has write scope, but their sub has no vault entry:
    // fail closed (NoEntry), never the service token.
    let mock = MockServer::start().await;
    let server = write_server_for(&mock); // vault maps "alice", not "bob"
    let result = server
        .plan_write_impl(
            crate::mcp::write::PlanWriteArgs {
                operation: crate::mcp::write::WriteOperation::DeviceStatus {
                    device: "edge01".into(),
                    status: "active".into(),
                },
            },
            Some(caller("bob", true)),
        )
        .await;
    let err = result.err().expect("unknown sub must be rejected");
    assert!(
        err.message
            .contains("no vault entry for caller sub \"bob\""),
        "error should name the missing vault entry: {}",
        err.message
    );
}

#[tokio::test]
async fn plan_write_device_status_produces_plan() {
    // Set the per-user token env var so the vault resolves successfully.
    unsafe {
        std::env::set_var("NBOX_VAULT_TEST_WRITE", "nbt_per_user_token");
    }
    let mock = MockServer::start().await;

    // Mount the device detail (read-before-write) — paginated search response.
    mount_one(
        &mock,
        "/api/dcim/devices/",
        json!({
            "id": 1, "name": "edge01", "slug": "edge01",
            "status": {"value": "planned", "label": "Planned"},
            "display": "edge01", "url": "u",
            "custom_fields": {}
        }),
    )
    .await;
    // Mount the choices endpoint for status validation (DRF OPTIONS shape).
    mock.register(
        wiremock::Mock::given(wiremock::matchers::method("OPTIONS"))
            .and(wiremock::matchers::path("/api/dcim/devices/"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "name": "Device",
                "actions": {
                    "POST": {
                        "status": {
                            "type": "choice",
                            "label": "Status",
                            "choices": [
                                {"value": "active", "display": "Active"},
                                {"value": "planned", "display": "Planned"},
                            ]
                        }
                    }
                }
            }))),
    )
    .await;

    // Mount the device detail endpoint (read-before-write with ETag).
    mock.register(
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/dcim/devices/1/"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "id": 1, "name": "edge01", "slug": "edge01",
                        "status": {"value": "planned", "label": "Planned"},
                        "display": "edge01", "url": "u",
                        "custom_fields": {}
                    }))
                    .insert_header("ETag", "\"v1\""),
            ),
    )
    .await;

    let server = write_server_for(&mock);
    let result = server
        .plan_write_impl(
            crate::mcp::write::PlanWriteArgs {
                operation: crate::mcp::write::WriteOperation::DeviceStatus {
                    device: "edge01".into(),
                    status: "active".into(),
                },
            },
            Some(caller("alice", true)),
        )
        .await;

    unsafe {
        std::env::remove_var("NBOX_VAULT_TEST_WRITE");
    }

    let plan = result.expect("plan should succeed with vault configured");
    let plan = plan.0; // unwrap Json
    assert_eq!(plan.operation, crate::netbox::mutation::Operation::Update);
    assert_eq!(plan.target.kind, "device");
    assert_eq!(plan.target.r#ref, "edge01");
    // The plan should show the status changing from planned to active.
    let status_change = plan
        .fields
        .iter()
        .find(|f| f.field == "status")
        .expect("plan should include a status field change");
    assert_eq!(status_change.before, "planned");
    assert_eq!(status_change.after, "active");
    // The plan must have a confirm_token (non-empty).
    assert!(!plan.confirm_token.is_empty());
}

#[tokio::test]
async fn apply_write_rejects_tampered_plan() {
    // A plan whose confirm_token doesn't match its contents must be rejected.
    let mock = MockServer::start().await;
    let server = server_for(&mock); // vault doesn't matter — verify is first

    // Build a fake plan with a bad confirm_token.
    let plan = crate::netbox::mutation::MutationPlan {
        schema_version: 1,
        operation: crate::netbox::mutation::Operation::Update,
        target: crate::netbox::mutation::PlanTarget {
            kind: "device".into(),
            r#ref: "edge01".into(),
            id: 1,
            display: "edge01".into(),
            endpoint: "/api/dcim/devices/1/".into(),
            profile: "test-profile".into(),
        },
        precondition: crate::netbox::mutation::Precondition::None,
        fields: vec![],
        patch: json!({}),
        no_op: true,
        warnings: vec![],
        errors: vec![],
        changelog_message: None,
        count: 1,
        confirm_token: "tampered_token".into(),
        expires_at: "2999-01-01T00:00:00Z".into(),
    };

    let result = server
        .apply_write_impl(crate::mcp::write::ApplyWriteArgs { plan }, None)
        .await;
    assert!(result.is_err(), "should reject tampered plan");
    let err = result.err().unwrap();

    assert!(
        err.message.contains("confirmation token"),
        "error should mention confirm token: {}",
        err.message
    );
}
