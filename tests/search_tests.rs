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
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

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
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

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
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

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
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

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
async fn search_surfaces_tenants_and_contacts() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tenancy/tenants/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/tenancy/tenants/1/",
                "name": "Acme Corp", "slug": "acme",
                "group": {"id": 5, "display": "Customers"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tenancy/contacts/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 2, "url": "http://nb/api/tenancy/contacts/2/",
                "name": "Acme NOC", "email": "noc@acme.example"
            }]
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
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "acme".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap();
    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let results = outcome.results;

    let kinds: Vec<ObjectKind> = results.iter().map(|r| r.kind).collect();
    assert!(kinds.contains(&ObjectKind::Tenant), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::Contact), "got: {kinds:?}");

    let tenant = results
        .iter()
        .find(|r| r.kind == ObjectKind::Tenant)
        .unwrap();
    assert_eq!(tenant.display, "Acme Corp");
    assert_eq!(tenant.subtitle.as_deref(), Some("Customers"));
    assert_eq!(tenant.url, "http://nb/tenancy/tenants/1/");

    let contact = results
        .iter()
        .find(|r| r.kind == ObjectKind::Contact)
        .unwrap();
    assert_eq!(contact.display, "Acme NOC");
    assert_eq!(contact.subtitle.as_deref(), Some("noc@acme.example"));
    assert_eq!(contact.url, "http://nb/tenancy/contacts/2/");
}

#[tokio::test]
async fn search_surfaces_providers() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/circuits/providers/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/circuits/providers/1/",
                "name": "ACME Telecom", "slug": "acme-telecom",
                "asns": [{"id": 5, "url": "u", "asn": 64512}]
            }]
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
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "acme".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap();
    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);

    let provider = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::Provider)
        .unwrap();
    assert_eq!(provider.display, "ACME Telecom");
    // Subtitle prefers the first AS number.
    assert_eq!(provider.subtitle.as_deref(), Some("AS64512"));
    assert_eq!(provider.url, "http://nb/circuits/providers/1/");
}

#[tokio::test]
async fn search_surfaces_vms_and_clusters() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/virtualization/virtual-machines/1/",
                "name": "prod-web-01",
                "cluster": {"id": 5, "display": "prod"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/clusters/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 2, "url": "http://nb/api/virtualization/clusters/2/",
                "name": "prod",
                "type": {"id": 1, "display": "VMware"}
            }]
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
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "prod".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap();
    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let results = outcome.results;

    let kinds: Vec<ObjectKind> = results.iter().map(|r| r.kind).collect();
    assert!(kinds.contains(&ObjectKind::Vm), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::Cluster), "got: {kinds:?}");

    let vm = results.iter().find(|r| r.kind == ObjectKind::Vm).unwrap();
    assert_eq!(vm.display, "prod-web-01");
    // VM subtitle prefers the cluster.
    assert_eq!(vm.subtitle.as_deref(), Some("prod"));
    assert_eq!(vm.url, "http://nb/virtualization/virtual-machines/1/");

    let cluster = results
        .iter()
        .find(|r| r.kind == ObjectKind::Cluster)
        .unwrap();
    assert_eq!(cluster.display, "prod");
    // Cluster subtitle prefers the type.
    assert_eq!(cluster.subtitle.as_deref(), Some("VMware"));
    assert_eq!(cluster.url, "http://nb/virtualization/clusters/2/");
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
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

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
    mount_empty(&server, "/api/virtualization/virtual-machines/").await; // accepts `site`
    mount_empty(&server, "/api/virtualization/clusters/").await; // accepts `site`
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

