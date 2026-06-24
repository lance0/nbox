//! Integration tests for `list_all` pagination.
//!
//! `list_all` sizes the first page to `max(page_size, min(max, MAX_PAGE_SIZE))`,
//! so a fetch with `max <= MAX_PAGE_SIZE` (1000) lands in a single request; a
//! larger `max` pages. From the second page on it follows the server's `next`
//! link (DRF `LimitOffsetPagination`) rather than computing offsets itself —
//! `next`'s offset uses the *capped* limit, so a server that shrinks our
//! requested page below `page_size` still advances row-by-row with no gap. These
//! cover the single-request path, the multi-page `next`-following path, a
//! mid-stream short page, and the MAX_PAGE_SIZE-cap regression.

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
    // `max` (2500) exceeds the server window, so the first page caps at 1000 and
    // the fetch pages 1000/1000/500 — following the server's `next` link each
    // time. Each mock matches its page's `offset` and `limit=1000` exactly, so a
    // request that strays from the `next` link gets no reply and the call fails.
    let server = MockServer::start().await;
    let next_url = |offset: usize| {
        format!(
            "{}/api/dcim/sites/?limit=1000&offset={offset}",
            server.uri()
        )
    };
    let page = |offset: usize, n: usize, next: Option<String>| {
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/"))
            .and(query_param("limit", "1000"))
            .and(query_param("offset", offset.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 2500, "next": next, "previous": null,
                "results": id_rows(offset, n)
            })))
            .expect(1)
            .mount(&server)
    };
    page(0, 1000, Some(next_url(1000))).await;
    page(1000, 1000, Some(next_url(2000))).await;
    page(2000, 500, None).await;

    let client = client_with_page_size(&server, 100);
    let all: Vec<Item> = client
        .list_all(Endpoint::Sites, vec![], 2500)
        .await
        .unwrap();
    assert_eq!(all.len(), 2500);
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        3,
        "three aligned 1000-row trips following next"
    );
}

#[tokio::test]
async fn list_all_follows_next_past_a_short_first_page() {
    // The FIRST page comes back short (999 rows for a 1000 window — a server
    // `limit` cap or a serializer dropping a row post-count). The server's `next`
    // link still points to offset=1000 (DRF sizes it with the capped limit, not
    // the rows returned), so following it keeps the windows aligned — no
    // misalignment, no duplicate refetch. The dropped row is the server's loss,
    // not a client skip. (On the old `offset += got`, page two would request
    // offset=999 — a misaligned window with no mounted mock → 404 → failure.)
    let server = MockServer::start().await;
    let next_url = |offset: usize| {
        format!(
            "{}/api/dcim/sites/?limit=1000&offset={offset}",
            server.uri()
        )
    };
    let page = |offset: usize, ids: Vec<serde_json::Value>, next: Option<String>| {
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/"))
            .and(query_param("limit", "1000"))
            .and(query_param("offset", offset.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 2001, "next": next, "previous": null, "results": ids
            })))
            .expect(1)
            .mount(&server)
    };
    page(0, id_rows(0, 999), Some(next_url(1000))).await; // short page (999, not 1000)
    page(1000, id_rows(1000, 1000), Some(next_url(2000))).await;
    page(2000, id_rows(2000, 2), None).await;

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
        "followed next past the short first page (offset 0/1000/2000)"
    );
}

#[tokio::test]
async fn list_all_follows_next_when_the_server_caps_the_page_below_the_request() {
    // The KNOWN_ISSUES regression: we request `limit=1000` (page_size grown to
    // `max`), but the server caps `MAX_PAGE_SIZE` at 500, returning a short 500-
    // row page. The server's `next` link points to `offset=500` (DRF sizes it
    // with the CAPPED limit, 500, not our requested 1000), so following it
    // fetches rows 500-999 next. Computing `offset += page_size` (1000) would
    // jump to offset=1000 and SILENTLY SKIP rows 500-999 — this asserts that gap
    // never opens: all 1000 ids come back contiguous.
    let server = MockServer::start().await;
    let next_url =
        |offset: usize| format!("{}/api/dcim/sites/?limit=500&offset={offset}", server.uri());
    // Page 1: we SEND limit=1000 (grown page_size); the server caps to 500 and
    // returns 500 rows, pointing `next` at offset=500 (the capped limit).
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("limit", "1000"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1000, "next": next_url(500), "previous": null,
            "results": id_rows(0, 500)
        })))
        .expect(1)
        .mount(&server)
        .await;
    // Page 2: we follow `next`, so we SEND limit=500&offset=500 (from the link,
    // not our grown 1000). Rows 500-999, no more pages.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("limit", "500"))
        .and(query_param("offset", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1000, "next": null, "previous": null,
            "results": id_rows(500, 500)
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_with_page_size(&server, 100);
    let all: Vec<Item> = client
        .list_all(Endpoint::Sites, vec![], 1000)
        .await
        .unwrap();
    // All 1000 rows, contiguous 0-999 — the gap at 500 (the old bug) never opens.
    assert_eq!(all.len(), 1000);
    assert_eq!(
        all.iter().map(|i| i.id).collect::<Vec<_>>(),
        (0u64..1000).collect::<Vec<_>>(),
        "no rows skipped where the server capped the page below the request"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
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
