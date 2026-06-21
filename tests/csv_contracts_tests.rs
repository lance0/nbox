//! Integration tests: the CSV output-mode contract (`-o csv` / `--output csv`).
//!
//! CSV is the tabular sibling of the JSON-family flags pinned in
//! `output_flags_tests.rs`. Where those assert the JSON shaping path, these pin
//! the CSV one: how a list/array result renders as a table, how the `search`
//! command's `--cols` selects and orders columns, what an empty array produces,
//! and that a single (non-array) object is rejected as a usage error (exit 2).
//!
//! Like the JSON tests, the list cases build the exact serializable value the
//! `run_search` handler hands to the CSV path — a real `Vec<SearchResult>`
//! fetched through wiremock — and render it via `output::csv::to_csv`. The
//! single-object rejection is pinned both at the `emit()`/render layer (the
//! shared output path) and at the process boundary (the compiled binary), the
//! latter matching the established `error_contract_tests.rs` style for exit
//! codes.
//!
//! These are contracts only: they pin behavior as it is today. No `src/` change.

mod support;

use nbox::error::NboxError;
use nbox::output::csv::to_csv;
use nbox::output::json::JsonOptions;
use nbox::output::{Format, emit};
use serde_json::json;
use support::binary::{run_nbox, temp_config};
use support::netbox::{client, mount_empty_list, page};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The literal usage-error message for `-o csv` on a single object. Pinned here
/// (not imported) so the test fails loudly if the production string ever drifts.
const CSV_NOT_TABULAR: &str = "CSV output is only supported for tabular results (arrays). For single objects, use --json or plain text.";

/// Parse a `--cols` argument exactly as `run_search` does (split on `,`, trim,
/// drop empties) so the render-layer column tests exercise the real selection.
fn parse_cols(spec: &str) -> Vec<String> {
    spec.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Fetch the real `Vec<SearchResult>` the `search` command produces for a single
/// device hit (everything else empty), mirroring `output_flags_tests.rs`.
async fn one_device_search() -> Vec<nbox::netbox::search::SearchResult> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![json!({
                "id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01",
                "site": {"id": 9, "display": "iad1"}
        })])))
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
        mount_empty_list(&server, ep).await;
    }

    client(&server)
        .search(nbox::netbox::search::SearchRequest {
            query: "edge01".into(),
            limit: 25,
            filters: nbox::netbox::search::SearchFilters::default(),
        })
        .await
        .unwrap()
        .results
}

#[tokio::test]
async fn search_results_render_as_a_table_with_default_columns() {
    // The exact value `run_search` serializes for `-o csv`. Default columns are
    // inferred as the union of object keys in first-seen order. Because
    // serde_json maps keys are sorted (no `preserve_order` feature), the inferred
    // header is the SearchResult fields in *alphabetical* order, not struct
    // order: display,id,kind,score,subtitle,url. A device hit carries the site
    // as its subtitle, so that optional column is present here. ObjectKind
    // serializes snake_case ("device"); the `url` is the normalized object URL
    // the client surfaces (the `/api` segment dropped), pinned as produced.
    let results = one_device_search().await;
    let value = serde_json::to_value(&results).unwrap();
    let csv = to_csv(&value, None).unwrap();

    assert_eq!(
        csv,
        "display,id,kind,score,subtitle,url\n\
         edge01,1,device,100,iad1,http://nb/dcim/devices/1/\n"
    );
}

#[tokio::test]
async fn cols_select_and_order_columns_for_search_csv() {
    // `--cols display,kind,url` selects those columns in that exact order,
    // regardless of the natural field order in the struct.
    let results = one_device_search().await;
    let value = serde_json::to_value(&results).unwrap();

    let cols = parse_cols("display,kind,url");
    assert_eq!(
        to_csv(&value, Some(&cols)).unwrap(),
        "display,kind,url\n\
         edge01,device,http://nb/dcim/devices/1/\n"
    );
}

#[test]
fn cols_with_an_unknown_column_emits_an_empty_cell() {
    // An explicitly requested column that no row carries is not an error: the
    // header still lists it and every row gets an empty cell for it, so the
    // shape stays predictable for scripts.
    let value = json!([{"kind": "device", "display": "edge01"}]);
    let cols = parse_cols("display,nope,kind");
    assert_eq!(
        to_csv(&value, Some(&cols)).unwrap(),
        "display,nope,kind\nedge01,,device\n"
    );
}

