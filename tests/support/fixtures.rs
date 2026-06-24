use std::collections::BTreeMap;

use nbox::domain::device_detail::{CableRow, DeviceDetail, IfaceRow, IpRow, ServiceRow, VlanRow};
use nbox::domain::device_view::DeviceView;
use nbox::domain::interface_view::InterfaceView;
use nbox::domain::ip_view::IpView;
use nbox::domain::journal_view::{JournalEntryRow, JournalView};
use nbox::domain::prefix_view::{PrefixIp, PrefixView};
use nbox::domain::site_view::SiteView;
use nbox::domain::vlan_view::VlanView;
use nbox::domain::vrf_view::{RouteTargetRef, VrfAddressRow, VrfDetail, VrfPrefixRow, VrfView};
use nbox::netbox::search::{ObjectKind, SearchResult};
use serde_json::{Value, json};

/// A VRF routing-context detail with a small prefix tree (container + child),
/// one scoped address, and a capped total, exercising every serialized field
/// (including the skip-if-empty ones). This is the exact shape `nbox vrf --json`
/// and MCP `nbox_get vrf` emit.
pub fn vrf_detail() -> VrfDetail {
    VrfDetail {
        summary: VrfView {
            id: 12,
            name: "customer-prod".into(),
            rd: Some("65000:100".into()),
            tenant: Some("Acme".into()),
            enforce_unique: Some(true),
            import_targets: vec![RouteTargetRef {
                id: 5,
                name: "65000:100".into(),
            }],
            export_targets: vec![RouteTargetRef {
                id: 5,
                name: "65000:100".into(),
            }],
            prefix_count: Some(3),
            ipaddress_count: Some(1),
            description: Some("Customer production VRF".into()),
            tags: vec!["prod".into()],
            custom_fields: BTreeMap::new(),
        },
        prefixes: vec![
            VrfPrefixRow {
                id: 1,
                prefix: "10.50.0.0/16".into(),
                depth: 0,
                status: Some("container".into()),
                description: "supernet".into(),
                utilization: Some(42),
            },
            VrfPrefixRow {
                id: 2,
                prefix: "10.50.1.0/24".into(),
                depth: 1,
                status: Some("active".into()),
                description: String::new(),
                utilization: None,
            },
        ],
        addresses: vec![VrfAddressRow {
            id: 9,
            address: "10.50.1.1/24".into(),
            status: Some("active".into()),
            dns_name: Some("gw.customer".into()),
        }],
        // prefix_total exceeds prefixes.len() to model a capped section.
        prefix_total: 3,
        address_total: 1,
    }
}

/// An IP address with full parent-prefix context, a tag, and a custom field —
/// the exact shape `nbox ip --json` and MCP `nbox_get ip` emit. Every
/// skip-if-empty field is populated so the contract pins their presence.
pub fn ip_view() -> IpView {
    IpView {
        address: "10.44.208.55/24".into(),
        status: Some("active".into()),
        dns_name: Some("printer-55.example.com".into()),
        vrf: Some("customer-prod".into()),
        tenant: Some("Acme".into()),
        assigned: Some("edge01 xe-0/0/0".into()),
        parent_prefix: Some("10.44.208.0/24".into()),
        vlan: Some("208 (v-prod)".into()),
        scope: Some("den1".into()),
        scope_type: Some("site".into()),
        tags: vec!["printer".into()],
        custom_fields: BTreeMap::from([("owner".to_string(), json!("netops"))]),
    }
}

/// A prefix with scope/VLAN context, a child prefix, and contained addresses
/// (one assigned, one free) — the shape `nbox prefix --json` and MCP
/// `nbox_get prefix` emit. Exercises the optional scalars plus the two lists.
pub fn prefix_view() -> PrefixView {
    PrefixView {
        prefix: "10.44.208.0/24".into(),
        status: Some("active".into()),
        vrf: Some("customer-prod".into()),
        vlan: Some("208 (v-prod)".into()),
        scope: Some("den1".into()),
        scope_type: Some("site".into()),
        tenant: Some("Acme".into()),
        role: Some("access".into()),
        children: Some(2),
        utilization: Some(37.5),
        description: Some("user access prefix".into()),
        tags: vec!["prod".into()],
        custom_fields: BTreeMap::from([("vlan_owner".to_string(), json!("netops"))]),
        child_prefixes: vec!["10.44.208.0/26".into(), "10.44.208.64/26".into()],
        ip_addresses: vec![
            PrefixIp {
                address: "10.44.208.1/24".into(),
                assigned: Some("edge01 irb.208".into()),
            },
            PrefixIp {
                address: "10.44.208.55/24".into(),
                assigned: None,
            },
        ],
    }
}