/// Shared helper: a scope flag resolves its ref to an id and the prefix request
/// carries `scope_type=<content_type>` + `scope_id`. `endpoint`/`content_type`
/// vary per scope kind; `filters` selects which flag is set.
async fn assert_scope_filters_prefixes(endpoint: &str, content_type: &str, filters: SearchFilters) {
    let server = MockServer::start().await;

    // Scope resolution: `*_by_ref` looks the slug up first; return id 7.
    Mock::given(method("GET"))
        .and(path(endpoint))
        .and(query_param("slug", "scope-ref"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 7, "url": "http://nb/api/.../7/", "name": "Scope Ref", "slug": "scope-ref"
            }]
        })))
        .mount(&server)
        .await;
    // Catch-all for the scope endpoint so other lookups don't 404.
    mount_empty(&server, endpoint).await;

    // The prefix endpoint must carry the translated scope params, and a matching
    // prefix comes back (proving it's queried, not skipped).
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("scope_type", content_type))
        .and(query_param("scope_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 11, "url": "http://nb/api/ipam/prefixes/11/", "prefix": "10.2.0.0/24",
                "scope_type": content_type, "scope": {"id": 7, "display": "Scope Ref"}
            }]
        })))
        .mount(&server)
        .await;

    // Devices + clusters honor region/site-group/location scopes; give them empty
    // pages so the fan-out doesn't 404. Other endpoints are skipped.
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "10.2".into(),
            limit: 25,
            filters,
        })
        .await
        .unwrap()
        .results;

    let prefix = results
        .iter()
        .find(|r| r.kind == ObjectKind::Prefix)
        .expect("scope-filtered prefix surfaced");
    assert_eq!(prefix.display, "10.2.0.0/24");
}

