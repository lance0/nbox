//! Integration tests for the multi-endpoint search fan-out.

mod support;
use nbox::config::ProfileConfig;
use nbox::netbox::client::NetBoxClient;
use nbox::netbox::search::{ObjectKind, SearchFilters, SearchRequest};
use serde_json::json;
use support::netbox::{
    empty_page, mount_empty_list, nb_circuit, nb_device, nb_ip, nb_prefix, nb_rack, nb_site,
    nb_tenant, nb_vlan, nb_vrf, page,
};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> NetBoxClient {
    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    NetBoxClient::new(&profile, None).unwrap()
}

#[tokio::test]
async fn search_merges_ranks_and_dedups_across_endpoints() {
    let server = MockServer::start().await;

    // Devices: one exact-ish hit.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![nb_device(1, "edge01").site(9, "iad1").build()])),
        )
        .mount(&server)
        .await;
    // VLAN whose name contains the query (lower score than the exact device).
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![nb_vlan(5, 10, "edge01-transit").build()])),
        )
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/dcim/sites/").await;
    mount_empty_list(&server, "/api/ipam/ip-addresses/").await;
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/circuits/circuits/").await;
    mount_empty_list(&server, "/api/circuits/virtual-circuits/").await;
    mount_empty_list(&server, "/api/ipam/aggregates/").await;
    mount_empty_list(&server, "/api/ipam/asns/").await;
    mount_empty_list(&server, "/api/ipam/ip-ranges/").await;
    mount_empty_list(&server, "/api/tenancy/tenants/").await;
    mount_empty_list(&server, "/api/tenancy/contacts/").await;
    mount_empty_list(&server, "/api/circuits/providers/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

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
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_site(1, "site-a", "site-a").build(),
            nb_site(2, "site-b", "site-b").build(),
            nb_site(3, "site-c", "site-c").build(),
        ])))
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/dcim/devices/").await;
    mount_empty_list(&server, "/api/ipam/ip-addresses/").await;
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/ipam/vlans/").await;
    mount_empty_list(&server, "/api/circuits/circuits/").await;
    mount_empty_list(&server, "/api/circuits/virtual-circuits/").await;
    mount_empty_list(&server, "/api/ipam/aggregates/").await;
    mount_empty_list(&server, "/api/ipam/asns/").await;
    mount_empty_list(&server, "/api/ipam/ip-ranges/").await;
    mount_empty_list(&server, "/api/tenancy/tenants/").await;
    mount_empty_list(&server, "/api/tenancy/contacts/").await;
    mount_empty_list(&server, "/api/circuits/providers/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

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
        .respond_with(
            ResponseTemplate::new(200).set_body_json(page(vec![nb_device(1, "edge01").build()])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/ipam/ip-addresses/").await;
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/ipam/vlans/").await;
    mount_empty_list(&server, "/api/circuits/circuits/").await;
    mount_empty_list(&server, "/api/circuits/virtual-circuits/").await;
    mount_empty_list(&server, "/api/ipam/aggregates/").await;
    mount_empty_list(&server, "/api/ipam/asns/").await;
    mount_empty_list(&server, "/api/ipam/ip-ranges/").await;
    mount_empty_list(&server, "/api/tenancy/tenants/").await;
    mount_empty_list(&server, "/api/tenancy/contacts/").await;
    mount_empty_list(&server, "/api/circuits/providers/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

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
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_circuit(1, "edge-wan-1").provider(7, "ACME").build(),
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/aggregates/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 2, "url": "http://nb/api/ipam/aggregates/2/", "prefix": "10.0.0.0/8",
                   "rir": {"id": 3, "display": "RFC 1918"}}),
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/asns/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 3, "url": "http://nb/api/ipam/asns/3/", "asn": 64512,
                   "rir": {"id": 3, "display": "RFC 6996"}}),
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-ranges/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 4, "url": "http://nb/api/ipam/ip-ranges/4/",
                   "start_address": "10.0.0.10/24", "end_address": "10.0.0.20/24"}),
        ])))
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/dcim/devices/").await;
    mount_empty_list(&server, "/api/dcim/sites/").await;
    mount_empty_list(&server, "/api/ipam/ip-addresses/").await;
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/ipam/vlans/").await;
    mount_empty_list(&server, "/api/tenancy/tenants/").await;
    mount_empty_list(&server, "/api/tenancy/contacts/").await;
    mount_empty_list(&server, "/api/circuits/providers/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

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
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_tenant(1, "Acme Corp", "acme").group(5, "Customers").build(),
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tenancy/contacts/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 2, "url": "http://nb/api/tenancy/contacts/2/",
                   "name": "Acme NOC", "email": "noc@acme.example"}),
        ])))
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/dcim/devices/").await;
    mount_empty_list(&server, "/api/dcim/sites/").await;
    mount_empty_list(&server, "/api/ipam/ip-addresses/").await;
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/ipam/vlans/").await;
    mount_empty_list(&server, "/api/circuits/circuits/").await;
    mount_empty_list(&server, "/api/circuits/virtual-circuits/").await;
    mount_empty_list(&server, "/api/ipam/aggregates/").await;
    mount_empty_list(&server, "/api/ipam/asns/").await;
    mount_empty_list(&server, "/api/ipam/ip-ranges/").await;
    mount_empty_list(&server, "/api/circuits/providers/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

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
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 1, "url": "http://nb/api/circuits/providers/1/",
                   "name": "ACME Telecom", "slug": "acme-telecom",
                   "asns": [{"id": 5, "url": "u", "asn": 64512}]}),
        ])))
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/dcim/devices/").await;
    mount_empty_list(&server, "/api/dcim/sites/").await;
    mount_empty_list(&server, "/api/ipam/ip-addresses/").await;
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/ipam/vlans/").await;
    mount_empty_list(&server, "/api/circuits/circuits/").await;
    mount_empty_list(&server, "/api/circuits/virtual-circuits/").await;
    mount_empty_list(&server, "/api/ipam/aggregates/").await;
    mount_empty_list(&server, "/api/ipam/asns/").await;
    mount_empty_list(&server, "/api/ipam/ip-ranges/").await;
    mount_empty_list(&server, "/api/tenancy/tenants/").await;
    mount_empty_list(&server, "/api/tenancy/contacts/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

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
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 1, "url": "http://nb/api/virtualization/virtual-machines/1/",
                   "name": "prod-web-01", "cluster": {"id": 5, "display": "prod"}}),
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/clusters/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 2, "url": "http://nb/api/virtualization/clusters/2/",
                   "name": "prod", "type": {"id": 1, "display": "VMware"}}),
        ])))
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/dcim/devices/").await;
    mount_empty_list(&server, "/api/dcim/sites/").await;
    mount_empty_list(&server, "/api/ipam/ip-addresses/").await;
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/ipam/vlans/").await;
    mount_empty_list(&server, "/api/circuits/circuits/").await;
    mount_empty_list(&server, "/api/circuits/virtual-circuits/").await;
    mount_empty_list(&server, "/api/ipam/aggregates/").await;
    mount_empty_list(&server, "/api/ipam/asns/").await;
    mount_empty_list(&server, "/api/ipam/ip-ranges/").await;
    mount_empty_list(&server, "/api/tenancy/tenants/").await;
    mount_empty_list(&server, "/api/tenancy/contacts/").await;
    mount_empty_list(&server, "/api/circuits/providers/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;

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
async fn search_surfaces_racks() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/racks/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![nb_rack(3, "R1-42").site(1, "den1").build()])),
        )
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/dcim/devices/").await;
    mount_empty_list(&server, "/api/dcim/sites/").await;
    mount_empty_list(&server, "/api/ipam/ip-addresses/").await;
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/ipam/vlans/").await;
    mount_empty_list(&server, "/api/circuits/circuits/").await;
    mount_empty_list(&server, "/api/circuits/virtual-circuits/").await;
    mount_empty_list(&server, "/api/ipam/aggregates/").await;
    mount_empty_list(&server, "/api/ipam/asns/").await;
    mount_empty_list(&server, "/api/ipam/ip-ranges/").await;
    mount_empty_list(&server, "/api/tenancy/tenants/").await;
    mount_empty_list(&server, "/api/tenancy/contacts/").await;
    mount_empty_list(&server, "/api/circuits/providers/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "R1".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap();
    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);

    let rack = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::Rack)
        .expect("rack surfaced in search results");
    assert_eq!(rack.display, "R1-42");
    // Subtitle is the parent site.
    assert_eq!(rack.subtitle.as_deref(), Some("den1"));
    // The `/api/` web-URL rewrite drops the API prefix.
    assert_eq!(rack.url, "http://nb/dcim/racks/3/");
}

