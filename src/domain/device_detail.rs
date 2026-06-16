//! Composed device detail for `nbox device` (plain + JSON): the flat summary
//! plus its interfaces, IP addresses, cables, and VLANs.

use std::collections::HashSet;

use serde::Serialize;

use crate::domain::device_view::DeviceView;
use crate::netbox::models::dcim::{Device, Interface};
use crate::netbox::models::ipam::IpAddress;

/// One interface row in the device's Interfaces section.
#[derive(Debug, Clone, Serialize)]
pub struct IfaceRow {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One address row in the device's IP Addresses section.
#[derive(Debug, Clone, Serialize)]
pub struct IpRow {
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
}

/// One cabled interface in the device's Cables section.
#[derive(Debug, Clone, Serialize)]
pub struct CableRow {
    pub interface: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cable: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub connected_to: Vec<String>,
}

/// One VLAN seen on the device's interfaces (untagged or tagged).
#[derive(Debug, Clone, Serialize)]
pub struct VlanRow {
    pub id: u64,
    pub vlan: String,
}

/// A device summary plus its interfaces, IPs, cables, and VLANs.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceDetail {
    #[serde(flatten)]
    pub summary: DeviceView,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<IfaceRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ip_addresses: Vec<IpRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cables: Vec<CableRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub vlans: Vec<VlanRow>,
}

impl DeviceDetail {
    /// Build from a device and its interfaces + assigned IPs. VLANs and cables
    /// are derived from the interfaces (no extra requests).
    pub fn build(device: Device, interfaces: Vec<Interface>, ips: Vec<IpAddress>) -> Self {
        let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };

        let mut vlans: Vec<VlanRow> = Vec::new();
        let mut seen_vlan = HashSet::new();
        let mut cables: Vec<CableRow> = Vec::new();

        for i in &interfaces {
            for v in i.untagged_vlan.iter().chain(i.tagged_vlans.iter()) {
                if seen_vlan.insert(v.id) {
                    vlans.push(VlanRow {
                        id: v.id,
                        vlan: v.label(),
                    });
                }
            }
            if let Some(cable) = &i.cable {
                cables.push(CableRow {
                    interface: i.name.clone(),
                    cable: Some(cable.label()),
                    connected_to: i
                        .connected_endpoints
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|b| b.label())
                        .collect(),
                });
            }
        }

        let iface_rows = interfaces
            .into_iter()
            .map(|i| IfaceRow {
                name: i.name,
                enabled: i.enabled,
                type_: i.type_.map(|c| c.label),
                description: i.description.and_then(non_empty),
            })
            .collect();

        let ip_rows = ips
            .into_iter()
            .map(|ip| IpRow {
                interface: ip.assigned_object.as_ref().and_then(iface_name),
                address: ip.address,
            })
            .collect();

        Self {
            summary: DeviceView::from_model(device),
            interfaces: iface_rows,
            ip_addresses: ip_rows,
            cables,
            vlans,
        }
    }

    /// Render the summary plus each non-empty section for plain output.
    pub fn to_plain(&self) -> String {
        let mut out = self.summary_plain();
        for (_, title, body) in self.sections() {
            out.push_str(&format!("\n\n{title}\n{body}"));
        }
        out
    }

    /// The device summary alone, as `key: value` lines.
    pub fn summary_plain(&self) -> String {
        self.summary.to_key_values().render()
    }

    /// Non-empty sections as `(tab key, title, body)` — used for the TUI tabs.
    pub fn sections(&self) -> Vec<(char, &'static str, String)> {
        let mut tabs = Vec::new();
        if !self.interfaces.is_empty() {
            tabs.push(('i', "Interfaces", self.iface_lines().join("\n")));
        }
        if !self.ip_addresses.is_empty() {
            tabs.push(('p', "IP Addresses", self.ip_lines().join("\n")));
        }
        if !self.cables.is_empty() {
            tabs.push(('c', "Cables", self.cable_lines().join("\n")));
        }
        if !self.vlans.is_empty() {
            tabs.push(('v', "VLANs", self.vlan_lines().join("\n")));
        }
        tabs
    }

    fn iface_lines(&self) -> Vec<String> {
        self.interfaces
            .iter()
            .map(|i| {
                let mut row = format!("  {}", i.name);
                if let Some(t) = &i.type_ {
                    row.push_str(&format!("  {t}"));
                }
                if i.enabled == Some(false) {
                    row.push_str("  (disabled)");
                }
                row
            })
            .collect()
    }

    fn ip_lines(&self) -> Vec<String> {
        self.ip_addresses
            .iter()
            .map(|ip| match &ip.interface {
                Some(name) => format!("  {}  {name}", ip.address),
                None => format!("  {}", ip.address),
            })
            .collect()
    }

    fn cable_lines(&self) -> Vec<String> {
        self.cables
            .iter()
            .map(|c| {
                if c.connected_to.is_empty() {
                    format!("  {}  {}", c.interface, c.cable.as_deref().unwrap_or(""))
                } else {
                    format!("  {} -> {}", c.interface, c.connected_to.join(", "))
                }
            })
            .collect()
    }

    fn vlan_lines(&self) -> Vec<String> {
        self.vlans.iter().map(|v| format!("  {}", v.vlan)).collect()
    }
}

