//! Flattened IP-address view for `nbox ip` (plain + JSON).
//!
//! Resolves the most-specific containing prefix locally with `ipnet`, and pulls
//! VLAN/scope context from that prefix.

use std::collections::BTreeMap;

use ipnet::IpNet;
use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::non_empty;
use crate::netbox::models::common::BriefObject;
use crate::netbox::models::ipam::{IpAddress, Prefix};
use crate::netbox::query::friendly_scope_type;
use crate::output::plain::KeyValues;

/// An IP address with resolved parent-prefix context.
#[derive(Debug, Clone, Serialize)]
pub struct IpView {
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dns_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vrf: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vlan: Option<String>,
    /// Name of the parent prefix's scope object (site, location, region, …) for
    /// any scope type — see [`scope_type`](Self::scope_type) for which kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Friendly scope type of the parent prefix, e.g. `site`/`location`/`region`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_type: Option<String>,
    /// The inside IP this address NATs for, when this is a NAT *outside* (NetBox
    /// 4.6 embeds `nat_inside` on the outside IP). The address (e.g.
    /// `100.64.0.9/30`). `None` when not a NAT outside or NetBox omits it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nat_inside: Option<String>,
    /// The outside IP(s) this inside address is NAT'd to, when this is a NAT
    /// *inside* (NetBox 4.6 embeds `nat_outside` on the inside IP). Addresses as
    /// above. Empty when not a NAT inside.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nat_outside: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl IpView {
    /// Build a view from an IP and its (optional) most-specific parent prefix.
    pub fn build(ip: IpAddress, parent: Option<Prefix>) -> Self {
        let (parent_prefix, vlan, scope, scope_type) = match parent {
            Some(p) => {
                let scope = p.scope.as_ref().map(BriefObject::label);
                let scope_type = p.scope_type.as_deref().map(friendly_scope_type);
                (Some(p.prefix), p.vlan.map(|b| b.label()), scope, scope_type)
            }
            None => (None, None, None, None),
        };

        Self {
            address: ip.address,
            status: ip.status.map(|c| c.value),
            dns_name: ip.dns_name.and_then(non_empty),
            vrf: ip.vrf.map(|b| b.label()),
            tenant: ip.tenant.map(|b| b.label()),
            assigned: ip.assigned_object.as_ref().and_then(assigned_label),
            parent_prefix,
            vlan,
            scope,
            scope_type,
            nat_inside: ip.nat_inside.as_ref().map(BriefObject::label),
            nat_outside: ip.nat_outside.iter().map(BriefObject::label).collect(),
            tags: ip.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&ip.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("address", self.address.clone())
            .push_opt("status", self.status.clone())
            .push_opt("dns", self.dns_name.clone())
            .push_opt("vrf", self.vrf.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("assigned", self.assigned.clone())
            .push_opt("parent_prefix", self.parent_prefix.clone())
            .push_opt("vlan", self.vlan.clone())
            .push_opt("scope", self.scope.clone())
            .push_opt("scope_type", self.scope_type.clone())
            .push_opt("nat_inside", self.nat_inside.clone());
        if !self.nat_outside.is_empty() {
            kv.push("nat_outside", self.nat_outside.join(", "));
        }
        if !self.tags.is_empty() {
            kv.push("tags", self.tags.join(", "));
        }
        custom::append(&mut kv, &self.custom_fields);
        kv
    }
}

/// Pick the most-specific (longest-prefix) entry that parses as a network.
pub fn most_specific(prefixes: Vec<Prefix>) -> Option<Prefix> {
    prefixes
        .into_iter()
        .filter_map(|p| {
            p.prefix
                .parse::<IpNet>()
                .ok()
                .map(|net| (net.prefix_len(), p))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, p)| p)
}

/// Extract a "device interface" (or just "interface") label from an IP's
/// `assigned_object` brief, tolerating its polymorphic shape.
pub(crate) fn assigned_label(v: &serde_json::Value) -> Option<String> {
    let iface = v
        .get("display")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("name").and_then(|x| x.as_str()))?;
    let device = v.get("device").and_then(|d| {
        d.get("display")
            .or_else(|| d.get("name"))
            .and_then(|x| x.as_str())
    });
    Some(match device {
        Some(dev) => format!("{dev} {iface}"),
        None => iface.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prefix(cidr: &str) -> Prefix {
        serde_json::from_value(json!({
            "id": 1, "url": "http://nb/p/", "prefix": cidr
        }))
        .unwrap()
    }

    #[test]
    fn most_specific_picks_longest_prefix() {
        let chosen = most_specific(vec![
            prefix("10.0.0.0/8"),
            prefix("10.44.208.0/24"),
            prefix("10.44.0.0/16"),
        ])
        .unwrap();
        assert_eq!(chosen.prefix, "10.44.208.0/24");
    }

    #[test]
    fn most_specific_handles_empty_and_unparseable() {
        assert!(most_specific(vec![]).is_none());
        assert!(most_specific(vec![prefix("not-a-cidr")]).is_none());
    }

    #[test]
    fn assigned_label_combines_device_and_interface() {
        let v = json!({"display": "xe-0/0/1", "device": {"display": "edge01"}});
        assert_eq!(assigned_label(&v).as_deref(), Some("edge01 xe-0/0/1"));

        let bare = json!({"name": "eth0"});
        assert_eq!(assigned_label(&bare).as_deref(), Some("eth0"));
    }

    #[test]
    fn build_pulls_context_from_parent_prefix() {
        let ip: IpAddress = serde_json::from_value(json!({
            "id": 7, "url": "http://nb/ip/7/", "address": "10.44.208.55/24",
            "status": {"value": "active", "label": "Active"},
            "dns_name": "printer-55.example.com",
            "tags": [{"id": 1, "name": "printer", "slug": "printer"}]
        }))
        .unwrap();
        let parent: Prefix = serde_json::from_value(json!({
            "id": 5, "url": "http://nb/p/5/", "prefix": "10.44.208.0/24",
            "scope_type": "dcim.site",
            "scope": {"id": 1, "display": "iad1"},
            "vlan": {"id": 2, "display": "208 (users)"}
        }))
        .unwrap();

        let view = IpView::build(ip, Some(parent));
        assert_eq!(view.parent_prefix.as_deref(), Some("10.44.208.0/24"));
        assert_eq!(view.scope.as_deref(), Some("iad1"));
        assert_eq!(view.scope_type.as_deref(), Some("site"));
        assert_eq!(view.vlan.as_deref(), Some("208 (users)"));
        assert_eq!(view.dns_name.as_deref(), Some("printer-55.example.com"));
        assert_eq!(view.tags, vec!["printer"]);
        assert!(view.to_key_values().render().contains("tags: printer"));
    }

    #[test]
    fn tags_dropped_when_empty() {
        let ip: IpAddress = serde_json::from_value(json!({
            "id": 7, "url": "http://nb/ip/7/", "address": "10.0.0.5/24"
        }))
        .unwrap();
        let view = IpView::build(ip, None);
        assert!(view.tags.is_empty());
        let value = serde_json::to_value(&view).unwrap();
        assert!(value.get("tags").is_none());
        assert!(!view.to_key_values().render().contains("tags:"));
    }

    #[test]
    fn build_renders_nat_inside_on_an_outside_ip() {
        // NetBox 4.6 embeds `nat_inside` (a brief IP ref) on the *outside* IP.
        // Addresses are RFC-reserved (TEST-NET-3 outside, CGNAT inside) — no live
        // infrastructure data.
        let ip: IpAddress = serde_json::from_value(json!({
            "id": 4101, "url": "http://nb/ip/4101/", "address": "203.0.113.5/32",
            "nat_inside": {"id": 4100, "address": "100.64.0.9/30", "display": "100.64.0.9/30"}
        }))
        .unwrap();
        let view = IpView::build(ip, None);
        assert_eq!(view.nat_inside.as_deref(), Some("100.64.0.9/30"));
        assert!(view.nat_outside.is_empty());

        // JSON: `nat_inside` set, `nat_outside` omitted (empty Vec → skip).
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["nat_inside"], "100.64.0.9/30");
        assert!(
            value.get("nat_outside").is_none(),
            "empty nat_outside must be omitted"
        );

        let kv = view.to_key_values().render();
        assert!(kv.contains("nat_inside: 100.64.0.9/30"), "got: {kv}");
        assert!(!kv.contains("nat_outside:"), "got: {kv}");
    }

    #[test]
    fn build_renders_nat_outside_on_an_inside_ip() {
        // The reciprocal: NetBox 4.6 embeds `nat_outside` (an array of brief IP
        // refs) on the *inside* IP. Addresses are RFC-reserved (CGNAT inside,
        // TEST-NET-3 outside) — no live infrastructure data.
        let ip: IpAddress = serde_json::from_value(json!({
            "id": 4100, "url": "http://nb/ip/4100/", "address": "100.64.0.9/30",
            "nat_outside": [
                {"id": 4101, "address": "203.0.113.5/32", "display": "203.0.113.5/32"},
                {"id": 4102, "address": "203.0.113.71/32", "display": "203.0.113.71/32"}
            ]
        }))
        .unwrap();
        let view = IpView::build(ip, None);
        assert!(view.nat_inside.is_none());
        assert_eq!(view.nat_outside, vec!["203.0.113.5/32", "203.0.113.71/32"]);

        let value = serde_json::to_value(&view).unwrap();
        assert!(
            value.get("nat_inside").is_none(),
            "nat_inside absent on an inside IP"
        );
        assert_eq!(
            value["nat_outside"],
            serde_json::json!(["203.0.113.5/32", "203.0.113.71/32"])
        );

        let kv = view.to_key_values().render();
        assert!(
            kv.contains("nat_outside: 203.0.113.5/32, 203.0.113.71/32"),
            "got: {kv}"
        );
        assert!(!kv.contains("nat_inside:"), "got: {kv}");
    }

    #[test]
    fn build_omits_both_nat_fields_when_absent() {
        // A non-NAT IP carries neither field — both stay omitted (byte-identical
        // to pre-NAT output), so the enrichment can't disturb existing consumers.
        let ip: IpAddress = serde_json::from_value(json!({
            "id": 7, "url": "http://nb/ip/7/", "address": "10.0.0.5/24"
        }))
        .unwrap();
        let view = IpView::build(ip, None);
        assert!(view.nat_inside.is_none());
        assert!(view.nat_outside.is_empty());
        let value = serde_json::to_value(&view).unwrap();
        assert!(value.get("nat_inside").is_none());
        assert!(value.get("nat_outside").is_none());
    }

    #[test]
    fn build_derives_non_site_scope_from_parent() {
        let ip: IpAddress = serde_json::from_value(json!({
            "id": 7, "url": "http://nb/ip/7/", "address": "10.0.0.5/24"
        }))
        .unwrap();
        let parent: Prefix = serde_json::from_value(json!({
            "id": 5, "url": "http://nb/p/5/", "prefix": "10.0.0.0/24",
            "scope_type": "dcim.region",
            "scope": {"id": 1, "display": "us-east"}
        }))
        .unwrap();

        let view = IpView::build(ip, Some(parent));
        assert_eq!(view.scope.as_deref(), Some("us-east"));
        assert_eq!(view.scope_type.as_deref(), Some("region"));

        // The JSON view must expose `scope`/`scope_type` and carry no `site` key.
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["scope"], "us-east");
        assert_eq!(value["scope_type"], "region");
        assert!(value.get("site").is_none(), "site key must be gone");

        let kv = view.to_key_values().render();
        assert!(kv.contains("scope: us-east"), "got: {kv}");
        assert!(kv.contains("scope_type: region"), "got: {kv}");
        assert!(!kv.contains("site:"), "got: {kv}");
    }
}