#[tokio::test]
async fn search_surfaces_vrfs() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/ipam/vrfs/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_vrf(4, "customer-prod", "65000:100").tenant(1, "Acme Corp").build(),
        ])))
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/dcim/devices/").await;
    mount_empty_list(&server, "/api/dcim/sites/").await;
    mount_empty_list(&server, "/api/ipam/ip-addresses/").await;
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/ipam/vlans/").await;
    mount_empty_list(&server, "/api/circuits/circuits/").await;
    mount_empty_list(&server, "/api/circuits/virtual-circuits/").await;
    mount_empty_list(&server, "/api/ipam/aggregates/").await;
    mount_empty_list(&server, "/api/ipam/asns/").await;
    mount_empty_list(&server, "/api/ipam/ip-ranges/").await;
    mount_empty_list(&server, "/api/tenancy/tenants/").await;
    mount_empty_list(&server, "/api/tenancy/contacts/").await;
    mount_empty_list(&server, "/api/circuits/providers/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "customer".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap();
    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);

    let vrf = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::Vrf)
        .expect("vrf surfaced in search results");
    assert_eq!(vrf.display, "customer-prod");
    // Subtitle prefers the RD.
    assert_eq!(vrf.subtitle.as_deref(), Some("65000:100"));
    // The `/api/` web-URL rewrite drops the API prefix.
    assert_eq!(vrf.url, "http://nb/ipam/vrfs/4/");
}

