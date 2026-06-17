//! Integration tests for the additive `--journal` flag on detail commands.
//!
//! These exercise the same composed path the `run_*` handlers take when
//! `--journal` is passed: build the object's view via the shared
//! `detail::*_by_ref` fetch, pull its recent journal rows via
//! `detail::journal_rows`, and wrap the two in `WithJournal`. We assert the
//! wrapped JSON carries a top-level `journal` array, that the wrapped object is
//! otherwise byte-identical to the bare view (additive), and that the journal
//! entries map through the existing `JournalView`/`JournalEntryRow` shape.

use nbox::config::ProfileConfig;
use nbox::domain::WithJournal;
use nbox::domain::detail;
use nbox::netbox::client::NetBoxClient;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> NetBoxClient {
    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    NetBoxClient::new(&profile, None).unwrap()
}

/// A not-found closure standing in for the CLI's real one.
fn not_found(noun: &str, value: &str) -> anyhow::Error {
    anyhow::anyhow!("no {noun} matched \"{value}\"")
}

/// Mount a journal-entries response keyed on the object's content type and id.
async fn mock_journal(server: &MockServer, content_type: &str, object_id: u64, body: Value) {
    Mock::given(method("GET"))
        .and(path("/api/extras/journal-entries/"))
        .and(query_param("assigned_object_type", content_type))
        .and(query_param("assigned_object_id", object_id.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn device_with_journal_carries_entries_and_leaves_object_unchanged() {
    let server = MockServer::start().await;
    // `device_by_ref("edge01")` (non-numeric) hits the exact name__ie filter.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("name__ie", "edge01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 7, "url": "http://nb/api/dcim/devices/7/", "name": "edge01",
                "status": {"value": "active", "label": "Active"},
                "custom_fields": {}
            }]
        })))
        .mount(&server)
        .await;
    // The device detail fan-out pulls interfaces, IPs, and services (empty here).
    for ep in [
        "/api/dcim/interfaces/",
        "/api/ipam/ip-addresses/",
        "/api/ipam/services/",
    ] {
        Mock::given(method("GET"))
            .and(path(ep))
            .and(query_param("device_id", "7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 0, "next": null, "previous": null, "results": []
            })))
            .mount(&server)
            .await;
    }
    // The inline journal fetch addresses the device by its dotted content type.
    mock_journal(
        &server,
        "dcim.device",
        7,
        json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {
                    "id": 5, "created": "2024-03-02",
                    "kind": {"value": "info", "label": "Info"},
                    "created_by": {"username": "admin", "display": "admin"},
                    "comments": "rebooted"
                },
                {
                    "id": 4, "created": "2024-03-01",
                    "kind": {"value": "warning", "label": "Warning"},
                    "comments": "fan alert"
                }
            ]
        }),
    )
    .await;

    let cli = client(&server);

    // The bare view (no flag) — the additive baseline.
    let view = detail::device_detail_by_ref(&cli, "edge01", &not_found)
        .await
        .unwrap();
    let bare: Value = serde_json::to_value(&view).unwrap();
    assert!(bare.get("journal").is_none(), "bare view has no journal");

    // With `--journal`: fetch the rows and wrap.
    let entries = detail::journal_rows(&cli, "dcim.device", 7).await.unwrap();
    assert_eq!(entries.len(), 2);
    let wrapped = WithJournal::new(view, entries);
    let v: Value = serde_json::to_value(&wrapped).unwrap();

    // The wrapped JSON carries a top-level `journal` array (newest first).
    let journal = v["journal"].as_array().expect("journal array");
    assert_eq!(journal.len(), 2);
    assert_eq!(journal[0]["comments"], json!("rebooted"));
    assert_eq!(journal[0]["author"], json!("admin"));
    assert_eq!(journal[1]["comments"], json!("fan alert"));

    // Additive: stripping `journal` leaves an object byte-identical to the bare
    // view, so output without the flag is unchanged.
    let mut without = v.clone();
    without.as_object_mut().unwrap().remove("journal");
    assert_eq!(without, bare);
    assert_eq!(v["name"], json!("edge01"));
}