#[test]
fn cols_parsing_trims_whitespace_and_drops_empty_fields() {
    // The `--cols` parse splits on `,`, trims each token, and drops empties, so
    // " a , , b " selects exactly [a, b].
    let value = json!([{"a": 1, "b": 2, "c": 3}]);

    let trimmed = parse_cols(" a , , b ");
    assert_eq!(trimmed, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(to_csv(&value, Some(&trimmed)).unwrap(), "a,b\n1,2\n");

    // A spec of only separators/whitespace parses to an empty Vec. Note this is
    // NOT the same as `None`: `Some([])` selects *zero* columns explicitly, so
    // the renderer emits an empty header line plus one empty (zero-column) row
    // per item — it does not fall back to inferred columns. (`None` is the
    // inferring case, covered by the default-columns test.)
    let empty = parse_cols(" , , ");
    assert!(empty.is_empty());
    assert_eq!(to_csv(&value, Some(&empty)).unwrap(), "\n\n");
}

#[test]
fn empty_array_renders_a_single_empty_header_line() {
    // No items -> no columns to infer -> one empty header line (just a newline).
    assert_eq!(to_csv(&json!([]), None).unwrap(), "\n");

    // With explicit cols the header is those columns and there are zero rows.
    let cols = parse_cols("kind,display");
    assert_eq!(to_csv(&json!([]), Some(&cols)).unwrap(), "kind,display\n");
}

#[test]
fn single_object_is_rejected_at_the_render_layer() {
    // CSV is tabular-only: a bare object is a usage error carrying the stable
    // exit code 2, with the exact message — no `field,value` fallback.
    let value = json!({
        "name": "iad1",
        "status": "active",
        "custom_fields": {"owner": "neteng"}
    });
    let err = to_csv(&value, None).unwrap_err();
    assert_eq!(format!("{err:#}"), CSV_NOT_TABULAR);
    assert_eq!(NboxError::exit_code_for(&err), 2);
}

#[test]
fn single_object_is_rejected_through_the_emit_path() {
    // The same rejection holds through the shared `output::emit` path every
    // data-producing command funnels into (where `-o csv` is wired). The plain
    // closure must not run, since the format is CSV.
    let value = json!({"name": "iad1", "status": "active"});
    let err = emit(Format::Csv, &JsonOptions::default(), &value, || {
        panic!("plain closure must not run for -o csv");
    })
    .unwrap_err();
    assert_eq!(format!("{err:#}"), CSV_NOT_TABULAR);
    assert_eq!(NboxError::exit_code_for(&err), 2);
}

#[tokio::test]
async fn binary_csv_on_a_single_object_exits_2_with_the_usage_message() {
    // At the process boundary, `nbox <detail> -o csv` (a single object) is a
    // usage error: exit 2, clean stdout, the usage message on stderr. Pins the
    // CLI contract the way error_contract_tests.rs pins the others.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![json!({
                "id": 7, "url": "http://nb/api/dcim/devices/7/", "name": "edge01",
                "status": {"value": "active", "label": "Active"},
                "custom_fields": {}
        })])))
        .mount(&server)
        .await;
    for ep in [
        "/api/dcim/interfaces/",
        "/api/ipam/ip-addresses/",
        "/api/ipam/services/",
    ] {
        mount_empty_list(&server, ep).await;
    }

    let config = temp_config(&server.uri());
    let output = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "device".as_ref(),
        "edge01".as_ref(),
        "-o".as_ref(),
        "csv".as_ref(),
    ]);

    assert_eq!(output.code, Some(2), "stderr: {}", output.stderr);
    assert!(
        output.stdout.is_empty(),
        "error paths must keep stdout clean, got: {:?}",
        output.stdout
    );
    assert!(
        output.stderr.contains(CSV_NOT_TABULAR),
        "stderr should carry the usage message, got: {:?}",
        output.stderr
    );
}

#[tokio::test]
async fn binary_search_csv_honors_cols_order() {
    // End-to-end: the real `--cols` flag flows through `run_search` to the CSV
    // renderer, selecting and ordering columns on stdout. Pins the whole path
    // (parse + render) the way the binary actually runs it. `--partial` accepts
    // the unmounted endpoints (a documented mode); the CSV stays on clean stdout
    // while the partial warning goes to stderr.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![json!({
                "id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01",
                "site": {"id": 9, "display": "iad1"}
        })])))
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
        mount_empty_list(&server, ep).await;
    }

    let config = temp_config(&server.uri());
    let output = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "search".as_ref(),
        "edge01".as_ref(),
        "-o".as_ref(),
        "csv".as_ref(),
        "--cols".as_ref(),
        "display,kind,url".as_ref(),
        "--partial".as_ref(),
    ]);

    assert_eq!(output.code, Some(0), "stderr: {}", output.stderr);
    // stdout is exactly the CSV header + the one row, in the requested column
    // order (with the normalized object URL).
    assert_eq!(
        output.stdout,
        "display,kind,url\n\
         edge01,device,http://nb/dcim/devices/1/\n"
    );
}

#[tokio::test]
async fn binary_search_csv_quotes_a_value_containing_a_comma() {
    // RFC 4180 escaping survives the full binary path: a cell value containing a
    // comma (here a device's site display) is wrapped in quotes on real stdout, so
    // the row stays one logical record for a downstream parser. The render-layer
    // `escape` is unit-tested in `output::csv`; this pins it end-to-end.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![json!({
                "id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01",
                "site": {"id": 9, "display": "iad1, dc1"}
        })])))
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
        mount_empty_list(&server, ep).await;
    }

    let config = temp_config(&server.uri());
    let output = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "search".as_ref(),
        "edge01".as_ref(),
        "-o".as_ref(),
        "csv".as_ref(),
        "--cols".as_ref(),
        "display,subtitle".as_ref(),
        "--partial".as_ref(),
    ]);

    assert_eq!(output.code, Some(0), "stderr: {}", output.stderr);
    // The comma-bearing subtitle is quoted; the row is a single record.
    assert_eq!(output.stdout, "display,subtitle\nedge01,\"iad1, dc1\"\n");
}
