//! Integration tests for the multi-endpoint search fan-out.

use nbox::config::ProfileConfig;
use nbox::netbox::client::NetBoxClient;
use nbox::netbox::search::{ObjectKind, SearchRequest};
use serde_json::json;
use wiremock::matchers::{method, path};
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

    let results = client(&server)
        .search(SearchRequest {
            query: "edge01".into(),
            limit: 25,
        })
        .await
        .unwrap();

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

    let results = client(&server)
        .search(SearchRequest {
            query: "site".into(),
            limit: 2,
        })
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
}
