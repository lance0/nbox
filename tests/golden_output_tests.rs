//! File-backed JSON output contracts.
//!
//! These are intentionally broader than unit tests: they pin the exact pretty
//! JSON emitted by the shared output renderer for machine-facing shapes. When a
//! contract changes intentionally, update the matching file in `tests/golden/`
//! in the same commit so reviewers see the API surface change directly.

use std::collections::BTreeMap;

use nbox::domain::device_detail::{CableRow, DeviceDetail, IfaceRow, IpRow, ServiceRow, VlanRow};
use nbox::domain::device_view::DeviceView;
use nbox::netbox::search::{ObjectKind, SearchResult};
use nbox::output::json::{JsonOptions, render_with};
use serde::Serialize;
use serde_json::{Value, json};

fn assert_golden<T: Serialize>(value: &T, golden: &str) {
    let rendered = render_with(value, &JsonOptions::default()).expect("render JSON");
    assert_eq!(rendered, golden.trim_end());
}

#[test]
fn status_json_contract() {
    let report = json!({
        "netbox_url": "https://netbox.example.com/",
        "backend": "graphql",
        "netbox_version": "4.5.5",
        "django_version": "5.2.1",
        "python_version": "3.12.3",
        "capabilities": {
            "backend": "graphql",
            "version": {
                "netbox": "4.5.5",
                "minimum_supported": "4.2",
                "compatible": true
            },
            "rest": {
                "available": true,
                "search": true,
                "detail": true,
                "page_size": 250,
                "exclude_config_context": true
            },
            "graphql": {
                "configured": true,
                "probed": true,
                "available": true,
                "search": {
                    "lists_found": 14,
                    "paginated_lists": 14,
                    "missing_lists": []
                },
                "filters": {
                    "scope_type": {
                        "shape": "lookup",
                        "named_type": "ContentTypeFilter"
                    },
                    "scope_id": {
                        "shape": "scalar",
                        "named_type": "ID"
                    },
                    "site_id": {
                        "shape": "scalar",
                        "named_type": "ID"
                    },
                    "vrf_id": {
                        "shape": "scalar",
                        "named_type": "ID"
                    },
                    "tree_node_scope_ids": true
                }
            }
        }
    });

    assert_golden(&report, include_str!("golden/status.json"));
}

#[test]
fn search_json_contract() {
    let results = vec![
        SearchResult {
            kind: ObjectKind::Device,
            id: 7,
            display: "edge01".to_string(),
            subtitle: Some("den1".to_string()),
            url: "https://netbox.example.com/dcim/devices/7/".to_string(),
            score: 100,
        },
        SearchResult {
            kind: ObjectKind::Prefix,
            id: 42,
            display: "10.44.208.0/24".to_string(),
            subtitle: Some("den1".to_string()),
            url: "https://netbox.example.com/ipam/prefixes/42/".to_string(),
            score: 50,
        },
    ];

    assert_golden(&results, include_str!("golden/search.json"));
}

#[test]
fn device_detail_json_contract() {
    let mut custom_fields = BTreeMap::<String, Value>::new();
    custom_fields.insert("monitored".to_string(), json!(true));
    custom_fields.insert("ticket".to_string(), json!("INC-7"));

    let detail = DeviceDetail {
        summary: DeviceView {
            id: 7,
            name: "edge01".to_string(),
            status: Some("active".to_string()),
            role: Some("leaf".to_string()),
            site: Some("den1".to_string()),
            rack: Some("rack-a1".to_string()),
            platform: Some("junos".to_string()),
            tenant: Some("infra".to_string()),
            primary_ip4: Some("10.44.208.55/24".to_string()),
            primary_ip6: None,
            serial: Some("JN123".to_string()),
            description: Some("edge leaf".to_string()),
            tags: vec!["edge".to_string(), "prod".to_string()],
            custom_fields,
        },
        interfaces: vec![IfaceRow {
            name: "xe-0/0/0".to_string(),
            enabled: Some(true),
            type_: Some("SFP+".to_string()),
            description: Some("uplink".to_string()),
        }],
        ip_addresses: vec![IpRow {
            address: "10.44.208.55/24".to_string(),
            interface: Some("xe-0/0/0".to_string()),
        }],
        cables: vec![CableRow {
            interface: "xe-0/0/0".to_string(),
            cable: Some("CABLE-1".to_string()),
            connected_to: vec!["core01 xe-0/0/1".to_string()],
        }],
        vlans: vec![VlanRow {
            id: 208,
            vlan: "208 (v-prod)".to_string(),
        }],
        services: vec![ServiceRow {
            name: "ssh".to_string(),
            protocol: Some("tcp".to_string()),
            ports: vec![22],
        }],
    };

    assert_golden(&detail, include_str!("golden/device_detail.json"));
}
