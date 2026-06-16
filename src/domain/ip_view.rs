//! Flattened IP-address view for `nbx ip` (plain + JSON).
//!
//! Resolves the most-specific containing prefix locally with `ipnet`, and pulls
//! VLAN/site context from that prefix.

use ipnet::IpNet;
use serde::Serialize;

use crate::netbox::models::ipam::{IpAddress, Prefix};
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
}

impl IpView {
    /// Build a view from an IP and its (optional) most-specific parent prefix.
    pub fn build(ip: IpAddress, parent: Option<Prefix>) -> Self {
        let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };

        let (parent_prefix, vlan, site) = match parent {
            Some(p) => {
                let site = p
                    .scope
                    .as_ref()
                    .filter(|_| p.scope_type.as_deref() == Some("dcim.site"))
                    .map(|b| b.label());
                (Some(p.prefix), p.vlan.map(|b| b.label()), site)
            }
            None => (None, None, None),
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
            site,
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
            .push_opt("site", self.site.clone());
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
            "dns_name": "printer-55.example.com"
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
        assert_eq!(view.site.as_deref(), Some("iad1"));
        assert_eq!(view.vlan.as_deref(), Some("208 (users)"));
        assert_eq!(view.dns_name.as_deref(), Some("printer-55.example.com"));
    }
}
