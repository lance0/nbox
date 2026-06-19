//! Integration tests: the global JSON-family flags (`--json`, `--fields`,
//! `--raw`, `--envelope`) take effect uniformly across data-producing commands.
//!
//! Every shell command renders its JSON through the one shared shaping path,
//! `output::json::render_with` (what `output::emit` / the CLI's `emit` call for
//! `Format::Json`). These tests build the exact serializable value each `run_*`
//! handler hands to that path — fetching real views via wiremock where a NetBox
//! call is involved, or constructing the local value otherwise — and assert the
//! four flags shape it identically regardless of command:
//!   * `--fields k1,k2` keeps only those top-level keys;
//!   * `--raw` emits compact single-line JSON;
//!   * `--envelope` wraps as `{schema_version, data}` and composes with the rest.

use nbox::config::{Config, ProfileConfig};
use nbox::domain::detail;
use nbox::domain::journal_view::JournalView;
use nbox::domain::tag_view::TagsView;
use nbox::netbox::client::NetBoxClient;
use nbox::output::json::{JsonOptions, SCHEMA_VERSION, render_with};
use serde::Serialize;
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

fn not_found(noun: &str, value: &str) -> anyhow::Error {
    anyhow::anyhow!("no {noun} matched \"{value}\"")
}

/// Parse the JSON a command would emit with `--json` plus the given options.
fn shaped<T: Serialize>(value: &T, opts: &JsonOptions) -> Value {
    let rendered = render_with(value, opts).unwrap();
    serde_json::from_str(&rendered).unwrap()
}

fn fields(list: &[&str]) -> JsonOptions {
    JsonOptions {
        fields: Some(list.iter().map(ToString::to_string).collect()),
        ..Default::default()
    }
}

/// The full flag battery applied to a single object: `--fields`, `--raw`,
/// `--envelope`, and their composition all behave as advertised.
fn assert_object_flags(value: &impl Serialize, keep: &[&str], expected_drop: &str) {
    let full = shaped(value, &JsonOptions::default());
    assert!(
        full.get(keep[0]).is_some(),
        "baseline JSON has the kept key {:?}: {full}",
        keep[0]
    );

    // --fields keeps only the requested top-level keys.
    let trimmed = shaped(value, &fields(keep));
    let obj = trimmed.as_object().expect("object after field-select");
    for k in keep {
        assert!(obj.contains_key(*k), "kept key {k} present: {trimmed}");
    }
    assert!(
        !obj.contains_key(expected_drop),
        "dropped key {expected_drop} absent: {trimmed}"
    );

    // --raw is single-line compact (and still valid JSON).
    let raw = render_with(
        value,
        &JsonOptions {
            raw: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!raw.contains('\n'), "raw is single-line: {raw:?}");
    let _: Value = serde_json::from_str(&raw).unwrap();

    // --envelope wraps as {schema_version, data}.
    let env = shaped(
        value,
        &JsonOptions {
            envelope: true,
            ..Default::default()
        },
    );
    assert_eq!(env["schema_version"], json!(SCHEMA_VERSION));
    assert_eq!(env["data"], full);

    // --fields + --envelope + --raw compose: field-select happens inside the
    // envelope, the whole thing on one line.
    let composed_raw = render_with(
        value,
        &JsonOptions {
            fields: Some(keep.iter().map(ToString::to_string).collect()),
            raw: true,
            envelope: true,
        },
    )
    .unwrap();
    assert!(!composed_raw.contains('\n'), "composed raw is single-line");
    let composed: Value = serde_json::from_str(&composed_raw).unwrap();
    assert_eq!(composed["schema_version"], json!(SCHEMA_VERSION));
    let data = composed["data"].as_object().expect("data object");
    assert!(data.contains_key(keep[0]), "composed keeps key: {composed}");
    assert!(
        !data.contains_key(expected_drop),
        "composed drops key: {composed}"
    );
}

/// The same battery applied to a list payload: field-select applies per element,
/// envelope wraps the whole array, raw stays single-line.
fn assert_array_flags(value: &impl Serialize, keep: &[&str], expected_drop: &str) {
    let full = shaped(value, &JsonOptions::default());
    let arr = full.as_array().expect("baseline is an array");
    assert!(!arr.is_empty(), "non-empty array for the test");

    let trimmed = shaped(value, &fields(keep));
    for el in trimmed.as_array().expect("array after field-select") {
        let obj = el.as_object().expect("array element is an object");
        for k in keep {
            assert!(obj.contains_key(*k), "kept key {k}: {trimmed}");
        }
        assert!(!obj.contains_key(expected_drop), "dropped key: {trimmed}");
    }

    let raw = render_with(
        value,
        &JsonOptions {
            raw: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!raw.contains('\n'), "raw array is single-line");

    let env = shaped(
        value,
        &JsonOptions {
            envelope: true,
            ..Default::default()
        },
    );
    assert_eq!(env["schema_version"], json!(SCHEMA_VERSION));
    assert_eq!(env["data"], full);
}

#[tokio::test]
async fn device_detail_json_honors_all_flags() {
    let server = MockServer::start().await;
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

    let view = detail::device_detail_by_ref(&client(&server), "edge01", &not_found)
        .await
        .unwrap();

    // Keep `name`, drop `id` — both are top-level keys on the device view.
    assert_object_flags(&view, &["name", "status"], "id");
}

#[tokio::test]
async fn search_results_json_honors_all_flags() {
    let server = MockServer::start().await;
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
    for ep in [
        "/api/dcim/sites/",
        "/api/ipam/ip-addresses/",
        "/api/ipam/prefixes/",
        "/api/ipam/vlans/",
        "/api/circuits/circuits/",
        "/api/ipam/aggregates/",
        "/api/ipam/asns/",
        "/api/ipam/ip-ranges/",
    ] {
        Mock::given(method("GET"))
            .and(path(ep))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 0, "next": null, "previous": null, "results": []
            })))
            .mount(&server)
            .await;
    }

    let results = client(&server)
        .search(nbox::netbox::search::SearchRequest {
            query: "edge01".into(),
            limit: 25,
            filters: nbox::netbox::search::SearchFilters::default(),
        })
        .await
        .unwrap()
        .results;

    // Search emits an array of result rows; field-select trims each element.
    assert_array_flags(&results, &["kind", "display"], "url");
}

