//! Flattened prefix view for `nbox prefix` (plain + JSON), with child prefixes
//! and contained IP addresses.

use serde::Serialize;

use crate::domain::ip_view::assigned_label;
use crate::netbox::models::ipam::{IpAddress, Prefix};
use crate::output::plain::KeyValues;

/// An IP address listed under a prefix.
#[derive(Debug, Clone, Serialize)]
pub struct PrefixIp {
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned: Option<String>,
}

/// A prefix with resolved children and contained addresses.
#[derive(Debug, Clone, Serialize)]
pub struct PrefixView {
    pub prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vrf: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vlan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utilization: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub child_prefixes: Vec<String>,
    pub ip_addresses: Vec<PrefixIp>,
}

impl PrefixView {
    /// Build a view from a prefix plus its children and contained IPs.
    pub fn build(p: Prefix, children: Vec<Prefix>, ips: Vec<IpAddress>) -> Self {
        let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };
        let scope = p
            .scope
            .as_ref()
            .filter(|_| p.scope_type.as_deref() == Some("dcim.site"))
            .map(|b| b.label());

        Self {
            prefix: p.prefix,
            status: p.status.map(|c| c.value),
            vrf: p.vrf.map(|b| b.label()),
            vlan: p.vlan.map(|b| b.label()),
            scope,
            tenant: p.tenant.map(|b| b.label()),
            role: p.role.map(|b| b.label()),
            children: p.children,
            utilization: p.utilization.as_ref().and_then(coerce_pct),
            description: p.description.and_then(non_empty),
            child_prefixes: children.into_iter().map(|c| c.prefix).collect(),
            ip_addresses: ips
                .into_iter()
                .map(|ip| PrefixIp {
                    assigned: ip.assigned_object.as_ref().and_then(assigned_label),
                    address: ip.address,
                })
                .collect(),
        }
    }

    /// Render header fields plus child-prefix and IP sections for plain output.
    pub fn to_plain(&self) -> String {
        let mut kv = KeyValues::new();
        kv.push("prefix", self.prefix.clone())
            .push_opt("status", self.status.clone())
            .push_opt("vrf", self.vrf.clone())
            .push_opt("vlan", self.vlan.clone())
            .push_opt("scope", self.scope.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("role", self.role.clone())
            .push_opt("children", self.children.map(|c| c.to_string()))
            .push_opt("utilization", self.utilization.map(format_utilization))
            .push_opt("description", self.description.clone());
        let mut out = kv.render();

        if !self.child_prefixes.is_empty() {
            out.push_str("\n\nChild Prefixes\n");
            let lines: Vec<String> = self
                .child_prefixes
                .iter()
                .map(|p| format!("  {p}"))
                .collect();
            out.push_str(&lines.join("\n"));
        }

        if !self.ip_addresses.is_empty() {
            out.push_str("\n\nIP Addresses\n");
            let lines: Vec<String> = self
                .ip_addresses
                .iter()
                .map(|ip| match &ip.assigned {
                    Some(a) => format!("  {}  {}", ip.address, a),
                    None => format!("  {}", ip.address),
                })
                .collect();
            out.push_str(&lines.join("\n"));
        }

        out
    }
}

/// Coerce a permissive utilization value (number, or string like "75%") to a percent.
fn coerce_pct(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| {
        v.as_str()
            .and_then(|s| s.trim().trim_end_matches('%').trim().parse().ok())
    })
}

/// Format a percent with a small ASCII bar, e.g. `52% [########--------]`.
fn format_utilization(pct: f64) -> String {
    format!("{pct:.0}% {}", pct_bar(pct, 16))
}

fn pct_bar(pct: f64, width: usize) -> String {
    let filled = ((pct.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let mut bar = String::with_capacity(width + 2);
    bar.push('[');
    for i in 0..width {
        bar.push(if i < filled { '#' } else { '-' });
    }
    bar.push(']');
    bar
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn utilization_is_coerced_and_rendered_when_numeric() {
        let p: Prefix = serde_json::from_value(json!({
            "id": 5, "url": "u", "prefix": "10.44.208.0/24", "utilization": 75.0
        }))
        .unwrap();
        let view = PrefixView::build(p, vec![], vec![]);
        assert_eq!(view.utilization, Some(75.0));
        let plain = view.to_plain();
        assert!(plain.contains("utilization: 75% ["), "got: {plain}");
        assert!(plain.contains('#') && plain.contains('-'));
    }

    #[test]
    fn utilization_absent_renders_no_line() {
        let p: Prefix =
            serde_json::from_value(json!({"id": 5, "url": "u", "prefix": "10.0.0.0/8"})).unwrap();
        let view = PrefixView::build(p, vec![], vec![]);
        assert_eq!(view.utilization, None);
        assert!(!view.to_plain().contains("utilization:"));
    }

    #[test]
    fn coerce_pct_handles_number_and_percent_string() {
        assert_eq!(coerce_pct(&json!(42.5)), Some(42.5));
        assert_eq!(coerce_pct(&json!("50%")), Some(50.0));
        assert_eq!(coerce_pct(&json!("nope")), None);
    }

    #[test]
    fn build_flattens_and_collects_sections() {
        let p: Prefix = serde_json::from_value(json!({
            "id": 5, "url": "http://nb/p/5/", "prefix": "10.44.208.0/24",
            "status": {"value": "active", "label": "Active"},
            "scope_type": "dcim.site",
            "scope": {"id": 1, "display": "iad1"},
            "vlan": {"id": 2, "display": "208 (users)"},
            "children": 4
        }))
        .unwrap();
        let children: Vec<Prefix> = vec![
            serde_json::from_value(json!({"id": 6, "url": "u", "prefix": "10.44.208.0/26"}))
                .unwrap(),
        ];
        let ips: Vec<IpAddress> = vec![
            serde_json::from_value(json!({
                "id": 7, "url": "u", "address": "10.44.208.1/24",
                "assigned_object": {"display": "irb.208", "device": {"display": "edge01"}}
            }))
            .unwrap(),
        ];

        let view = PrefixView::build(p, children, ips);
        assert_eq!(view.scope.as_deref(), Some("iad1"));
        assert_eq!(view.vlan.as_deref(), Some("208 (users)"));
        assert_eq!(view.children, Some(4));
        assert_eq!(view.child_prefixes, vec!["10.44.208.0/26"]);
        assert_eq!(
            view.ip_addresses[0].assigned.as_deref(),
            Some("edge01 irb.208")
        );

        let plain = view.to_plain();
        assert!(plain.contains("prefix: 10.44.208.0/24"));
        assert!(plain.contains("Child Prefixes\n  10.44.208.0/26"));
        assert!(plain.contains("IP Addresses\n  10.44.208.1/24  edge01 irb.208"));
    }
}