/// The interface name from an IP's `assigned_object` (display, else name).
fn iface_name(v: &serde_json::Value) -> Option<String> {
    v.get("display")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("name").and_then(|x| x.as_str()))
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn device() -> Device {
        serde_json::from_value(json!({
            "id": 1, "url": "u", "name": "edge01",
            "status": {"value": "active", "label": "Active"},
            "custom_fields": {}
        }))
        .unwrap()
    }

    #[test]
    fn composes_sections_and_dedups_vlans() {
        let interfaces: Vec<Interface> = vec![
            serde_json::from_value(json!({
                "id": 1, "url": "u", "name": "xe-0/0/0", "enabled": true,
                "type": {"value": "x", "label": "SFP+"},
                "untagged_vlan": {"id": 10, "display": "10 (mgmt)"},
                "tagged_vlans": [{"id": 20, "display": "20 (prod)"}],
                "cable": {"id": 3, "display": "#3"},
                "connected_endpoints": [{"id": 9, "display": "core01 xe-1/0/0"}]
            }))
            .unwrap(),
            serde_json::from_value(json!({
                "id": 2, "url": "u", "name": "xe-0/0/1", "enabled": false,
                "tagged_vlans": [{"id": 20, "display": "20 (prod)"}]
            }))
            .unwrap(),
        ];
        let ips: Vec<IpAddress> = vec![
            serde_json::from_value(json!({
                "id": 7, "url": "u", "address": "10.0.0.1/31",
                "assigned_object": {"name": "xe-0/0/0"}
            }))
            .unwrap(),
        ];

        let detail = DeviceDetail::build(device(), interfaces, ips);
        assert_eq!(detail.interfaces.len(), 2);
        assert_eq!(detail.cables.len(), 1);
        // VLAN 20 appears on both interfaces but is deduped.
        assert_eq!(detail.vlans.len(), 2);
        assert_eq!(
            detail.ip_addresses[0].interface.as_deref(),
            Some("xe-0/0/0")
        );

        let plain = detail.to_plain();
        assert!(plain.starts_with("name: edge01"));
        assert!(plain.contains("Interfaces\n  xe-0/0/0  SFP+\n  xe-0/0/1  (disabled)"));
        assert!(plain.contains("IP Addresses\n  10.0.0.1/31  xe-0/0/0"));
        assert!(plain.contains("Cables\n  xe-0/0/0 -> core01 xe-1/0/0"));
        assert!(plain.contains("VLANs\n  10 (mgmt)\n  20 (prod)"));
    }
}