#[tokio::test]
async fn site_with_journal_carries_entries_and_leaves_object_unchanged() {
    let server = MockServer::start().await;
    // `site_by_ref` tries slug first.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/dcim/sites/1/",
                "name": "iad1", "slug": "iad1",
                "status": {"value": "active", "label": "Active"}
            }]
        })))
        .mount(&server)
        .await;
    mock_journal(
        &server,
        "dcim.site",
        1,
        json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 9, "created": "2024-02-10",
                "kind": {"value": "info", "label": "Info"},
                "created_by": {"display": "neteng"},
                "comments": "commissioned"
            }]
        }),
    )
    .await;

    let cli = client(&server);

    let view = detail::site_view_by_ref(&cli, "iad1", &not_found)
        .await
        .unwrap();
    let bare: Value = serde_json::to_value(&view).unwrap();
    assert!(bare.get("journal").is_none(), "bare view has no journal");

    let entries = detail::journal_rows(&cli, "dcim.site", 1).await.unwrap();
    assert_eq!(entries.len(), 1);
    let wrapped = WithJournal::new(view, entries);
    let v: Value = serde_json::to_value(&wrapped).unwrap();

    let journal = v["journal"].as_array().expect("journal array");
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0]["comments"], json!("commissioned"));
    assert_eq!(journal[0]["author"], json!("neteng"));

    // Additive: the site object is unchanged once `journal` is removed.
    let mut without = v.clone();
    without.as_object_mut().unwrap().remove("journal");
    assert_eq!(without, bare);
    assert_eq!(v["name"], json!("iad1"));
    assert_eq!(v["slug"], json!("iad1"));
}

#[tokio::test]
async fn aggregate_with_journal_carries_entries_and_leaves_object_unchanged() {
    let server = MockServer::start().await;
    // `aggregate_by_ref` (non-numeric) filters by exact prefix.
    Mock::given(method("GET"))
        .and(path("/api/ipam/aggregates/"))
        .and(query_param("prefix", "10.0.0.0/8"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "http://nb/api/ipam/aggregates/3/", "prefix": "10.0.0.0/8",
                "rir": {"id": 1, "display": "RFC 1918"}
            }]
        })))
        .mount(&server)
        .await;
    // The inline journal fetch addresses the aggregate by its dotted content type.
    mock_journal(
        &server,
        "ipam.aggregate",
        3,
        json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 11, "created": "2024-05-01",
                "kind": {"value": "info", "label": "Info"},
                "created_by": {"display": "ipam"},
                "comments": "allocated"
            }]
        }),
    )
    .await;

    let cli = client(&server);

    let view = detail::aggregate_view_by_ref(&cli, "10.0.0.0/8", &not_found)
        .await
        .unwrap();
    let bare: Value = serde_json::to_value(&view).unwrap();
    assert!(bare.get("journal").is_none(), "bare view has no journal");

    let entries = detail::journal_rows(&cli, "ipam.aggregate", 3)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    let wrapped = WithJournal::new(view, entries);
    let v: Value = serde_json::to_value(&wrapped).unwrap();

    let journal = v["journal"].as_array().expect("journal array");
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0]["comments"], json!("allocated"));
    assert_eq!(journal[0]["author"], json!("ipam"));

    // Additive: the aggregate object is unchanged once `journal` is removed.
    let mut without = v.clone();
    without.as_object_mut().unwrap().remove("journal");
    assert_eq!(without, bare);
    assert_eq!(v["prefix"], json!("10.0.0.0/8"));
}

