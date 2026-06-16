//! Integration tests for offset-based pagination in `list_all`.

use nbox::config::ProfileConfig;
use nbox::netbox::client::NetBoxClient;
use nbox::netbox::endpoints::Endpoint;
use serde::Deserialize;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Debug, Deserialize)]
struct Item {
    id: u64,
}

#[tokio::test]
async fn list_all_follows_offset_pagination() {
    let server = MockServer::start().await;

    // Page size 2; three total objects across two pages.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 3,
            "next": "next-page",
            "previous": null,
            "results": [{"id": 1}, {"id": 2}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("offset", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 3,
            "next": null,
            "previous": "prev-page",
            "results": [{"id": 3}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        url: server.uri(),
        page_size: Some(2),
        ..Default::default()
    };
    let client = NetBoxClient::new(&profile, None).unwrap();

    let all: Vec<Item> = client.list_all(Endpoint::Sites, vec![], 100).await.unwrap();
    assert_eq!(all.iter().map(|i| i.id).collect::<Vec<_>>(), vec![1, 2, 3]);
}

#[tokio::test]
async fn list_all_respects_max_cap() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 100,
            "next": "more",
            "previous": null,
            "results": [{"id": 1}, {"id": 2}, {"id": 3}, {"id": 4}, {"id": 5}]
        })))
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        url: server.uri(),
        page_size: Some(5),
        ..Default::default()
    };
    let client = NetBoxClient::new(&profile, None).unwrap();

    // Cap below the first page size — we should stop and truncate.
    let capped: Vec<Item> = client.list_all(Endpoint::Sites, vec![], 3).await.unwrap();
    assert_eq!(capped.len(), 3);
}