#[tokio::test]
async fn search_with_region_scopes_prefixes_by_scope_type_and_id() {
    assert_scope_filters_prefixes(
        "/api/dcim/regions/",
        "dcim.region",
        SearchFilters {
            region: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_site_group_scopes_prefixes_by_scope_type_and_id() {
    assert_scope_filters_prefixes(
        "/api/dcim/site-groups/",
        "dcim.sitegroup",
        SearchFilters {
            site_group: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_location_scopes_prefixes_by_scope_type_and_id() {
    assert_scope_filters_prefixes(
        "/api/dcim/locations/",
        "dcim.location",
        SearchFilters {
            location: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

/// Shared helper: an unknown scope ref must fail with a typed not-found (exit 4),
/// not a silent-empty result — resolution happens before the fan-out.
async fn assert_unknown_scope_is_not_found(endpoint: &str, noun: &str, filters: SearchFilters) {
    let server = MockServer::start().await;
    // Every lookup (`slug`, `name__ie`, `name__ic`) comes back empty.
    mount_empty(&server, endpoint).await;

    let err = client(&server)
        .search(SearchRequest {
            query: "10.2".into(),
            limit: 25,
            filters,
        })
        .await
        .expect_err("unknown scope ref should error, not return empty");

    assert_eq!(nbox::error::NboxError::exit_code_for(&err), 4);
    assert!(
        format!("{err:#}").contains(&format!("no {noun} matched \"nope\"")),
        "got: {err:#}"
    );
}

#[tokio::test]
async fn search_with_unknown_region_errors_not_found() {
    assert_unknown_scope_is_not_found(
        "/api/dcim/regions/",
        "region",
        SearchFilters {
            region: Some("nope".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_unknown_site_group_errors_not_found() {
    assert_unknown_scope_is_not_found(
        "/api/dcim/site-groups/",
        "site group",
        SearchFilters {
            site_group: Some("nope".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_unknown_location_errors_not_found() {
    assert_unknown_scope_is_not_found(
        "/api/dcim/locations/",
        "location",
        SearchFilters {
            location: Some("nope".into()),
            ..Default::default()
        },
    )
    .await;
}

/// Shared helper: a scope flag also filters CLUSTERS by `scope_type`+`scope_id`
/// (NetBox 4.2+ scopes a cluster polymorphically, same as a prefix).
async fn assert_scope_filters_clusters(endpoint: &str, content_type: &str, filters: SearchFilters) {
    let server = MockServer::start().await;

    // Scope resolution: `*_by_ref` looks the slug up first; return id 7.
    Mock::given(method("GET"))
        .and(path(endpoint))
        .and(query_param("slug", "scope-ref"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 7, "url": "http://nb/api/.../7/", "name": "Scope Ref", "slug": "scope-ref"
            }]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, endpoint).await;

    // The cluster endpoint must carry the translated scope params, and a matching
    // cluster comes back (proving it's queried, not skipped).
    Mock::given(method("GET"))
        .and(path("/api/virtualization/clusters/"))
        .and(query_param("scope_type", content_type))
        .and(query_param("scope_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 12, "url": "http://nb/api/virtualization/clusters/12/", "name": "prod",
                "scope_type": content_type, "scope": {"id": 7, "display": "Scope Ref"}
            }]
        })))
        .mount(&server)
        .await;

    // Prefixes + devices also honor the scope; give them empty pages so the
    // fan-out doesn't 404. Everything else is skipped.
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/dcim/devices/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "prod".into(),
            limit: 25,
            filters,
        })
        .await
        .unwrap()
        .results;

    let cluster = results
        .iter()
        .find(|r| r.kind == ObjectKind::Cluster)
        .expect("scope-filtered cluster surfaced");
    assert_eq!(cluster.display, "prod");
}

#[tokio::test]
async fn search_with_region_scopes_clusters_by_scope_type_and_id() {
    assert_scope_filters_clusters(
        "/api/dcim/regions/",
        "dcim.region",
        SearchFilters {
            region: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_site_group_scopes_clusters_by_scope_type_and_id() {
    assert_scope_filters_clusters(
        "/api/dcim/site-groups/",
        "dcim.sitegroup",
        SearchFilters {
            site_group: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_location_scopes_clusters_by_scope_type_and_id() {
    assert_scope_filters_clusters(
        "/api/dcim/locations/",
        "dcim.location",
        SearchFilters {
            location: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_two_scope_filters_is_a_usage_error() {
    // NetBox prefix scope is a single type+id, so combining scope flags is a
    // usage error (exit 2) — surfaced before any endpoint is hit. No mocks
    // mounted: a request to any endpoint would 404 and prove the early bail-out
    // didn't run.
    let server = MockServer::start().await;

    let err = client(&server)
        .search(SearchRequest {
            query: "10.2".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("iad1".into()),
                region: Some("us-east".into()),
                ..Default::default()
            },
        })
        .await
        .expect_err("two scope filters should be a usage error");

    assert_eq!(nbox::error::NboxError::exit_code_for(&err), 2);
    assert!(
        format!("{err:#}").contains("mutually exclusive"),
        "got: {err:#}"
    );
}

#[tokio::test]
async fn search_with_vrf_filters_ip_and_prefix_by_vrf_id() {
    // `--vrf` resolves the ref to an id once, then filters the VRF-capable
    // endpoints (IPs, prefixes) by `vrf_id=`. VRF-incapable endpoints (devices,
    // sites, …) are not vrf-filtered — they're queried with `q` only.
    let server = MockServer::start().await;

    // VRF resolution: a non-numeric ref tries `rd` first (VRFs have no slug);
    // return id 7. A catch-all keeps the later name fallbacks from 404ing.
    Mock::given(method("GET"))
        .and(path("/api/ipam/vrfs/"))
        .and(query_param("rd", "blue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 7, "url": "http://nb/api/ipam/vrfs/7/", "name": "blue", "rd": "blue"
            }]
        })))
        .mount(&server)
        .await;

    // IPs carry the vrf filter and a matching IP comes back (proving it's
    // applied, not dropped).
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 21, "url": "http://nb/api/ipam/ip-addresses/21/", "address": "10.0.0.1/24",
                "vrf": {"id": 7, "display": "blue"}
            }]
        })))
        .mount(&server)
        .await;
    // Prefixes carry the vrf filter too.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 31, "url": "http://nb/api/ipam/prefixes/31/", "prefix": "10.0.0.0/24",
                "vrf": {"id": 7, "display": "blue"}
            }]
        })))
        .mount(&server)
        .await;

    // VRF-incapable endpoints are queried WITHOUT a vrf filter (matched on `q`).
    // A device hit here must NOT carry `vrf_id`; mount it on the plain `q` query
    // so a regression that vrf-filtered devices would 404 instead.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("q", "10.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "10.0-edge"}]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/dcim/sites/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "10.0".into(),
            limit: 25,
            filters: SearchFilters {
                vrf: Some("blue".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(
        outcome.errors.is_empty(),
        "no vrf filter should leak onto a vrf-incapable endpoint; errors: {:?}",
        outcome.errors
    );
    let ip = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::IpAddress)
        .expect("vrf-filtered IP surfaced");
    assert_eq!(ip.display, "10.0.0.1/24");
    let prefix = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::Prefix)
        .expect("vrf-filtered prefix surfaced");
    assert_eq!(prefix.display, "10.0.0.0/24");
    // The device (vrf-incapable) still surfaces — it isn't vrf-filtered away.
    assert!(
        outcome.results.iter().any(|r| r.kind == ObjectKind::Device),
        "vrf-incapable device should still surface"
    );
}

#[tokio::test]
async fn search_with_vrf_resolved_by_id_filters_prefixes() {
    // A numeric `--vrf` resolves straight off the detail endpoint, then filters.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/ipam/vrfs/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "http://nb/api/ipam/vrfs/7/", "name": "blue", "rd": "65000:7"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 31, "url": "http://nb/api/ipam/prefixes/31/", "prefix": "10.0.0.0/24",
                "vrf": {"id": 7, "display": "blue"}
            }]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/dcim/sites/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "10.0".into(),
            limit: 25,
            filters: SearchFilters {
                vrf: Some("7".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
        .results;

    let prefix = results
        .iter()
        .find(|r| r.kind == ObjectKind::Prefix)
        .expect("vrf-filtered prefix surfaced");
    assert_eq!(prefix.display, "10.0.0.0/24");
}

#[tokio::test]
async fn search_with_unknown_vrf_errors_not_found_not_empty() {
    // An unknown `--vrf` must fail with a typed not-found (exit 4), not quietly
    // return an empty result set — VRF resolution happens before the fan-out.
    let server = MockServer::start().await;

    // Every VRF lookup (`rd`, `name__ie`, `name__ic`) comes back empty.
    mount_empty(&server, "/api/ipam/vrfs/").await;

    let err = client(&server)
        .search(SearchRequest {
            query: "10.0".into(),
            limit: 25,
            filters: SearchFilters {
                vrf: Some("nope".into()),
                ..Default::default()
            },
        })
        .await
        .expect_err("unknown vrf should error, not return empty");

    assert_eq!(nbox::error::NboxError::exit_code_for(&err), 4);
    assert!(
        format!("{err:#}").contains("no VRF matched \"nope\""),
        "got: {err:#}"
    );
}

#[tokio::test]
async fn search_combines_vrf_and_site_scope_on_prefixes() {
    // `--vrf` is orthogonal to `--site`: prefixes carry BOTH `scope_*` and
    // `vrf_id` (NetBox ANDs them); other endpoints honor only what they can.
    let server = MockServer::start().await;

    // Site resolution → id 9.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 9, "url": "http://nb/api/dcim/sites/9/", "name": "iad1", "slug": "iad1"}]
        })))
        .mount(&server)
        .await;
    // VRF resolution (by id) → id 7.
    Mock::given(method("GET"))
        .and(path("/api/ipam/vrfs/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "http://nb/api/ipam/vrfs/7/", "name": "blue"
        })))
        .mount(&server)
        .await;

    // Prefixes must carry scope_type/scope_id AND vrf_id together.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("scope_type", "dcim.site"))
        .and(query_param("scope_id", "9"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 31, "url": "http://nb/api/ipam/prefixes/31/", "prefix": "10.0.0.0/24",
                "scope_type": "dcim.site", "scope": {"id": 9, "display": "iad1"},
                "vrf": {"id": 7, "display": "blue"}
            }]
        })))
        .mount(&server)
        .await;

    // The site-search branch hits `/api/dcim/sites/` with `q=`; empty page so the
    // slug resolution above stays unambiguous.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("q", "10.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(&server)
        .await;
    // IPs honor vrf (and skip on site since IPs can't carry `--site` here).
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
            query: "10.0".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("iad1".into()),
                vrf: Some("7".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
        .results;

    let prefix = results
        .iter()
        .find(|r| r.kind == ObjectKind::Prefix)
        .expect("vrf+site-scoped prefix surfaced");
    assert_eq!(prefix.display, "10.0.0.0/24");
}

#[tokio::test]
async fn search_region_scope_skips_non_prefix_non_device_non_cluster_endpoints() {
    // An id-based scope (region) has no clean filter on IPs/sites/circuits/…, so
    // those endpoints are skipped (never hit). Only the region lookup, the prefix
    // endpoint, the device endpoint, and the cluster endpoint are mounted (the
    // latter three honor the region scope); an unexpected request to a skipped
    // endpoint would 404 and surface as a partial failure.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/regions/"))
        .and(query_param("slug", "us-east"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "http://nb/api/dcim/regions/3/", "name": "US East", "slug": "us-east"
            }]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/ipam/prefixes/").await; // scope-filtered
    mount_empty(&server, "/api/dcim/devices/").await; // region_id-filtered
    mount_empty(&server, "/api/virtualization/clusters/").await; // scope-filtered

    let outcome = client(&server)
        .search(SearchRequest {
            query: "x".into(),
            limit: 25,
            filters: SearchFilters {
                region: Some("us-east".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(
        outcome.errors.is_empty(),
        "no endpoint should have been hit that can't honor --region; errors: {:?}",
        outcome.errors
    );
}
