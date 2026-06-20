use std::collections::BTreeMap;

use nbox::domain::device_detail::{CableRow, DeviceDetail, IfaceRow, IpRow, ServiceRow, VlanRow};
use nbox::domain::device_view::DeviceView;
use nbox::netbox::search::{ObjectKind, SearchResult};
use serde_json::{Value, json};

pub fn status_report() -> Value {
    json!({
        "netbox_url": "https://netbox.example.com/",
        "api": {
            "search": {
                "configured": "graphql",
                "effective": "rest",
                "reason": "NetBox GraphQL exposes no REST-equivalent full-text (q) search"
            },
            "vrf": { "configured": "graphql", "effective": "graphql" }
        },
        "netbox_version": "4.5.5",
        "django_version": "5.2.1",
        "python_version": "3.12.3",
        "capabilities": {
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
                "probed": true,
                "available": true,
                "surfaces": {
                    "search": {
                        "supported": false,
                        "recommended": false,
                        "missing": ["NetBox GraphQL exposes no REST-equivalent full-text (q) search"]
                    },
                    "vrf": { "supported": true, "recommended": true, "missing": [] }
                }
            }
        }
    })
}

pub fn search_results() -> Vec<SearchResult> {
    vec![
        search_result(ObjectKind::Device, 7, "edge01")
            .subtitle("den1")
            .url("https://netbox.example.com/dcim/devices/7/")
            .score(100)
            .build(),
        search_result(ObjectKind::Prefix, 42, "10.44.208.0/24")
            .subtitle("den1")
            .url("https://netbox.example.com/ipam/prefixes/42/")
            .score(50)
            .build(),
    ]
}

pub fn search_result(kind: ObjectKind, id: u64, display: impl Into<String>) -> SearchResultBuilder {
    SearchResultBuilder {
        kind,
        id,
        display: display.into(),
        subtitle: None,
        url: format!("https://netbox.example.com/objects/{id}/"),
        score: 10,
    }
}

pub struct SearchResultBuilder {
    kind: ObjectKind,
    id: u64,
    display: String,
    subtitle: Option<String>,
    url: String,
    score: i32,
}

impl SearchResultBuilder {
    pub fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    pub const fn score(mut self, score: i32) -> Self {
        self.score = score;
        self
    }

    pub fn build(self) -> SearchResult {
        SearchResult {
            kind: self.kind,
            id: self.id,
            display: self.display,
            subtitle: self.subtitle,
            url: self.url,
            score: self.score,
        }
    }
}

pub fn device_detail() -> DeviceDetailBuilder {
    DeviceDetailBuilder {
        detail: DeviceDetail {
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
                custom_fields: BTreeMap::from([
                    ("monitored".to_string(), json!(true)),
                    ("ticket".to_string(), json!("INC-7")),
                ]),
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
        },
    }
}

pub struct DeviceDetailBuilder {
    detail: DeviceDetail,
}

impl DeviceDetailBuilder {
    pub fn custom_field(mut self, key: impl Into<String>, value: Value) -> Self {
        self.detail.summary.custom_fields.insert(key.into(), value);
        self
    }

    pub fn build(self) -> DeviceDetail {
        self.detail
    }
}
