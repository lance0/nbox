//! Integration tests for the NetBox REST client, backed by a wiremock server.

use nbox::config::ProfileConfig;
use nbox::netbox::auth::AuthScheme;
use nbox::netbox::client::NetBoxClient;
use nbox::netbox::endpoints::Endpoint;
use nbox::netbox::pagination::Page;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client_for(server: &MockServer) -> NetBoxClient {
    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    NetBoxClient::new(&profile, None).unwrap()
}

#[tokio::test]
async fn verify_compatible_accepts_supported_version() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/status/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "netbox-version": "4.5.5"
        })))
        .mount(&server)
        .await;

    let status = client_for(&server).verify_compatible().await.unwrap();
    assert_eq!(status.netbox_version, "4.5.5");
}

#[tokio::test]
async fn verify_compatible_rejects_old_version() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/status/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "netbox-version": "4.1.0"
        })))
        .mount(&server)
        .await;

    let err = client_for(&server)
        .verify_compatible()
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("4.1.0"),
        "error should name the version: {err}"
    );
    assert!(err.contains("4.2"), "error should name the floor: {err}");
}

fn empty_page() -> Value {
    json!({ "count": 0, "next": null, "previous": null, "results": [] })
}

#[tokio::test]
async fn sends_legacy_token_authorization_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(header("authorization", "Token secret123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .expect(1)
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        url: server.uri(),
        auth_scheme: Some(AuthScheme::Token),
        ..Default::default()
    };
    let client = NetBoxClient::new(&profile, Some("secret123".into())).unwrap();

    let page: Page<Value> = client.list(Endpoint::Prefixes, vec![]).await.unwrap();
    assert_eq!(page.count, 0);
}

#[tokio::test]
async fn auto_scheme_uses_bearer_for_v2_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(header("authorization", "Bearer nbt_abc.def"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .expect(1)
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    let client = NetBoxClient::new(&profile, Some("nbt_abc.def".into())).unwrap();

    let _page: Page<Value> = client.list(Endpoint::Sites, vec![]).await.unwrap();
}

#[tokio::test]
async fn excludes_config_context_for_devices() {
    let server = MockServer::start().await;
    // This mock only matches when ?exclude=config_context is present.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("exclude", "config_context"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .expect(1)
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    let client = NetBoxClient::new(&profile, None).unwrap();

    let _page: Page<Value> = client.list(Endpoint::Devices, vec![]).await.unwrap();
}

#[tokio::test]
async fn non_success_status_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    let client = NetBoxClient::new(&profile, None).unwrap();

    let result: Result<Page<Value>, _> = client.list(Endpoint::Sites, vec![]).await;
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("403"),
        "error should mention the status: {err}"
    );
}