#[tokio::test]
async fn asn_with_journal_carries_entries_and_leaves_object_unchanged() {
    let server = MockServer::start().await;
    // `asn_by_ref` filters by the AS number.
    Mock::given(method("GET"))
        .and(path("/api/ipam/asns/"))
        .and(query_param("asn", "64512"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 9, "url": "http://nb/api/ipam/asns/9/", "asn": 64512}]
        })))
        .mount(&server)
        .await;
    mock_journal(
        &server,
        "ipam.asn",
        9,
        json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 21, "created": "2024-06-01",
                "kind": {"value": "info", "label": "Info"},
                "created_by": {"display": "neteng"},
                "comments": "reserved"
            }]
        }),
    )
    .await;

    let cli = client(&server);

    let view = detail::asn_view_by_ref(&cli, 64512, "64512", &not_found)
        .await
        .unwrap();
    let bare: Value = serde_json::to_value(&view).unwrap();
    assert!(bare.get("journal").is_none(), "bare view has no journal");

    let entries = detail::journal_rows(&cli, "ipam.asn", 9).await.unwrap();
    assert_eq!(entries.len(), 1);
    let wrapped = WithJournal::new(view, entries);
    let v: Value = serde_json::to_value(&wrapped).unwrap();

    let journal = v["journal"].as_array().expect("journal array");
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0]["comments"], json!("reserved"));
    assert_eq!(journal[0]["author"], json!("neteng"));

    // Additive: the ASN object is unchanged once `journal` is removed.
    let mut without = v.clone();
    without.as_object_mut().unwrap().remove("journal");
    assert_eq!(without, bare);
    assert_eq!(v["asn"], json!(64512));
}

#[tokio::test]
async fn ip_range_with_journal_carries_entries_and_leaves_object_unchanged() {
    let server = MockServer::start().await;
    // `ip_range_by_ref` (non-numeric) filters by start address.
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-ranges/"))
        .and(query_param("start_address", "10.0.0.10/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 4, "url": "http://nb/api/ipam/ip-ranges/4/",
                "start_address": "10.0.0.10/24", "end_address": "10.0.0.20/24", "size": 11
            }]
        })))
        .mount(&server)
        .await;
    // The inline journal fetch addresses the range by its dotted content type.
    mock_journal(
        &server,
        "ipam.iprange",
        4,
        json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 31, "created": "2024-06-10",
                "kind": {"value": "info", "label": "Info"},
                "created_by": {"display": "ipam"},
                "comments": "carved out"
            }]
        }),
    )
    .await;

    let cli = client(&server);

    let view = detail::ip_range_view_by_ref(&cli, "10.0.0.10/24", &not_found)
        .await
        .unwrap();
    let bare: Value = serde_json::to_value(&view).unwrap();
    assert!(bare.get("journal").is_none(), "bare view has no journal");

    let entries = detail::journal_rows(&cli, "ipam.iprange", 4).await.unwrap();
    assert_eq!(entries.len(), 1);
    let wrapped = WithJournal::new(view, entries);
    let v: Value = serde_json::to_value(&wrapped).unwrap();

    let journal = v["journal"].as_array().expect("journal array");
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0]["comments"], json!("carved out"));
    assert_eq!(journal[0]["author"], json!("ipam"));

    // Additive: the range object is unchanged once `journal` is removed.
    let mut without = v.clone();
    without.as_object_mut().unwrap().remove("journal");
    assert_eq!(without, bare);
    assert_eq!(v["start_address"], json!("10.0.0.10/24"));
}

#[tokio::test]
async fn journal_rows_caps_at_inline_max() {
    let server = MockServer::start().await;
    // The endpoint returns more entries than the inline cap; `journal_rows` must
    // keep only the newest JOURNAL_INLINE_MAX of them.
    let results: Vec<Value> = (0..(detail::JOURNAL_INLINE_MAX + 3))
        .map(|i| {
            json!({
                "id": i, "created": format!("2024-04-{:02}", i + 1),
                "kind": {"value": "info", "label": "Info"},
                "comments": format!("entry {i}")
            })
        })
        .collect();
    mock_journal(
        &server,
        "dcim.device",
        7,
        json!({
            "count": results.len(), "next": null, "previous": null,
            "results": results
        }),
    )
    .await;

    let rows = detail::journal_rows(&client(&server), "dcim.device", 7)
        .await
        .unwrap();
    assert_eq!(rows.len(), detail::JOURNAL_INLINE_MAX);
}