/// A VLAN scoped to a site, belonging to a group that is itself region-scoped,
/// with two referencing prefixes — the shape `nbox vlan --json` and MCP
/// `nbox_get vlan` emit. Populates the group-scope and direct-scope fields plus
/// the skip-if-empty scalars so the contract pins every serialized field.
pub fn vlan_view() -> VlanView {
    VlanView {
        vid: 208,
        name: "v-prod".into(),
        status: Some("active".into()),
        group: Some("den1-campus".into()),
        scope: Some("den1".into()),
        scope_type: Some("site".into()),
        group_scope: Some("us-west".into()),
        group_scope_type: Some("region".into()),
        tenant: Some("Acme".into()),
        role: Some("access".into()),
        description: Some("production user VLAN".into()),
        tags: vec!["prod".into()],
        custom_fields: BTreeMap::from([("vlan_owner".to_string(), json!("netops"))]),
        prefixes: vec!["10.44.208.0/24".into(), "10.45.208.0/24".into()],
    }
}

/// A site with region/group/tenant context, a facility, a tag, and a custom
/// field — the shape `nbox site --json` and MCP `nbox_get site` emit. Every
/// skip-if-empty field is populated so the contract pins their presence.
pub fn site_view() -> SiteView {
    SiteView {
        id: 3,
        name: "den1".into(),
        slug: "den1".into(),
        status: Some("active".into()),
        region: Some("us-west".into()),
        group: Some("colos".into()),
        tenant: Some("Acme".into()),
        facility: Some("DEN-1 / Suite 400".into()),
        description: Some("Denver edge site".into()),
        tags: vec!["edge".into(), "prod".into()],
        custom_fields: BTreeMap::from([("region_lead".to_string(), json!("netops"))]),
    }
}

/// A tagged-mode interface with an untagged + two tagged VLANs, a cable with its
/// far end, a rendered cable-trace path, two assigned IPs, a tag, and a custom
/// field — the shape `nbox interface --json` and MCP `nbox_get interface` emit.
/// Every skip-if-empty field (scalars and lists) is populated to pin the contract.
pub fn interface_view() -> InterfaceView {
    InterfaceView {
        device: Some("edge01".into()),
        name: "xe-0/0/0".into(),
        enabled: Some(true),
        type_: Some("SFP+ (10GE)".into()),
        mtu: Some(9000),
        mac_address: Some("00:1b:44:11:3a:b7".into()),
        mode: Some("Tagged".into()),
        untagged_vlan: Some("10 (mgmt)".into()),
        tagged_vlans: vec!["208 (v-prod)".into(), "209 (v-dev)".into()],
        cable: Some("#3".into()),
        connected_to: vec!["core01 xe-1/0/0".into()],
        description: Some("uplink to core01".into()),
        ip_addresses: vec!["10.44.208.1/24".into(), "2001:db8::1/64".into()],
        trace: vec!["edge01 xe-0/0/0 --[Cable #3]-- core01 xe-1/0/0".into()],
        diagram: vec![
            " A  edge01".into(),
            "    xe-0/0/0".into(),
            "    │".into(),
            "    ┿ #3".into(),
            "    │".into(),
            " Z  core01".into(),
            "    xe-1/0/0".into(),
        ],
        tags: vec!["uplink".into()],
        custom_fields: BTreeMap::from([("link_owner".to_string(), json!("netops"))]),
    }
}

/// A journal with two entries (one with author/kind/date, one comments-only) —
/// the shape `nbox journal --json` and MCP `nbox_journal` emit (a top-level
/// `{ "entries": [...] }`). Exercises the per-row skip-if-empty optionals.
pub fn journal_view() -> JournalView {
    JournalView {
        entries: vec![
            JournalEntryRow {
                created: Some("2026-01-02T15:04:05Z".into()),
                kind: Some("info".into()),
                author: Some("admin".into()),
                comments: "Replaced uplink optic; link is stable.".into(),
            },
            JournalEntryRow {
                created: None,
                kind: None,
                author: None,
                comments: "Migrated to new rack.".into(),
            },
        ],
    }
}

pub fn status_report() -> Value {
    json!({
        "netbox_url": "https://netbox.example.com/",
        "api": {
            "search": {
                "configured": "graphql",
                "effective": "rest",
                "reason": "NetBox GraphQL exposes no REST-equivalent full-text (q) search"
            },
            "vrf": { "configured": "graphql", "effective": "graphql" },
            "route_target": { "configured": "graphql", "effective": "graphql" }
        },
        "netbox_version": "4.5.5",
        "django_version": "5.2.1",
        "python_version": "3.12.3",
        "token": {
            "status": "valid",
            "username": "admin",
            "display": "admin"
        },
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
                    "vrf": { "supported": true, "recommended": true, "missing": [] },
                    "route_target": { "supported": true, "recommended": true, "missing": [] }
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
                id: 5001,
                name: "xe-0/0/0".to_string(),
                enabled: Some(true),
                type_: Some("SFP+".to_string()),
                description: Some("uplink".to_string()),
            }],
            ip_addresses: vec![IpRow {
                id: 9001,
                address: "10.44.208.55/24".to_string(),
                interface: Some("xe-0/0/0".to_string()),
            }],
            cables: vec![CableRow {
                id: 5001,
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
