//! File-backed JSON output contracts.
//!
//! These are intentionally broader than unit tests: they pin the exact pretty
//! JSON emitted by the shared output renderer for machine-facing shapes. When a
//! contract changes intentionally, update the matching file in `tests/golden/`
//! in the same commit so reviewers see the API surface change directly.

mod support;

use support::fixtures;
use support::json_contract::assert_golden;

#[test]
fn status_json_contract() {
    assert_golden(
        &fixtures::status_report(),
        include_str!("golden/status.json"),
    );
}

#[test]
fn search_json_contract() {
    assert_golden(
        &fixtures::search_results(),
        include_str!("golden/search.json"),
    );
}

#[test]
fn device_detail_json_contract() {
    assert_golden(
        &fixtures::device_detail().build(),
        include_str!("golden/device_detail.json"),
    );
}

#[test]
fn vrf_detail_json_contract() {
    assert_golden(
        &fixtures::vrf_detail(),
        include_str!("golden/vrf_detail.json"),
    );
}

#[test]
fn ip_json_contract() {
    assert_golden(&fixtures::ip_view(), include_str!("golden/ip.json"));
}

#[test]
fn prefix_json_contract() {
    assert_golden(&fixtures::prefix_view(), include_str!("golden/prefix.json"));
}
