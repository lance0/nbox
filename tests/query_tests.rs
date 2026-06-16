//! Integration tests for endpoint query helpers.

use nbox::config::ProfileConfig;
use nbox::netbox::client::NetBoxClient;
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

#[tokio::test]
async fn device_by_name_uses_name_ie_filter() {
    let server = MockServer::start().await;
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
        .mount(&server)
        .await;

    let device = client(&server)
        .device_by_ref("edge01")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(device.name, "edge01");
}

#[tokio::test]
async fn device_by_id_hits_detail_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/5/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 5, "url": "http://nb/api/dcim/devices/5/", "name": "edge05"
        })))
        .mount(&server)
        .await;

    let device = client(&server).device_by_ref("5").await.unwrap().unwrap();
    assert_eq!(device.id, 5);
    assert_eq!(device.name, "edge05");
}

#[tokio::test]
async fn device_by_missing_id_returns_none_not_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/99999/"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Not found."))
        .mount(&server)
        .await;

    // A 404 on the ID detail endpoint must map to Ok(None), per the contract.
    let device = client(&server).device_by_ref("99999").await.unwrap();
    assert!(device.is_none());
}

#[tokio::test]
async fn device_not_found_returns_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&server)
        .await;

    let device = client(&server).device_by_ref("nope").await.unwrap();
    assert!(device.is_none());
}

#[tokio::test]
async fn device_contains_with_multiple_matches_is_ambiguous() {
    let server = MockServer::start().await;
    // Exact (name__ie) finds nothing; the contains fallback returns several.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("name__ie", "edge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("name__ic", "edge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "u", "name": "edge01"},
                {"id": 2, "url": "u", "name": "edge02"}
            ]
        })))
        .mount(&server)
        .await;

    let err = client(&server).device_by_ref("edge").await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("ambiguous"), "got: {msg}");
    assert!(
        msg.contains("edge01") && msg.contains("edge02"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn ip_candidates_use_address_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("address", "10.44.208.55"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 7, "url": "http://nb/ip/7/", "address": "10.44.208.55/24"}]
        })))
        .mount(&server)
        .await;

    let ips = client(&server).ip_candidates("10.44.208.55").await.unwrap();
    assert_eq!(ips.len(), 1);
    assert_eq!(ips[0].address, "10.44.208.55/24");
}

#[tokio::test]
async fn prefixes_containing_use_contains_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("contains", "10.44.208.55"))
        .and(query_param("vrf_id", "null"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "http://nb/p/1/", "prefix": "10.44.0.0/16"},
                {"id": 2, "url": "http://nb/p/2/", "prefix": "10.44.208.0/24"}
            ]
        })))
        .mount(&server)
        .await;

    // No VRF → scoped to the global table (vrf_id=null).
    let prefixes = client(&server)
        .prefixes_containing("10.44.208.55", None)
        .await
        .unwrap();
    assert_eq!(prefixes.len(), 2);
}

#[tokio::test]
async fn prefixes_containing_scopes_to_vrf() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("contains", "10.0.0.1"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 9, "url": "http://nb/p/9/", "prefix": "10.0.0.0/24"}]
        })))
        .mount(&server)
        .await;

    let prefixes = client(&server)
        .prefixes_containing("10.0.0.1", Some(7))
        .await
        .unwrap();
    assert_eq!(prefixes.len(), 1);
}

#[tokio::test]
async fn prefix_by_cidr_uses_prefix_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("prefix", "10.44.208.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 5, "url": "http://nb/p/5/", "prefix": "10.44.208.0/24"}]
        })))
        .mount(&server)
        .await;

    let prefix = client(&server)
        .prefix_by_cidr("10.44.208.0/24")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prefix.prefix, "10.44.208.0/24");
}

#[tokio::test]
async fn prefix_children_and_ips_use_within_and_parent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("within", "10.44.208.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 6, "url": "u", "prefix": "10.44.208.0/26"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("parent", "10.44.208.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 7, "url": "u", "address": "10.44.208.1/24"}]
        })))
        .mount(&server)
        .await;

    let cli = client(&server);
    let children = cli.prefix_children("10.44.208.0/24", 50).await.unwrap();
    let ips = cli.prefix_ips("10.44.208.0/24", 50).await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(ips.len(), 1);
}

