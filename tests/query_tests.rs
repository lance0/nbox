//! Integration tests for endpoint query helpers.

use nbx::config::ProfileConfig;
use nbx::netbox::client::NetBoxClient;
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
