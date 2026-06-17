//! Integration tests for the multi-endpoint search fan-out.

use nbox::config::ProfileConfig;
use nbox::netbox::client::NetBoxClient;
use nbox::netbox::search::{ObjectKind, SearchFilters, SearchRequest};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> NetBoxClient {
    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    NetBoxClient::new(&profile, None).unwrap()
}

fn empty() -> serde_json::Value {
    json!({ "count": 0, "next": null, "previous": null, "results": [] })
}

async fn mount_empty(server: &MockServer, p: &str) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(server)
        .await;
}

#[tokio::test]
async fn search_merges_ranks_and_dedups_across_endpoints() {
    let server = MockServer::start().await;

    // Devices: one exact-ish hit.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01",
                "site": {"id": 9, "display": "iad1"}
            }]
        })))
        .mount(&server)
        .await;
    // VLAN whose name contains the query (lower score than the exact device).
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 5, "url": "http://nb/api/ipam/vlans/5/", "vid": 10, "name": "edge01-transit"}]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/dcim/sites/").await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "edge01".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap()
        .results;

    assert_eq!(results.len(), 2);
    // Exact device match ranks first.
    assert_eq!(results[0].kind, ObjectKind::Device);
    assert_eq!(results[0].display, "edge01");
    assert_eq!(results[0].subtitle.as_deref(), Some("iad1"));
    // Web URL is derived from the API URL.
    assert_eq!(results[0].url, "http://nb/dcim/devices/1/");
    // VLAN (partial match) ranks lower.
    assert_eq!(results[1].kind, ObjectKind::Vlan);
}

#[tokio::test]
async fn search_truncates_to_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 3, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "http://nb/api/dcim/sites/1/", "name": "site-a", "slug": "site-a"},
                {"id": 2, "url": "http://nb/api/dcim/sites/2/", "name": "site-b", "slug": "site-b"},
                {"id": 3, "url": "http://nb/api/dcim/sites/3/", "name": "site-c", "slug": "site-c"}
            ]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "site".into(),
            limit: 2,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap()
        .results;
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn search_reports_partial_endpoint_failures() {
    let server = MockServer::start().await;
    // Devices succeed; sites return a 403; the rest are empty.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap();
    // Got the device, but the sites endpoint failure is reported (not hidden).
    assert_eq!(outcome.results.len(), 1);
    assert_eq!(outcome.errors.len(), 1);
    assert!(
        outcome.errors[0].contains("sites"),
        "got: {:?}",
        outcome.errors
    );
}

#[tokio::test]
async fn search_surfaces_circuits_aggregates_asns_and_ip_ranges() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/circuits/circuits/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/circuits/circuits/1/", "cid": "edge-wan-1",
                "provider": {"id": 7, "display": "ACME"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/aggregates/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 2, "url": "http://nb/api/ipam/aggregates/2/", "prefix": "10.0.0.0/8",
                "rir": {"id": 3, "display": "RFC 1918"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/asns/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "http://nb/api/ipam/asns/3/", "asn": 64512,
                "rir": {"id": 3, "display": "RFC 6996"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-ranges/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 4, "url": "http://nb/api/ipam/ip-ranges/4/",
                "start_address": "10.0.0.10/24", "end_address": "10.0.0.20/24"
            }]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/dcim/sites/").await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap()
        .results;

    let kinds: Vec<ObjectKind> = results.iter().map(|r| r.kind).collect();
    assert!(kinds.contains(&ObjectKind::Circuit), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::Aggregate), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::Asn), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::IpRange), "got: {kinds:?}");

    let circuit = results
        .iter()
        .find(|r| r.kind == ObjectKind::Circuit)
        .unwrap();
    assert_eq!(circuit.display, "edge-wan-1");
    assert_eq!(circuit.subtitle.as_deref(), Some("ACME"));
    assert_eq!(circuit.url, "http://nb/circuits/circuits/1/");

    let asn = results.iter().find(|r| r.kind == ObjectKind::Asn).unwrap();
    assert_eq!(asn.display, "AS64512");

    let range = results
        .iter()
        .find(|r| r.kind == ObjectKind::IpRange)
        .unwrap();
    assert_eq!(range.display, "10.0.0.10/24-10.0.0.20/24");
}

#[tokio::test]
async fn search_matches_asn_by_number() {
    let server = MockServer::start().await;
    // A numeric query is routed to the `asn=` filter (not the text `q`), so the
    // ASN endpoint must see `asn=64512` and no `q`.
    Mock::given(method("GET"))
        .and(path("/api/ipam/asns/"))
        .and(wiremock::matchers::query_param("asn", "64512"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 3, "url": "http://nb/api/ipam/asns/3/", "asn": 64512}]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/dcim/sites/").await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "64512".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap()
        .results;

    let asn = results
        .iter()
        .find(|r| r.kind == ObjectKind::Asn)
        .expect("asn surfaced by number");
    assert_eq!(asn.display, "AS64512");
}