#[tokio::test]
async fn available_ips_and_prefixes_parse_bare_arrays() {
    let server = MockServer::start().await;
    // These endpoints return a bare JSON array, not a paginated page.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/5/available-ips/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"family": 4, "address": "10.44.208.1/24"},
            {"family": 4, "address": "10.44.208.2/24"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/5/available-prefixes/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"family": 4, "prefix": "10.44.208.0/26"}
        ])))
        .mount(&server)
        .await;

    let cli = client(&server);
    let ips = cli.prefix_available_ips(5, 10).await.unwrap();
    assert_eq!(ips.len(), 2);
    assert_eq!(ips[0].address, "10.44.208.1/24");

    let prefixes = cli.prefix_available_prefixes(5).await.unwrap();
    assert_eq!(prefixes.len(), 1);
    assert_eq!(prefixes[0].prefix, "10.44.208.0/26");
}

#[tokio::test]
async fn vlan_by_numeric_ref_uses_vid_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("vid", "208"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 3, "url": "u", "vid": 208, "name": "users"}]
        })))
        .mount(&server)
        .await;

    let vlan = client(&server).vlan_by_ref("208").await.unwrap().unwrap();
    assert_eq!(vlan.vid, 208);
}

#[tokio::test]
async fn site_by_ref_tries_slug_first() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 1, "url": "u", "name": "iad1", "slug": "iad1"}]
        })))
        .mount(&server)
        .await;

    let site = client(&server).site_by_ref("iad1").await.unwrap().unwrap();
    assert_eq!(site.slug, "iad1");
}

#[tokio::test]
async fn circuit_by_cid_uses_cid_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/circuits/circuits/"))
        .and(query_param("cid", "ACME-1234"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "http://nb/api/circuits/circuits/3/", "cid": "ACME-1234",
                "provider": {"id": 1, "display": "ACME"}
            }]
        })))
        .mount(&server)
        .await;

    let circuit = client(&server)
        .circuit_by_ref("ACME-1234")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(circuit.cid, "ACME-1234");
    assert_eq!(circuit.provider.unwrap().label(), "ACME");
}

#[tokio::test]
async fn circuit_by_id_hits_detail_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/circuits/circuits/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "http://nb/api/circuits/circuits/7/", "cid": "ACME-7"
        })))
        .mount(&server)
        .await;

    let circuit = client(&server).circuit_by_ref("7").await.unwrap().unwrap();
    assert_eq!(circuit.id, 7);
    assert_eq!(circuit.cid, "ACME-7");
}

#[tokio::test]
async fn journal_entries_filter_by_assigned_object() {
    let server = MockServer::start().await;
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
        .mount(&server)
        .await;

    let entries = client(&server)
        .journal_entries("dcim.device", 1, 20)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].comments, "rebooted");
}

#[tokio::test]
async fn ip_range_by_start_uses_start_address_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-ranges/"))
        .and(query_param("start_address", "10.0.0.10/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/ipam/ip-ranges/1/",
                "start_address": "10.0.0.10/24", "end_address": "10.0.0.20/24", "size": 11
            }]
        })))
        .mount(&server)
        .await;

    let range = client(&server)
        .ip_range_by_ref("10.0.0.10/24")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(range.start_address, "10.0.0.10/24");
    assert_eq!(range.size, Some(11));
}

#[tokio::test]
async fn aggregate_by_cidr_uses_prefix_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/aggregates/"))
        .and(query_param("prefix", "10.0.0.0/8"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/ipam/aggregates/1/", "prefix": "10.0.0.0/8",
                "rir": {"id": 1, "display": "RFC 1918"}
            }]
        })))
        .mount(&server)
        .await;

    let agg = client(&server)
        .aggregate_by_ref("10.0.0.0/8")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(agg.prefix, "10.0.0.0/8");
}

#[tokio::test]
async fn asn_by_ref_uses_asn_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/asns/"))
        .and(query_param("asn", "64512"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 9, "url": "http://nb/api/ipam/asns/9/", "asn": 64512}]
        })))
        .mount(&server)
        .await;

    let asn = client(&server).asn_by_ref(64512).await.unwrap().unwrap();
    assert_eq!(asn.asn, 64512);
}

#[tokio::test]
async fn rack_by_id_hits_detail_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/racks/12/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 12, "url": "u", "name": "r12"
        })))
        .mount(&server)
        .await;

    let rack = client(&server).rack_by_ref("12").await.unwrap().unwrap();
    assert_eq!(rack.name, "r12");
}