#[tokio::test]
async fn search_surfaces_route_targets() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/ipam/route-targets/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 7, "url": "http://nb/api/ipam/route-targets/7/", "name": "65000:100",
                   "tenant": {"id": 1, "display": "Acme Corp"}}),
        ])))
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/dcim/devices/").await;
    mount_empty_list(&server, "/api/dcim/sites/").await;
    mount_empty_list(&server, "/api/ipam/ip-addresses/").await;
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/ipam/vlans/").await;
    mount_empty_list(&server, "/api/circuits/circuits/").await;
    mount_empty_list(&server, "/api/circuits/virtual-circuits/").await;
    mount_empty_list(&server, "/api/ipam/aggregates/").await;
    mount_empty_list(&server, "/api/ipam/asns/").await;
    mount_empty_list(&server, "/api/ipam/ip-ranges/").await;
    mount_empty_list(&server, "/api/tenancy/tenants/").await;
    mount_empty_list(&server, "/api/tenancy/contacts/").await;
    mount_empty_list(&server, "/api/circuits/providers/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "65000".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap();
    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);

    let rt = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::RouteTarget)
        .expect("route target surfaced in search results");
    assert_eq!(rt.display, "65000:100");
    // Subtitle is the tenant (route targets have no RD).
    assert_eq!(rt.subtitle.as_deref(), Some("Acme Corp"));
    assert_eq!(rt.url, "http://nb/ipam/route-targets/7/");
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
            "results": [json!({"id": 3, "url": "http://nb/api/ipam/asns/3/", "asn": 64512})]
        })))
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/dcim/devices/").await;
    mount_empty_list(&server, "/api/dcim/sites/").await;
    mount_empty_list(&server, "/api/ipam/ip-addresses/").await;
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/ipam/vlans/").await;
    mount_empty_list(&server, "/api/circuits/circuits/").await;
    mount_empty_list(&server, "/api/circuits/virtual-circuits/").await;
    mount_empty_list(&server, "/api/ipam/aggregates/").await;
    mount_empty_list(&server, "/api/ipam/ip-ranges/").await;
    mount_empty_list(&server, "/api/tenancy/tenants/").await;
    mount_empty_list(&server, "/api/tenancy/contacts/").await;
    mount_empty_list(&server, "/api/circuits/providers/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

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
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![nb_site(9, "iad1", "iad1").build()])),
        )
        .mount(&server)
        .await;

    // The prefix endpoint must carry the translated scope params, and a matching
    // prefix comes back (proving it's queried, not skipped).
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("scope_type", "dcim.site"))
        .and(query_param("scope_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_prefix(11, "10.1.0.0/24").scope("dcim.site", 9, "iad1").build(),
        ])))
        .mount(&server)
        .await;

    // The site-search branch also hits `/api/dcim/sites/` (with `q=`, no `slug`);
    // give it an empty page so the resolution mock above stays unambiguous.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("q", "10.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&server)
        .await;

    // Devices/VLANs/VMs filter by the RESOLVED `site_id`, never the slug-only
    // `?site=` (which would silently miss an id/display-name `--site`). Each comes
    // back with a hit, proving it's queried with `site_id` and surfaced.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_device(1, "10.1-edge").site(9, "iad1").build(),
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_vlan(5, 101, "10.1-vlan").site(9, "iad1").build(),
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 7, "url": "http://nb/api/virtualization/virtual-machines/7/",
                   "name": "10.1-vm", "site": {"id": 9, "display": "iad1"}}),
        ])))
        .mount(&server)
        .await;
    // Clusters honor `--site` via the polymorphic scope; give an empty page.
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

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
    // The device/VLAN/VM hits prove `site_id` filtering reaches them (the bug was
    // them silently missing when `--site` wasn't a slug).
    let device = results
        .iter()
        .find(|r| r.kind == ObjectKind::Device)
        .expect("site-scoped device surfaced");
    assert_eq!(device.display, "10.1-edge");
    let vlan = results
        .iter()
        .find(|r| r.kind == ObjectKind::Vlan)
        .expect("site-scoped VLAN surfaced");
    assert_eq!(vlan.display, "101 10.1-vlan");
    let vm = results
        .iter()
        .find(|r| r.kind == ObjectKind::Vm)
        .expect("site-scoped VM surfaced");
    assert_eq!(vm.display, "10.1-vm");
}

