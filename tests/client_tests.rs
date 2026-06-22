//! Integration tests for the NetBox REST client, backed by a wiremock server.

use nbox::config::ProfileConfig;
use nbox::error::NboxError;
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
async fn retries_on_429_then_succeeds() {
    let server = MockServer::start().await;
    // First request: 429 with Retry-After: 0 (no real delay), served at most once.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    // The retry then hits this 200.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .with_priority(2)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let page: Page<Value> = client.list(Endpoint::Sites, vec![]).await.unwrap();
    assert_eq!(page.count, 0);
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

/// Issue a list against a single mocked status and return the resulting error.
async fn error_for_status(status: u16, body: &str) -> anyhow::Error {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(status).set_body_string(body.to_string()))
        .mount(&server)
        .await;
    let result: Result<Page<Value>, _> = client_for(&server).list(Endpoint::Sites, vec![]).await;
    result.unwrap_err()
}

#[tokio::test]
async fn http_401_maps_to_authentication_error() {
    let err = error_for_status(401, "Invalid token").await;
    // The typed error must be discoverable in the chain (exit code 3).
    let typed = err
        .chain()
        .find_map(|e| e.downcast_ref::<NboxError>())
        .expect("an NboxError should be in the chain");
    assert!(
        matches!(typed, NboxError::Authentication(_)),
        "got: {typed:?}"
    );
    assert_eq!(NboxError::exit_code_for(&err), 3);
}

#[tokio::test]
async fn http_403_maps_to_permission_denied_error() {
    let err = error_for_status(403, "Forbidden").await;
    let typed = err
        .chain()
        .find_map(|e| e.downcast_ref::<NboxError>())
        .expect("an NboxError should be in the chain");
    assert!(
        matches!(typed, NboxError::PermissionDenied(_)),
        "got: {typed:?}"
    );
    assert_eq!(NboxError::exit_code_for(&err), 3);
}

#[tokio::test]
async fn http_404_on_get_maps_to_not_found_error() {
    // `get` (not `get_optional`) surfaces a raw 404 as NotFound (exit 4),
    // matching `nbox raw GET /…/999999/`.
    let err = error_for_status(404, "{\"detail\":\"Not found.\"}").await;
    let typed = err
        .chain()
        .find_map(|e| e.downcast_ref::<NboxError>())
        .expect("an NboxError should be in the chain");
    assert!(matches!(typed, NboxError::NotFound(_)), "got: {typed:?}");
    assert_eq!(NboxError::exit_code_for(&err), 4);
}

#[tokio::test]
async fn http_500_maps_to_generic_api_error() {
    let err = error_for_status(500, "boom").await;
    let typed = err
        .chain()
        .find_map(|e| e.downcast_ref::<NboxError>())
        .expect("an NboxError should be in the chain");
    match typed {
        NboxError::Api { status, body } => {
            assert_eq!(*status, 500);
            assert!(body.contains("boom"), "body should be carried: {body}");
        }
        other => panic!("expected Api error, got: {other:?}"),
    }
    assert_eq!(NboxError::exit_code_for(&err), 1);
}

#[tokio::test]
async fn http_418_other_status_maps_to_generic_api_error() {
    // Any unmapped non-success status falls through to the generic Api error.
    let err = error_for_status(418, "teapot").await;
    let typed = err
        .chain()
        .find_map(|e| e.downcast_ref::<NboxError>())
        .expect("an NboxError should be in the chain");
    assert!(
        matches!(typed, NboxError::Api { status: 418, .. }),
        "got: {typed:?}"
    );
    assert_eq!(NboxError::exit_code_for(&err), 1);
}

#[tokio::test]
async fn retries_exhaust_on_persistent_429() {
    let server = MockServer::start().await;
    // Always 429 with Retry-After: 0 (no real delay). After the retry budget is
    // spent the client surfaces the 429 as a generic Api error (exit 1), not a
    // success — it does not loop forever.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .mount(&server)
        .await;

    let result: Result<Page<Value>, _> = client_for(&server).list(Endpoint::Sites, vec![]).await;
    let err = result.unwrap_err();
    let typed = err
        .chain()
        .find_map(|e| e.downcast_ref::<NboxError>())
        .expect("an NboxError should be in the chain");
    assert!(
        matches!(typed, NboxError::Api { status: 429, .. }),
        "got: {typed:?}"
    );
    assert_eq!(NboxError::exit_code_for(&err), 1);
}

#[tokio::test]
async fn retries_recover_after_several_429s() {
    let server = MockServer::start().await;
    // Two 429s (within the retry budget), then a 200 — the client recovers.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .with_priority(2)
        .mount(&server)
        .await;

    let page: Page<Value> = client_for(&server)
        .list(Endpoint::Sites, vec![])
        .await
        .unwrap();
    assert_eq!(page.count, 0);
}
