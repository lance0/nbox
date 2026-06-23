//! Integration tests for offset-based pagination in `list_all`.
//!
//! `list_all` sizes each page to `max(page_size, min(max, MAX_PAGE_SIZE))`, so a
//! fetch with `max <= MAX_PAGE_SIZE` (1000) lands in a single request, and only a
//! larger `max` pages — at the server window (1000), advancing `offset` by that
//! window. These cover both the single-request path and the multi-page path
//! (including a mid-stream short page, the B1 offset-advance regression).

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

/// `[start, start+n)` as `{id}` rows.
fn id_rows(start: usize, n: usize) -> Vec<serde_json::Value> {
    (start..start + n).map(|id| json!({ "id": id })).collect()
}

fn client_with_page_size(server: &MockServer, page_size: usize) -> NetBoxClient {
    let profile = ProfileConfig {
        url: server.uri(),
        page_size: Some(page_size),
        ..Default::default()
    };
    NetBoxClient::new(&profile, None).unwrap()
}

#[tokio::test]
async fn list_all_fetches_all_rows_in_one_grown_page() {
    // `max` (100) is within the server window, so the page grows to 100 and the
    // whole result set lands in ONE request — `page_size` (2) is just a floor.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("limit", "100"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 3, "next": null, "previous": null,
            "results": [{"id": 1}, {"id": 2}, {"id": 3}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_with_page_size(&server, 2);
    let all: Vec<Item> = client.list_all(Endpoint::Sites, vec![], 100).await.unwrap();
    assert_eq!(all.iter().map(|i| i.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "a max within the server window is one round trip"
    );
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

    let client = client_with_page_size(&server, 5);

    // Cap below the first page size — we should stop and truncate.
    let capped: Vec<Item> = client.list_all(Endpoint::Sites, vec![], 3).await.unwrap();
    assert_eq!(capped.len(), 3);
}

#[tokio::test]
async fn list_all_pages_over_the_server_window() {
    // `max` (2500) exceeds the server window, so the page caps at 1000 and the
    // fetch pages 1000/1000/500 — offset advancing by the window each time.
    let server = MockServer::start().await;
    let page = |offset: usize, n: usize| {
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/"))
            .and(query_param("limit", "1000"))
            .and(query_param("offset", offset.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 2500, "next": null, "previous": null,
                "results": id_rows(offset, n)
            })))
            .expect(1)
            .mount(&server)
    };
    page(0, 1000).await;
    page(1000, 1000).await;
    page(2000, 500).await;

    let client = client_with_page_size(&server, 100);
    let all: Vec<Item> = client
        .list_all(Endpoint::Sites, vec![], 2500)
        .await
        .unwrap();
    assert_eq!(all.len(), 2500);
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        3,
        "three aligned 1000-row trips"
    );
}

#[tokio::test]
async fn list_all_advances_offset_by_page_size_on_a_short_first_page() {
    // B1 regression: the FIRST page comes back short (999 rows for a 1000 window —
    // a server `limit` cap or a serializer dropping a row post-count). The offset
    // must still advance by the page size (1000), not by the rows returned. On the
    // old `offset += got`, page two would request offset=999 — a misaligned window
    // with no mounted mock → 404 → the call errors and this test fails.
    let server = MockServer::start().await;
    let page = |offset: usize, ids: Vec<serde_json::Value>| {
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/"))
            .and(query_param("limit", "1000"))
            .and(query_param("offset", offset.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 2001, "next": null, "previous": null, "results": ids
            })))
            .expect(1)
            .mount(&server)
    };
    page(0, id_rows(0, 999)).await; // short page (999, not 1000)
    page(1000, id_rows(1000, 1000)).await;
    page(2000, id_rows(2000, 2)).await;

    let client = client_with_page_size(&server, 100);
    let all: Vec<Item> = client
        .list_all(Endpoint::Sites, vec![], 3000)
        .await
        .unwrap();
    assert_eq!(
        all.len(),
        2001,
        "no rows skipped despite the short first page"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        3,
        "offset advanced by the page size (1000), not by the 999 rows returned"
    );
}

#[tokio::test]
async fn list_all_stops_at_count_without_an_extra_page() {
    // 3 objects total, max far larger: one page returns all 3 (count == 3), and
    // the client must stop without requesting a (nonexistent) second page.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 3, "next": null, "previous": null,
            "results": [{"id": 1}, {"id": 2}, {"id": 3}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_with_page_size(&server, 100);
    let all: Vec<Item> = client
        .list_all(Endpoint::Sites, vec![], 1000)
        .await
        .unwrap();
    assert_eq!(all.iter().map(|i| i.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn list_all_truncates_to_the_cap() {
    // count 5, cap 3: the page grows to the cap (3), the server returns 3, and the
    // client stops (out >= max) without a second page, holding exactly 3.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("limit", "3"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 5, "next": "more", "previous": null,
            "results": [{"id": 1}, {"id": 2}, {"id": 3}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_with_page_size(&server, 2);
    let capped: Vec<Item> = client.list_all(Endpoint::Sites, vec![], 3).await.unwrap();
    assert_eq!(
        capped.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn page_size_is_clamped_to_server_window() {
    // R1: a profile page size above the server cap is clamped to 1000, and `0`
    // ("return ALL" to NetBox) falls back to the default 100. Assert both the
    // stored size and the `limit` actually sent on the wire.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("limit", "1000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .expect(1)
        .mount(&server)
        .await;

    let huge = ProfileConfig {
        url: server.uri(),
        page_size: Some(5000),
        ..Default::default()
    };
    let client = NetBoxClient::new(&huge, None).unwrap();
    // The clamp itself is what's proven here: a 5000 page size lands at the 1000 window.
    assert_eq!(client.page_size(), 1000);
    // On the wire the floor then dominates for a small max: limit = max(1000, 100) = 1000
    // (matched by the mock above) — incidental to the clamp, not the clamp itself.
    let _: Vec<Item> = client.list_all(Endpoint::Sites, vec![], 100).await.unwrap();

    // `page_size = 0` means "all" to NetBox; we fall back to the default 100.
    let zero = ProfileConfig {
        url: server.uri(),
        page_size: Some(0),
        ..Default::default()
    };
    assert_eq!(NetBoxClient::new(&zero, None).unwrap().page_size(), 100);

    // Unset also defaults to 100.
    let unset = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    assert_eq!(NetBoxClient::new(&unset, None).unwrap().page_size(), 100);
}

#[tokio::test]
async fn list_all_returns_empty_for_zero_results() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_with_page_size(&server, 2);
    let all: Vec<Item> = client.list_all(Endpoint::Sites, vec![], 100).await.unwrap();
    assert!(all.is_empty());
}