#[tokio::test]
async fn search_with_site_scopes_prefixes_by_scope_type_and_id() {
    // NetBox 4.2 dropped the prefix `site` FK for the polymorphic `scope`, so
    // `?site=` is a dead filter on prefixes. With `--site`, search resolves the
    // site to its id once and filters prefixes by `scope_type=dcim.site` +
    // `scope_id=<id>` rather than skipping the prefix endpoint entirely.
    let server = MockServer::start().await;

    // Site resolution: `site_by_ref` looks the slug up first; return id 9.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 9, "url": "http://nb/api/dcim/sites/9/", "name": "iad1", "slug": "iad1"
            }]
        })))
        .mount(&server)
        .await;

    // The prefix endpoint must carry the translated scope params, and a matching
    // prefix comes back (proving it's queried, not skipped).
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("scope_type", "dcim.site"))
        .and(query_param("scope_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 11, "url": "http://nb/api/ipam/prefixes/11/", "prefix": "10.1.0.0/24",
                "scope_type": "dcim.site", "scope": {"id": 9, "display": "iad1"}
            }]
        })))
        .mount(&server)
        .await;

    // The site-search branch also hits `/api/dcim/sites/` (with `q=`, no `slug`);
    // give it an empty page so the resolution mock above stays unambiguous.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("q", "10.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(&server)
        .await;

    // Devices/VLANs accept the site slug directly; the rest skip on `--site`.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("site", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("site", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(&server)
        .await;

    let results = client(&server)
        .search(SearchRequest {
            query: "10.1".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("iad1".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
        .results;

    let prefix = results
        .iter()
        .find(|r| r.kind == ObjectKind::Prefix)
        .expect("site-scoped prefix surfaced");
    assert_eq!(prefix.display, "10.1.0.0/24");
    assert_eq!(prefix.subtitle.as_deref(), Some("iad1"));
}

#[tokio::test]
async fn search_with_unknown_site_errors_not_found_not_empty() {
    // An unknown `--site` must fail with a typed not-found (exit 4), not quietly
    // return an empty result set — site resolution happens before the fan-out.
    let server = MockServer::start().await;

    // Every site lookup (`slug`, `name__ie`, `name__ic`) comes back empty, so the
    // site can't be resolved.
    mount_empty(&server, "/api/dcim/sites/").await;

    let err = client(&server)
        .search(SearchRequest {
            query: "10.1".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("nope".into()),
                ..Default::default()
            },
        })
        .await
        .expect_err("unknown site should error, not return empty");

    assert_eq!(nbox::error::NboxError::exit_code_for(&err), 4);
    assert!(
        format!("{err:#}").contains("no site matched \"nope\""),
        "got: {err:#}"
    );
}

#[tokio::test]
async fn search_skips_non_site_endpoints_unchanged_with_active_site() {
    // The allowlist/skip behavior for endpoints that genuinely can't honor
    // `--site` (IPs, aggregates, ASNs, …) is unchanged: they are skipped, so
    // their endpoints are never hit. Mount ONLY the endpoints that should be
    // reached; an unexpected request to a skipped endpoint would 404 and surface
    // as a partial failure (asserted absent below).
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 9, "url": "http://nb/api/dcim/sites/9/", "name": "iad1", "slug": "iad1"
            }]
        })))
        .mount(&server)
        .await;
    // Endpoints that DO honor `--site` (directly or via scope) are reached.
    mount_empty(&server, "/api/dcim/devices/").await; // accepts `site`
    mount_empty(&server, "/api/ipam/vlans/").await; // accepts `site`
    mount_empty(&server, "/api/ipam/prefixes/").await; // scope-filtered
    // The site-search branch (`q=` lookup) is reached too; fall through to a
    // catch-all empty page for `/api/dcim/sites/` so it doesn't 404.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(&server)
        .await;

    // No mocks for ip-addresses / circuits / aggregates / asns / ip-ranges:
    // those are skipped because they can't honor `--site`. If the skip logic
    // regressed, they'd be requested, 404, and show up in `outcome.errors`.
    let outcome = client(&server)
        .search(SearchRequest {
            query: "x".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("iad1".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(
        outcome.errors.is_empty(),
        "no endpoint should have been hit that can't honor --site; errors: {:?}",
        outcome.errors
    );
}
