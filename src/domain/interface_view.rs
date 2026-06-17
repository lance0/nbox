//! Flattened interface view for `nbox interface` (plain + JSON).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::netbox::models::dcim::Interface;
use crate::netbox::models::ipam::IpAddress;
use crate::output::plain::KeyValues;

/// An interface, normalized to flat fields plus its assigned addresses.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InterfaceView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub untagged_vlan: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tagged_vlans: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cable: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub connected_to: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ip_addresses: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl InterfaceView {
    /// Build a view from a wire [`Interface`], the IPs assigned to it, and the
    /// raw cable-trace hops (`…/trace/`), rendered into a readable path.
    pub fn build(i: Interface, ips: Vec<IpAddress>, trace: Vec<Value>) -> Self {
        let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };
        Self {
            device: i.device.map(|b| b.label()),
            name: i.name,
            enabled: i.enabled,
            type_: i.type_.map(|c| c.label),
            mtu: i.mtu,
            mac_address: i.mac_address.and_then(non_empty),
            mode: i.mode.map(|c| c.label),
            untagged_vlan: i.untagged_vlan.map(|b| b.label()),
            tagged_vlans: i.tagged_vlans.into_iter().map(|b| b.label()).collect(),
            cable: i.cable.map(|b| b.label()),
            connected_to: i
                .connected_endpoints
                .unwrap_or_default()
                .into_iter()
                .map(|b| b.label())
                .collect(),
            description: i.description.and_then(non_empty),
            ip_addresses: ips.into_iter().map(|ip| ip.address).collect(),
            trace: format_trace(&trace),
            custom_fields: custom::fields(&i.custom_fields),
        }
    }

    /// Render header fields plus tagged-VLAN / connection / IP sections.
    pub fn to_plain(&self) -> String {
        let mut kv = KeyValues::new();
        kv.push_opt("device", self.device.clone())
            .push("name", self.name.clone())
            .push_opt("enabled", self.enabled.map(|b| b.to_string()))
            .push_opt("type", self.type_.clone())
            .push_opt("mtu", self.mtu.map(|m| m.to_string()))
            .push_opt("mac", self.mac_address.clone())
            .push_opt("mode", self.mode.clone())
            .push_opt("untagged_vlan", self.untagged_vlan.clone())
            .push_opt("cable", self.cable.clone())
            .push_opt("description", self.description.clone());
        custom::append(&mut kv, &self.custom_fields);
        let mut out = kv.render();

        out.push_str(&section("Tagged VLANs", &self.tagged_vlans));
        out.push_str(&section("Connected To", &self.connected_to));
        out.push_str(&section("Cable Path", &self.trace));
        out.push_str(&section("IP Addresses", &self.ip_addresses));
        out
    }
}

/// A `\n\nTitle\n  item` block, or empty when there are no items.
fn section(title: &str, items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = items.iter().map(|i| format!("  {i}")).collect();
    format!("\n\n{title}\n{}", lines.join("\n"))
}

/// Render trace hops (`[near terminations, cable, far terminations]`) into
/// readable `near --[cable]-- far` lines, tolerating the polymorphic JSON.
fn format_trace(hops: &[Value]) -> Vec<String> {
    hops.iter()
        .filter_map(|hop| {
            let arr = hop.as_array()?;
            let near = termination_labels(arr.first());
            let far = termination_labels(arr.get(2));
            let mid = match arr.get(1).and_then(cable_label) {
                Some(c) => format!(" --[{c}]-- "),
                None => " -- ".to_string(),
            };
            let line = format!("{near}{mid}{far}");
            (line.trim() != "--").then_some(line)
        })
        .collect()
}

/// Join the `display` labels of a terminations array (or a single object).
fn termination_labels(v: Option<&Value>) -> String {
    match v {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(label_of)
            .collect::<Vec<_>>()
            .join(", "),
        Some(other) => label_of(other).unwrap_or_default(),
        None => String::new(),
    }
}

/// A `device name` (or just `name`) label from a termination object.
fn label_of(v: &Value) -> Option<String> {
    let name = v
        .get("display")
        .or_else(|| v.get("name"))
        .and_then(|x| x.as_str())?;
    let device = v.get("device").and_then(|d| {
        d.get("display")
            .or_else(|| d.get("name"))
            .and_then(|x| x.as_str())
    });
    Some(match device {
        Some(dev) => format!("{dev} {name}"),
        None => name.to_string(),
    })
}

/// The display/label of a cable object, if present.
fn cable_label(v: &Value) -> Option<String> {
    v.get("display")
        .or_else(|| v.get("label"))
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_interface_with_vlans_and_ips() {
        let iface: Interface = serde_json::from_value(json!({
            "id": 42, "url": "u", "name": "xe-0/0/1",
            "device": {"id": 1, "display": "edge01"},
            "enabled": true,
            "type": {"value": "10gbase-x-sfpp", "label": "SFP+ (10GE)"},
            "mtu": 9000,
            "mode": {"value": "tagged", "label": "Tagged"},
            "untagged_vlan": {"id": 5, "display": "10 (mgmt)"},
            "tagged_vlans": [{"id": 6, "display": "20 (prod)"}, {"id": 7, "display": "30 (dev)"}],
            "cable": {"id": 3, "display": "#3"},
            "connected_endpoints": [{"id": 99, "display": "core01 xe-1/0/0"}],
            "custom_fields": {}
        }))
        .unwrap();
        let ips: Vec<IpAddress> = vec![
            serde_json::from_value(json!({"id": 8, "url": "u", "address": "10.0.0.1/31"})).unwrap(),
        ];

        let view = InterfaceView::build(iface, ips, vec![]);
        assert!(view.trace.is_empty());
        assert_eq!(view.device.as_deref(), Some("edge01"));
        assert_eq!(view.type_.as_deref(), Some("SFP+ (10GE)"));
        assert_eq!(view.mode.as_deref(), Some("Tagged"));
        assert_eq!(view.untagged_vlan.as_deref(), Some("10 (mgmt)"));
        assert_eq!(view.tagged_vlans, vec!["20 (prod)", "30 (dev)"]);
        assert_eq!(view.connected_to, vec!["core01 xe-1/0/0"]);

        let plain = view.to_plain();
        assert!(plain.contains("name: xe-0/0/1"));
        assert!(plain.contains("Tagged VLANs\n  20 (prod)\n  30 (dev)"));
        assert!(plain.contains("Connected To\n  core01 xe-1/0/0"));
        assert!(plain.contains("IP Addresses\n  10.0.0.1/31"));
    }

    #[test]
    fn renders_cable_trace_path() {
        let iface: Interface =
            serde_json::from_value(json!({"id": 1, "url": "u", "name": "xe-0/0/0"})).unwrap();
        // One hop: near interface --[cable]-- far interface (each side an array).
        let trace = vec![json!([
            [{"display": "xe-0/0/0", "device": {"display": "edge01"}}],
            {"display": "Cable #3"},
            [{"display": "xe-1/0/0", "device": {"display": "core01"}}]
        ])];

        let view = InterfaceView::build(iface, vec![], trace);
        assert_eq!(
            view.trace,
            vec!["edge01 xe-0/0/0 --[Cable #3]-- core01 xe-1/0/0"]
        );
        assert!(
            view.to_plain()
                .contains("Cable Path\n  edge01 xe-0/0/0 --[Cable #3]-- core01 xe-1/0/0")
        );
    }
}