#[tokio::test]
async fn search_with_unknown_site_errors_not_found_not_empty() {
    // An unknown `--site` must fail with a typed not-found (exit 4), not quietly
    // return an empty result set — site resolution happens before the fan-out.
    let server = MockServer::start().await;

    // Every site lookup (`slug`, `name__ie`, `name__ic`) comes back empty, so the
    // site can't be resolved.
    mount_empty_list(&server, "/api/dcim/sites/").await;

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
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![nb_site(9, "iad1", "iad1").build()])),
        )
        .mount(&server)
        .await;
    // Endpoints that honor `--site` (directly or via scope) are reached.
    mount_empty_list(&server, "/api/dcim/devices/").await; // site_id-filtered
    mount_empty_list(&server, "/api/ipam/vlans/").await; // site_id-filtered
    mount_empty_list(&server, "/api/ipam/prefixes/").await; // exact site scope
    mount_empty_list(&server, "/api/virtualization/virtual-machines/").await; // site_id-filtered
    mount_empty_list(&server, "/api/virtualization/clusters/").await; // exact site scope
    mount_empty_list(&server, "/api/dcim/racks/").await; // site_id-filtered
    // Defensive catch-alls for endpoints that are skipped under active scope;
    // if they are accidentally reached, `outcome.errors` below catches it.
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;
    // The site-search branch (`q=` lookup) is reached too; fall through to a
    // catch-all empty page for `/api/dcim/sites/` so it doesn't 404.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
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