#[tokio::test]
async fn tags_json_honors_all_flags() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/extras/tags/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "name": "Critical", "slug": "critical",
                "color": "ff0000", "tagged_items": 12
            }]
        })))
        .mount(&server)
        .await;

    let view = TagsView::from_models(client(&server).tags(200).await.unwrap());

    // `TagsView` is an object with a `tags` array; field-select keeps `tags`.
    assert_object_flags(&view, &["tags"], "missing_key_xyz");

    // The nested rows survive field selection on the wrapper untouched, and the
    // array shaping rules apply when the rows themselves are the payload.
    assert_array_flags(&view.tags, &["slug", "name"], "color");
}

#[tokio::test]
async fn journal_list_json_honors_all_flags() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/extras/journal-entries/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 5, "created": "2024-03-02",
                "kind": {"value": "info", "label": "Info"},
                "created_by": {"username": "admin", "display": "admin"},
                "comments": "rebooted"
            }]
        })))
        .mount(&server)
        .await;

    let entries = client(&server)
        .journal_entries("dcim.device", 7, 20)
        .await
        .unwrap();
    let view = JournalView::from_models(entries);

    assert_object_flags(&view, &["entries"], "missing_key_xyz");
    assert_array_flags(&view.entries, &["comments", "created"], "kind");
}

#[test]
fn status_report_json_honors_all_flags() {
    // The exact value `run_status` hands to `emit` for `--json`.
    let report = json!({
        "netbox_url": "https://netbox.example.com",
        "backend": "rest",
        "netbox_version": "4.2.0",
        "django_version": "5.0.9",
        "python_version": "3.11.2",
    });
    assert_object_flags(&report, &["netbox_url", "netbox_version"], "django_version");
}

#[test]
fn config_show_json_honors_all_flags() {
    // `config show` serializes the typed `Config`. Build one and exercise flags.
    let toml = r#"
config_version = 1
active_profile = "work"

[ui]
theme = "nord"

[profiles.work]
url = "https://netbox.example.com"
token_env = "NETBOX_TOKEN"
"#;
    let cfg: Config = toml::from_str(toml).unwrap();
    // Keep `active_profile`, drop `profiles` (both top-level config keys).
    assert_object_flags(&cfg, &["active_profile", "ui"], "profiles");
}

#[test]
fn profile_list_json_honors_all_flags() {
    // `profile list` emits a JSON array of profile names.
    let names = vec!["work".to_string(), "lab".to_string()];
    let full = shaped(&names, &JsonOptions::default());
    assert_eq!(full, json!(["work", "lab"]));

    // --raw is single-line.
    let raw = render_with(
        &names,
        &JsonOptions {
            raw: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(raw, r#"["work","lab"]"#);

    // --envelope wraps the array.
    let env = shaped(
        &names,
        &JsonOptions {
            envelope: true,
            ..Default::default()
        },
    );
    assert_eq!(env["schema_version"], json!(SCHEMA_VERSION));
    assert_eq!(env["data"], json!(["work", "lab"]));

    // --fields is a no-op on an array of scalars (no object keys to select),
    // leaving the names intact rather than erroring.
    let trimmed = shaped(&names, &fields(&["anything"]));
    assert_eq!(trimmed, json!(["work", "lab"]));
}

#[test]
fn profile_show_json_honors_all_flags() {
    // `profile show` serializes a single `ProfileConfig`.
    let profile = ProfileConfig {
        url: "https://netbox.example.com".into(),
        token_env: Some("NETBOX_TOKEN".into()),
        page_size: Some(250),
        ..Default::default()
    };
    assert_object_flags(&profile, &["url"], "token_env");
}
