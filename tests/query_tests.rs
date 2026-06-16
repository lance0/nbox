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
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "http://nb/p/1/", "prefix": "10.44.0.0/16"},
                {"id": 2, "url": "http://nb/p/2/", "prefix": "10.44.208.0/24"}
            ]
        })))
        .mount(&server)
        .await;

    let prefixes = client(&server)
        .prefixes_containing("10.44.208.55")
        .await
        .unwrap();
    assert_eq!(prefixes.len(), 2);
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
