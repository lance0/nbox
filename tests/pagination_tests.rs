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

/// A helper page response carrying `count` total objects, returning the rows for
/// the given `offset` window (page size 2).
fn page_at(offset: usize, count: usize) -> ResponseTemplate {
    let ids: Vec<_> = (offset + 1..=(offset + 2).min(count))
        .map(|id| json!({ "id": id }))
        .collect();
    let next = if offset + 2 < count {
        json!(format!("?offset={}", offset + 2))
    } else {
        json!(null)
    };
    ResponseTemplate::new(200).set_body_json(json!({
        "count": count,
        "next": next,
        "previous": null,
        "results": ids,
    }))
}

#[tokio::test]
async fn list_all_follows_three_pages() {
    let server = MockServer::start().await;
    // 5 objects, page size 2 → three pages at offset 0, 2, 4.
    for offset in [0usize, 2, 4] {
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/"))
            .and(query_param("offset", offset.to_string()))
            .respond_with(page_at(offset, 5))
            .expect(1)
            .mount(&server)
            .await;
    }

    let profile = ProfileConfig {
        url: server.uri(),
        page_size: Some(2),
        ..Default::default()
    };
    let client = NetBoxClient::new(&profile, None).unwrap();

    let all: Vec<Item> = client.list_all(Endpoint::Sites, vec![], 100).await.unwrap();
    assert_eq!(
        all.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
}

#[tokio::test]
async fn list_all_stops_at_count_when_max_exceeds_total() {
    let server = MockServer::start().await;
    // 3 objects total; the client must stop after page two without requesting a
    // (nonexistent) third page, even though `max` is far larger than the total.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("offset", "0"))
        .respond_with(page_at(0, 3))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("offset", "2"))
        .respond_with(page_at(2, 3))
        .expect(1)
        .mount(&server)
        .await;
    // No offset=4 mock: requesting it would 404 and fail the test.

    let profile = ProfileConfig {
        url: server.uri(),
        page_size: Some(2),
        ..Default::default()
    };
    let client = NetBoxClient::new(&profile, None).unwrap();

    let all: Vec<Item> = client
        .list_all(Endpoint::Sites, vec![], 1000)
        .await
        .unwrap();
    assert_eq!(all.iter().map(|i| i.id).collect::<Vec<_>>(), vec![1, 2, 3]);
}

#[tokio::test]
async fn list_all_truncates_when_cap_lands_mid_page() {
    let server = MockServer::start().await;
    // 5 objects, page size 2, cap 3. After two pages we hold 4 (>= cap), so we
    // stop without fetching page three and truncate to exactly 3.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("offset", "0"))
        .respond_with(page_at(0, 5))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("offset", "2"))
        .respond_with(page_at(2, 5))
        .expect(1)
        .mount(&server)
        .await;
    // offset=4 must never be requested (cap reached after offset=2).

    let profile = ProfileConfig {
        url: server.uri(),
        page_size: Some(2),
        ..Default::default()
    };
    let client = NetBoxClient::new(&profile, None).unwrap();

    let capped: Vec<Item> = client.list_all(Endpoint::Sites, vec![], 3).await.unwrap();
    assert_eq!(
        capped.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[tokio::test]
async fn list_all_handles_cap_on_exact_page_boundary() {
    let server = MockServer::start().await;
    // 6 objects, page size 2, cap 4. We expect exactly pages at offset 0 and 2
    // (4 rows == cap → stop), and never a request at offset 4.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("offset", "0"))
        .respond_with(page_at(0, 6))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("offset", "2"))
        .respond_with(page_at(2, 6))
        .expect(1)
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        url: server.uri(),
        page_size: Some(2),
        ..Default::default()
    };
    let client = NetBoxClient::new(&profile, None).unwrap();

    let capped: Vec<Item> = client.list_all(Endpoint::Sites, vec![], 4).await.unwrap();
    assert_eq!(
        capped.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
}

#[tokio::test]
async fn list_all_advances_by_page_size_when_page_returns_short() {
    // B1 regression: the requested page size is 4, but every page comes back with
    // only 3 rows (e.g. the server capped `limit`, or a serializer dropped a row
    // post-count). NetBox `offset` windows are absolute (page N = offset N*limit),
    // so paging must advance by the requested size — not by the rows returned.
    //
    // Total 9 rows across windows [0,4), [4,8), [8,12). At page size 4, each
    // window holds ids {1,2,3}, {5,6,7}, {9}: rows at offsets 3 and 7 are absent
    // (the simulated short page). The fix collects exactly the rows present, once.
    //
    // On the old `offset += got`, offsets would walk 0, 3, 6, … missing the
    // mounted windows (4, 8) → the request 404s with no matching mock and the
    // call errors, failing this test. On `offset += page_size` it walks 0, 4, 8.
    let server = MockServer::start().await;

    // Which absolute ids exist (the "short page" drops one id per 4-wide window).
    let present = [1u64, 2, 3, 5, 6, 7, 9];
    let count = present.len();

    for offset in [0usize, 4, 8] {
        let ids: Vec<_> = present
            .iter()
            .filter(|&&id| {
                let id = id as usize;
                id > offset && id <= offset + 4
            })
            .map(|&id| json!({ "id": id }))
            .collect();
        let next = if offset + 4 < 12 {
            json!(format!("?offset={}", offset + 4))
        } else {
            json!(null)
        };
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/"))
            .and(query_param("offset", offset.to_string()))
            .and(query_param("limit", "4"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": count,
                "next": next,
                "previous": null,
                "results": ids,
            })))
            .expect(1)
            .mount(&server)
            .await;
    }

    let profile = ProfileConfig {
        url: server.uri(),
        page_size: Some(4),
        ..Default::default()
    };
    let client = NetBoxClient::new(&profile, None).unwrap();

    let all: Vec<Item> = client.list_all(Endpoint::Sites, vec![], 100).await.unwrap();
    // Every present row, exactly once, in order — no skips, no duplicates.
    assert_eq!(
        all.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![1, 2, 3, 5, 6, 7, 9]
    );
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
    assert_eq!(client.page_size(), 1000);
    // The outgoing `limit` is the clamped value (matched by the mock above).
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

    let profile = ProfileConfig {
        url: server.uri(),
        page_size: Some(2),
        ..Default::default()
    };
    let client = NetBoxClient::new(&profile, None).unwrap();

    let all: Vec<Item> = client.list_all(Endpoint::Sites, vec![], 100).await.unwrap();
    assert!(all.is_empty());
}
