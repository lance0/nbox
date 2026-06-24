//! The MAC-address view: a flat `MacView` for `nbox mac <addr>` (CLI + MCP).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::netbox::models::dcim::MacAddress;
use crate::output::plain::KeyValues;

/// A MAC address plus its assignment. The reverse-resolve answer for
/// `nbox mac <addr>` — which interface(s)/device(s) carry this MAC.
#[derive(Debug, Clone, Serialize)]
pub struct MacView {
    pub id: u64,
    /// The MAC in NetBox's canonical form.
    pub mac_address: String,
    /// The dotted content type of the assigned object (`dcim.interface`,
    /// `virtualization.vminterface`, …), or `None` for an unassigned MAC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_object_type: Option<String>,
    /// A readable label for the assigned interface/VM-interface —
    /// `"<device> <interface>"` (e.g. `edge01 xe-0/0/1`) or just the interface
    /// name. `None` when the MAC is unassigned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned: Option<String>,
    /// The owning device's name (when assigned to a physical interface), for a
    /// quick "which device" without re-parsing `assigned`. `None` for VM
    /// interfaces and unassigned MACs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl MacView {
    /// Normalize a wire [`MacAddress`] into a flat view.
    #[must_use]
    pub fn from_model(m: MacAddress) -> Self {
        let assigned = m.assigned_object.as_ref().and_then(assigned_label);
        // Pull the owning device name straight from the assigned brief — it's
        // already there for a physical interface, so no second fetch is needed.
        let device = m
            .assigned_object
            .as_ref()
            .and_then(|v| v.get("device"))
            .and_then(|d| {
                d.get("display")
                    .or_else(|| d.get("name"))
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            });
        Self {
            id: m.id,
            mac_address: m.mac_address,
            assigned_object_type: m.assigned_object_type,
            assigned,
            device,
            description: m.description.and_then(crate::domain::util::non_empty),
            owner: m.owner.map(|b| b.label()),
            tags: m.tags.into_iter().map(|t| t.slug).collect(),
            custom_fields: crate::domain::custom::fields(&m.custom_fields),
        }
    }

    /// Plain-text rendering (`-o plain`): the MAC + its assignment, one row each.
    #[must_use]
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("mac", self.mac_address.clone());
        kv.push_opt("assigned_object_type", self.assigned_object_type.clone());
        kv.push_opt("assigned", self.assigned.clone());
        kv.push_opt("device", self.device.clone());
        kv.push_opt("description", self.description.clone());
        kv.push_opt("owner", self.owner.clone());
        if !self.tags.is_empty() {
            kv.push("tags", self.tags.join(", "));
        }
        kv
    }
}

/// Label a MAC's `assigned_object` brief, tolerating its polymorphic shape. NetBox
/// returns either a `dcim.interface` (carries a `device`) or a
/// `virtualization.vminterface` (carries a `virtual_machine`); both expose a
/// `display`/`name` and the parent's `display`/`name`. Mirrors `ip_view`'s
/// `assigned_label` but handles the VM-interface parent too.
///
/// Returns `"<parent> <interface>"` when the parent is present, else the bare
/// interface name — so `edge01 xe-0/0/1` for a device interface and
/// `web-01 eth0` for a VM interface.
pub(crate) fn assigned_label(v: &serde_json::Value) -> Option<String> {
    let iface = v
        .get("display")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("name").and_then(|x| x.as_str()))?;
    // A physical interface carries `device`; a VM interface carries
    // `virtual_machine`. Either way the brief nests a `display`/`name`.
    let parent = v
        .get("device")
        .or_else(|| v.get("virtual_machine"))
        .and_then(|p| {
            p.get("display")
                .or_else(|| p.get("name"))
                .and_then(|x| x.as_str())
        });
    Some(match parent {
        Some(name) => format!("{name} {iface}"),
        None => iface.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mac(id: u64, addr: &str, assigned: Option<serde_json::Value>) -> MacAddress {
        serde_json::from_value(json!({
            "id": id,
            "url": format!("http://nb/api/dcim/mac-addresses/{id}/"),
            "mac_address": addr,
            "assigned_object": assigned,
        }))
        .unwrap()
    }

    #[test]
    fn from_model_labels_a_device_interface_assignment() {
        let m = mac(
            1,
            "aa:bb:cc:dd:ee:ff",
            Some(json!({
                "display": "xe-0/0/1",
                "device": {"display": "edge01"}
            })),
        );
        let v = MacView::from_model(m);
        assert_eq!(v.mac_address, "aa:bb:cc:dd:ee:ff");
        assert_eq!(v.assigned.as_deref(), Some("edge01 xe-0/0/1"));
        assert_eq!(v.device.as_deref(), Some("edge01"));
    }

    #[test]
    fn from_model_labels_a_vm_interface_assignment() {
        let m = mac(
            2,
            "11:22:33:44:55:66",
            Some(json!({
                "name": "eth0",
                "virtual_machine": {"display": "web-01"}
            })),
        );
        let v = MacView::from_model(m);
        assert_eq!(v.assigned.as_deref(), Some("web-01 eth0"));
        // A VM interface has no `device` — `device` stays None.
        assert_eq!(v.device, None);
    }

    #[test]
    fn from_model_handles_an_unassigned_mac() {
        let m = mac(3, "aa:bb:cc:dd:ee:00", None);
        let v = MacView::from_model(m);
        assert_eq!(v.assigned, None);
        assert_eq!(v.device, None);
    }

    #[test]
    fn assigned_label_falls_back_to_the_bare_interface_name() {
        let bare = json!({"name": "eth0"});
        assert_eq!(assigned_label(&bare).as_deref(), Some("eth0"));
    }

    #[test]
    fn to_key_values_renders_mac_and_assignment() {
        let m = mac(
            1,
            "aa:bb:cc:dd:ee:ff",
            Some(json!({"display": "xe-0/0/1", "device": {"display": "edge01"}})),
        );
        let rendered = MacView::from_model(m).to_key_values().render();
        assert!(rendered.contains("mac: aa:bb:cc:dd:ee:ff"), "{rendered}");
        assert!(rendered.contains("assigned: edge01 xe-0/0/1"), "{rendered}");
        assert!(rendered.contains("device: edge01"), "{rendered}");
    }
}