/// Shared helper: a non-site scope flag resolves its ref to an id and the prefix
/// request carries NetBox's native tree-aware id filter (`region_id`,
/// `site_group_id`, or `location_id`). `endpoint`/`expected_filter` vary per
/// scope kind; `filters` selects which flag is set.
async fn assert_scope_filters_prefixes(
    endpoint: &str,
    expected_filter: &'static str,
    content_type: &str,
    filters: SearchFilters,
) {
    let server = MockServer::start().await;

    // Scope resolution: `*_by_ref` looks the slug up first; return id 7.
    Mock::given(method("GET"))
        .and(path(endpoint))
        .and(query_param("slug", "scope-ref"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 7, "url": "http://nb/api/.../7/", "name": "Scope Ref", "slug": "scope-ref"}),
        ])))
        .mount(&server)
        .await;
    // Catch-all for the scope endpoint so other lookups don't 404.
    mount_empty_list(&server, endpoint).await;

    // The prefix endpoint must carry the native scoped id filter, and a matching
    // prefix comes back (proving it's queried, not skipped). NetBox backs these
    // filters with TreeNodeMultipleChoiceFilter, so descendants are included
    // server-side.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param(expected_filter, "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_prefix(11, "10.2.0.0/24").scope(content_type, 7, "Scope Ref").build(),
        ])))
        .mount(&server)
        .await;

    // Devices + clusters honor region/site-group/location scopes; give them empty
    // pages so the fan-out doesn't 404. Other endpoints are skipped.
    mount_empty_list(&server, "/api/dcim/devices/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

    let c = client(&server);
    // Box the fan-out future to keep it off the stack (clippy::large_futures).
    let results = Box::pin(c.search(SearchRequest {
        query: "10.2".into(),
        limit: 25,
        filters,
    }))
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
async fn search_with_region_scopes_prefixes_by_tree_filter() {
    assert_scope_filters_prefixes(
        "/api/dcim/regions/",
        "region_id",
        "dcim.region",
        SearchFilters {
            region: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_site_group_scopes_prefixes_by_tree_filter() {
    assert_scope_filters_prefixes(
        "/api/dcim/site-groups/",
        "site_group_id",
        "dcim.sitegroup",
        SearchFilters {
            site_group: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_location_scopes_prefixes_by_tree_filter() {
    assert_scope_filters_prefixes(
        "/api/dcim/locations/",
        "location_id",
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
    mount_empty_list(&server, endpoint).await;

    let c = client(&server);
    // Box the fan-out future to keep it off the stack (clippy::large_futures).
    let err = Box::pin(c.search(SearchRequest {
        query: "10.2".into(),
        limit: 25,
        filters,
    }))
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

/// Shared helper: a non-site scope flag also filters CLUSTERS by NetBox's native
/// tree-aware scoped id filters, same as prefixes.
async fn assert_scope_filters_clusters(
    endpoint: &str,
    expected_filter: &'static str,
    content_type: &str,
    filters: SearchFilters,
) {
    let server = MockServer::start().await;

    // Scope resolution: `*_by_ref` looks the slug up first; return id 7.
    Mock::given(method("GET"))
        .and(path(endpoint))
        .and(query_param("slug", "scope-ref"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 7, "url": "http://nb/api/.../7/", "name": "Scope Ref", "slug": "scope-ref"}),
        ])))
        .mount(&server)
        .await;
    mount_empty_list(&server, endpoint).await;

    // The cluster endpoint must carry the native scoped id filter, and a matching
    // cluster comes back (proving it's queried, not skipped).
    Mock::given(method("GET"))
        .and(path("/api/virtualization/clusters/"))
        .and(query_param(expected_filter, "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 12, "url": "http://nb/api/virtualization/clusters/12/", "name": "prod",
                   "scope_type": content_type, "scope": {"id": 7, "display": "Scope Ref"}}),
        ])))
        .mount(&server)
        .await;

    // Prefixes, devices, and racks also honor the scope; give them empty pages so
    // the fan-out doesn't 404. Everything else is skipped.
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/dcim/devices/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

    let c = client(&server);
    // Box the fan-out future to keep it off the stack (clippy::large_futures).
    let results = Box::pin(c.search(SearchRequest {
        query: "prod".into(),
        limit: 25,
        filters,
    }))
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
async fn search_with_region_scopes_clusters_by_tree_filter() {
    assert_scope_filters_clusters(
        "/api/dcim/regions/",
        "region_id",
        "dcim.region",
        SearchFilters {
            region: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_site_group_scopes_clusters_by_tree_filter() {
    assert_scope_filters_clusters(
        "/api/dcim/site-groups/",
        "site_group_id",
        "dcim.sitegroup",
        SearchFilters {
            site_group: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_location_scopes_clusters_by_tree_filter() {
    assert_scope_filters_clusters(
        "/api/dcim/locations/",
        "location_id",
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
        .respond_with(
            ResponseTemplate::new(200).set_body_json(page(vec![nb_vrf(7, "blue", "blue").build()])),
        )
        .mount(&server)
        .await;

    // IPs carry the vrf filter and a matching IP comes back (proving it's
    // applied, not dropped).
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("vrf_id", "7"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![nb_ip(21, "10.0.0.1/24").vrf(7, "blue").build()])),
        )
        .mount(&server)
        .await;
    // Prefixes carry the vrf filter too.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_prefix(31, "10.0.0.0/24").vrf(7, "blue").build(),
        ])))
        .mount(&server)
        .await;

    // VRF-incapable endpoints are queried WITHOUT a vrf filter (matched on `q`).
    // A device hit here must NOT carry `vrf_id`; mount it on the plain `q` query
    // so a regression that vrf-filtered devices would 404 instead.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("q", "10.0"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(page(vec![nb_device(1, "10.0-edge").build()])),
        )
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/dcim/sites/").await;
    mount_empty_list(&server, "/api/ipam/vlans/").await;
    mount_empty_list(&server, "/api/circuits/circuits/").await;
    mount_empty_list(&server, "/api/circuits/virtual-circuits/").await;
    mount_empty_list(&server, "/api/ipam/aggregates/").await;
    mount_empty_list(&server, "/api/ipam/asns/").await;
    mount_empty_list(&server, "/api/ipam/ip-ranges/").await;
    mount_empty_list(&server, "/api/tenancy/tenants/").await;
    mount_empty_list(&server, "/api/tenancy/contacts/").await;
    mount_empty_list(&server, "/api/circuits/providers/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

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
async fn search_with_owner_filter_sends_owner_param_to_endpoints() {
    // `--owner` (NetBox 4.5+) is an endpoint param sent to every search
    // endpoint as `owner=<name>`. Pin it on devices (a hit comes back only when
    // the param is sent), and mount the rest empty — proving the filter is
    // forwarded, not dropped, and that no endpoint errors.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("owner", "netops"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(page(vec![nb_device(1, "edge01").build()])),
        )
        .mount(&server)
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
        mount_empty_list(&server, p).await;
    }

    let outcome = client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 25,
            filters: SearchFilters {
                owner: Some("netops".to_string()),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let device = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::Device)
        .expect("owner-filtered device surfaced");
    assert_eq!(device.display, "edge01");
}

#[tokio::test]
async fn search_with_vrf_resolved_by_id_filters_prefixes() {
    // A numeric `--vrf` resolves straight off the detail endpoint, then filters.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/ipam/vrfs/7/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(nb_vrf(7, "blue", "65000:7").build()),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_prefix(31, "10.0.0.0/24").vrf(7, "blue").build(),
        ])))
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/ipam/ip-addresses/").await;
    mount_empty_list(&server, "/api/dcim/devices/").await;
    mount_empty_list(&server, "/api/dcim/sites/").await;
    mount_empty_list(&server, "/api/ipam/vlans/").await;
    mount_empty_list(&server, "/api/circuits/circuits/").await;
    mount_empty_list(&server, "/api/circuits/virtual-circuits/").await;
    mount_empty_list(&server, "/api/ipam/aggregates/").await;
    mount_empty_list(&server, "/api/ipam/asns/").await;
    mount_empty_list(&server, "/api/ipam/ip-ranges/").await;
    mount_empty_list(&server, "/api/tenancy/tenants/").await;
    mount_empty_list(&server, "/api/tenancy/contacts/").await;
    mount_empty_list(&server, "/api/circuits/providers/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty_list(&server, "/api/virtualization/virtual-machine-types/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

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
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

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
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![nb_site(9, "iad1", "iad1").build()])),
        )
        .mount(&server)
        .await;
    // VRF resolution (by id) → id 7.
    Mock::given(method("GET"))
        .and(path("/api/ipam/vrfs/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(nb_vrf(7, "blue", "").build()))
        .mount(&server)
        .await;

    // Prefixes must carry scope_type/scope_id AND vrf_id together.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("scope_type", "dcim.site"))
        .and(query_param("scope_id", "9"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_prefix(31, "10.0.0.0/24").scope("dcim.site", 9, "iad1").vrf(7, "blue").build(),
        ])))
        .mount(&server)
        .await;

    // The site-search branch hits `/api/dcim/sites/` with `q=`; empty page so the
    // slug resolution above stays unambiguous.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("q", "10.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&server)
        .await;
    // Devices/VLANs/VMs filter by the resolved `site_id` (not the slug `?site=`);
    // IPs skip on site since they can't carry `--site`. Clusters honor it via the
    // polymorphic scope.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

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
async fn search_with_scope_and_vrf_filters_prefixes() {
    // Non-site scopes use NetBox's native tree-aware id filters on prefixes, and
    // `--vrf` remains orthogonal: NetBox ANDs `region_id` and `vrf_id`.
    let server = MockServer::start().await;

    // Region resolution → id 3.
    Mock::given(method("GET"))
        .and(path("/api/dcim/regions/"))
        .and(query_param("slug", "us-east"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [json!({"id": 3, "url": "http://nb/api/dcim/regions/3/", "name": "US East", "slug": "us-east"})]
        })))
        .mount(&server)
        .await;
    // VRF resolution (by id) → id 7.
    Mock::given(method("GET"))
        .and(path("/api/ipam/vrfs/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(nb_vrf(7, "blue", "").build()))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("region_id", "3"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_prefix(31, "10.0.0.0/24").scope("dcim.site", 9, "iad1").vrf(7, "blue").build(),
        ])))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("region_id", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/clusters/"))
        .and(query_param("region_id", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/racks/"))
        .and(query_param("region_id", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&server)
        .await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "10.0".into(),
            limit: 25,
            filters: SearchFilters {
                region: Some("us-east".into()),
                vrf: Some("7".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let prefix = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::Prefix)
        .expect("vrf+region-scoped prefix surfaced");
    assert_eq!(prefix.display, "10.0.0.0/24");
}

#[tokio::test]
async fn search_region_scope_skips_non_prefix_non_device_non_cluster_endpoints() {
    // An id-based scope (region) has no clean filter on IPs/sites/circuits/…, so
    // those endpoints are skipped (never hit). Only the region lookup and the
    // endpoints that honor the region scope — prefixes, devices, clusters, and
    // racks (racks expose `region_id` like devices) — are mounted; an unexpected
    // request to a skipped endpoint would 404 and surface as a partial failure.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/regions/"))
        .and(query_param("slug", "us-east"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({"id": 3, "url": "http://nb/api/dcim/regions/3/", "name": "US East", "slug": "us-east"}),
        ])))
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/ipam/prefixes/").await; // scope-filtered
    mount_empty_list(&server, "/api/dcim/devices/").await; // region_id-filtered
    mount_empty_list(&server, "/api/dcim/racks/").await;
    // Defensive catch-alls for skipped endpoints; `outcome.errors` catches any
    // accidental request to them.
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await; // scope-filtered

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

/// Regression (H3): `search --site <id>` must filter devices/VLANs/VMs by the
/// RESOLVED `site_id=<id>`, never the slug-only `?site=<id>` (which silently
/// matches nothing — a numeric `--site` is an id, not a slug). The numeric ref is
/// resolved straight off the site detail endpoint, then applied as `site_id`.
#[tokio::test]
async fn search_with_numeric_site_filters_devices_vlans_vms_by_site_id() {
    let server = MockServer::start().await;

    // Numeric `--site` → resolved via the detail endpoint (id 9).
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/9/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(nb_site(9, "iad1", "iad1").build()))
        .mount(&server)
        .await;

    // Devices/VLANs/VMs each carry `site_id=9` and a hit comes back — proving the
    // resolved id reaches them (the bug was a raw `site=9` slug query missing all).
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("site_id", "9"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(page(vec![nb_device(1, "edge01").build()])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("site_id", "9"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(page(vec![nb_vlan(5, 10, "edge").build()])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [json!({"id": 7, "url": "http://nb/api/virtualization/virtual-machines/7/", "name": "edge-vm"})]
        })))
        .mount(&server)
        .await;
    // Prefixes + clusters honor `--site` via the polymorphic scope (empty here).
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;
    // The site-search branch hits `/api/dcim/sites/` with `q=` (no detail id).
    mount_empty_list(&server, "/api/dcim/sites/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("9".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let kinds: Vec<ObjectKind> = outcome.results.iter().map(|r| r.kind).collect();
    assert!(kinds.contains(&ObjectKind::Device), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::Vlan), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::Vm), "got: {kinds:?}");
}

/// Regression (H3): `search --site <display-name>` resolves the name to an id and
/// filters devices/VLANs/VMs by `site_id`, never the slug-only `?site=<name>`.
#[tokio::test]
async fn search_with_site_name_filters_devices_by_site_id() {
    let server = MockServer::start().await;

    // A display-name `--site`: slug + exact miss, `name__ic` resolves to id 9.
    for key in ["slug", "name__ie"] {
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/"))
            .and(query_param(key, "IAD One"))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("name__ic", "IAD One"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![nb_site(9, "IAD One", "iad1").build()])),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("site_id", "9"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(page(vec![nb_device(1, "edge01").build()])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&server)
        .await;
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;
    // The site-search branch (`q=`) — catch-all empty page.
    mount_empty_list(&server, "/api/dcim/sites/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("IAD One".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let device = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::Device)
        .expect("name-resolved site filters devices by site_id");
    assert_eq!(device.display, "edge01");
}

/// Regression (H3): `search --region <ref>` filters DEVICES by the resolved
/// `region_id` (devices expose `region_id`/`site_group_id`/`location_id` cleanly),
/// not a raw `region=` value. Confirms the id-based scopes also use `*_id`.
#[tokio::test]
async fn search_with_region_filters_devices_by_region_id() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/regions/"))
        .and(query_param("slug", "us-east"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [json!({"id": 3, "url": "http://nb/api/dcim/regions/3/", "name": "US East", "slug": "us-east"})]
        })))
        .mount(&server)
        .await;
    // The device endpoint must carry `region_id=3` (not a raw `region=`).
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("region_id", "3"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(page(vec![nb_device(1, "edge01").build()])),
        )
        .mount(&server)
        .await;
    // Prefixes + clusters honor a region scope; empty pages keep the fan-out clean.
    mount_empty_list(&server, "/api/ipam/prefixes/").await;
    mount_empty_list(&server, "/api/virtualization/clusters/").await;
    mount_empty_list(&server, "/api/dcim/racks/").await;
    mount_empty_list(&server, "/api/dcim/rack-groups/").await;
    mount_empty_list(&server, "/api/ipam/vrfs/").await;
    mount_empty_list(&server, "/api/ipam/route-targets/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 25,
            filters: SearchFilters {
                region: Some("us-east".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let device = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::Device)
        .expect("region-scoped device filtered by region_id");
    assert_eq!(device.display, "edge01");
}

/// Regression: the 4.6 kinds (`vm-types`, `rack-groups`) are version-gated search
/// fan-out branches. On a pre-4.6 NetBox those endpoints 404 — the branches must
/// swallow the 404 and return empty, NOT fail the whole search closed. Before the
/// fix, a 404 on either branch made every `nbox search` exit 1 with "search
/// incomplete", breaking search entirely on 4.2–4.5.
#[tokio::test]
async fn search_swallows_404_on_version_gated_endpoints() {
    let server = MockServer::start().await;

    // The device hit is what we assert survives; everything else is empty.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(page(vec![nb_device(1, "edge01").build()])),
        )
        .mount(&server)
        .await;

    // The always-present endpoints: empty pages keep the fan-out clean.
    for ep in [
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
        "/api/virtualization/clusters/",
        "/api/dcim/racks/",
        "/api/ipam/vrfs/",
        "/api/ipam/route-targets/",
    ] {
        mount_empty_list(&server, ep).await;
    }

    // The 4.6-only endpoints return 404 — the NetBox 4.2 reality.
    for ep in [
        "/api/virtualization/virtual-machine-types/",
        "/api/dcim/rack-groups/",
    ] {
        Mock::given(method("GET"))
            .and(path(ep))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;
    }

    let outcome = client(&server)
        .search(SearchRequest {
            query: "edge01".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .expect("search must not fail closed when a version-gated endpoint 404s");

    // No errors surfaced: the 404s are absorbed, not reported as partial failures.
    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    // The device hit still comes through — search is fully usable on 4.2.
    let device = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::Device)
        .expect("device hit survives the 404s on version-gated branches");
    assert_eq!(device.display, "edge01");
}

// --- search per-endpoint row cap (0.12.1) --------------------------------------
// Each search branch fetches min(page_size, max(req.limit, SEARCH_BRANCH_FLOOR))
// rows, not the full page_size — the merge truncates to req.limit anyway, so the
// extra rows are deserialized only to be thrown away. These tests pin the limit=
// query param so the cap can't silently regress to page_size.

/// All 20 search endpoints except the one under test, mounted empty (no limit
/// constraint) so only the constrained mock can prove the limit value.
async fn mount_empty_all_except(server: &MockServer, except: &str) {
    let all = [
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
    ];
    for p in all {
        if p != except {
            mount_empty_list(server, p).await;
        }
    }
}

#[tokio::test]
async fn search_caps_per_endpoint_at_req_limit() {
    // A `--limit 25` search (page_size 100) caps each branch at 25, not 100.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("limit", "25"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_device(1, "edge01").url("u").site(9, "iad1").build(),
        ])))
        .expect(1) // regression catcher: must be called with limit=25
        .mount(&server)
        .await;
    mount_empty_all_except(&server, "/api/dcim/devices/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "edge01".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap()
        .results;
    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn search_floors_per_endpoint_limit_at_25() {
    // A tiny `--limit 5` still sends limit=25 (the floor), not 5 — the merge
    // needs enough candidates to rank across endpoints.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("limit", "25")) // floor, not 5
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_device(1, "edge01").url("u").site(9, "iad1").build(),
        ])))
        .expect(1)
        .mount(&server)
        .await;
    mount_empty_all_except(&server, "/api/dcim/devices/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "edge01".into(),
            limit: 5,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap()
        .results;
    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn search_caps_per_endpoint_at_page_size_for_large_limit() {
    // A `--limit 200` (above page_size 100) caps each branch at 100 (page_size),
    // not 200 — the server would silently clamp it anyway, but we avoid sending
    // a value above MAX_PAGE_SIZE.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("limit", "100")) // page_size, not 200
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            nb_device(1, "edge01").url("u").site(9, "iad1").build(),
        ])))
        .expect(1)
        .mount(&server)
        .await;
    mount_empty_all_except(&server, "/api/dcim/devices/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "edge01".into(),
            limit: 200,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap()
        .results;
    assert_eq!(results.len(), 1);
}
