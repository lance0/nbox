//! Integration tests: custom fields surface as `cf.<name>` rows in plain output
//! and as a `custom_fields` object in JSON, exercised through the shared view
//! fetch path (`detail::*_by_ref`) the CLI/MCP/TUI all use — not just the unit
//! serializers — so the wire → model → view → render chain is covered end to end.

use nbox::config::ProfileConfig;
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

#[tokio::test]
async fn site_custom_fields_render_in_plain_and_json() {
    let server = MockServer::start().await;
    // `site_by_ref` tries slug first; this responds on the slug lookup.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/dcim/sites/1/",
                "name": "iad1", "slug": "iad1",
                "status": {"value": "active", "label": "Active"},
                "custom_fields": {
                    "ticket": "INC-42",
                    "monitored": true,
                    "rack_units": 4,
                    "owner": null,
                    "notes": ""
                }
            }]
        })))
        .mount(&server)
        .await;

    let view = detail::site_view_by_ref(&client(&server), "iad1", &not_found)
        .await
        .unwrap();

    // Plain: non-empty custom fields appear as ordered `cf.<name>` rows; null and
    // empty-string values are dropped.
    let plain = view.to_key_values().render();
    assert!(plain.contains("cf.monitored: true"), "got: {plain}");
    assert!(plain.contains("cf.rack_units: 4"), "got: {plain}");
    assert!(plain.contains("cf.ticket: INC-42"), "got: {plain}");
    assert!(!plain.contains("cf.owner"), "null dropped: {plain}");
    assert!(!plain.contains("cf.notes"), "empty dropped: {plain}");
    // BTreeMap ordering: monitored < rack_units < ticket.
    let mon = plain.find("cf.monitored").unwrap();
    let ru = plain.find("cf.rack_units").unwrap();
    let tk = plain.find("cf.ticket").unwrap();
    assert!(
        mon < ru && ru < tk,
        "cf rows should be name-ordered: {plain}"
    );

    // JSON: a `custom_fields` object carrying the typed, non-empty values.
    let v: Value = serde_json::to_value(&view).unwrap();
    let cf = &v["custom_fields"];
    assert_eq!(cf["ticket"], json!("INC-42"));
    assert_eq!(cf["monitored"], json!(true));
    assert_eq!(cf["rack_units"], json!(4));
    assert!(cf.get("owner").is_none(), "null should be dropped: {cf}");
    assert!(cf.get("notes").is_none(), "empty should be dropped: {cf}");
}

#[tokio::test]
async fn site_without_custom_fields_omits_the_object_in_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "ord1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 2, "url": "http://nb/api/dcim/sites/2/",
                "name": "ord1", "slug": "ord1",
                "custom_fields": {"owner": null, "notes": ""}
            }]
        })))
        .mount(&server)
        .await;

    let view = detail::site_view_by_ref(&client(&server), "ord1", &not_found)
        .await
        .unwrap();

    // No non-empty custom fields → no `cf.` rows and the JSON key is skipped
    // (BTreeMap::is_empty serde guard).
    let plain = view.to_key_values().render();
    assert!(!plain.contains("cf."), "no cf rows expected: {plain}");
    let v: Value = serde_json::to_value(&view).unwrap();
    assert!(
        v.get("custom_fields").is_none(),
        "empty custom_fields object should be omitted: {v}"
    );
}

#[tokio::test]
async fn device_custom_fields_render_in_plain_and_json() {
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
                "custom_fields": {"ticket": "INC-7", "owner": null}
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

    let detail = detail::device_detail_by_ref(&client(&server), "edge01", &not_found)
        .await
        .unwrap();

    // Plain device summary carries the cf row.
    let plain = detail.to_plain();
    assert!(plain.contains("cf.ticket: INC-7"), "got: {plain}");
    assert!(!plain.contains("owner"), "null dropped: {plain}");

    // JSON: DeviceDetail flattens its DeviceView summary, so `custom_fields`
    // sits at the top level of the device object.
    let v: Value = serde_json::to_value(&detail).unwrap();
    assert_eq!(v["custom_fields"]["ticket"], json!("INC-7"));
    assert!(
        v["custom_fields"].get("owner").is_none(),
        "null should be dropped: {v}"
    );
}
