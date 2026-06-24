//! Flattened virtual-circuit view for `nbox virtual-circuit` (plain + JSON).
//!
//! A virtual circuit (NetBox 4.2+) is a logical overlay between two or more
//! device interfaces — unlike a physical circuit it carries no cables, no A/Z
//! sides, and no speeds. Its terminations are a flat, multi-point list, each
//! landing on an interface (and so on the device that carries it). There is no
//! cable-path diagram (nothing to walk); the structured termination refs are
//! the machine-readable form for navigation.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::non_empty;
use crate::netbox::models::circuits::{VirtualCircuit, VirtualCircuitTermination};
use crate::output::plain::KeyValues;

/// A carrying-device reference — `{id, name}` — so a consumer can jump straight
/// to the device (`nbox_get kind=device`). Mirrors the circuit view's `DeviceRef`
/// shape so the two kinds read identically to a host.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceRef {
    pub id: u64,
    pub name: String,
}

/// An interface reference — `{id, name}` — so a consumer can open the
/// termination's interface (`nbox_get kind=interface ref=<device>/<name>`).
#[derive(Debug, Clone, Serialize)]
pub struct InterfaceRef {
    pub id: u64,
    pub name: String,
}

/// One termination of a virtual circuit, normalized for display. Lands on a
/// device interface (carried as structured refs) with an optional role.
#[derive(Debug, Clone, Serialize)]
pub struct VirtualCircuitTerminationView {
    /// `device interface` (e.g. `edge01 xe-0/0/0`), or `(unterminated)` when no
    /// interface is set. The human-readable endpoint; the structured
    /// `device`/`interface` fields below are the machine form.
    pub endpoint: String,
    /// The device carrying the interface — `{id, name}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceRef>,
    /// The interface — `{id, name}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface: Option<InterfaceRef>,
    /// The termination role value (e.g. `peer`/`hub`/`spoke`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl VirtualCircuitTerminationView {
    /// Normalize a wire termination into a view.
    fn from_model(t: VirtualCircuitTermination) -> Self {
        let (endpoint, device, interface) = match t.interface.as_ref() {
            Some(b) => {
                let iface_name = b.name.clone().unwrap_or_else(|| format!("#{}", b.id));
                let dev = b.device.as_deref();
                let endpoint = match dev {
                    Some(d) => {
                        let dev_name = d.name.clone().unwrap_or_else(|| format!("#{}", d.id));
                        format!("{dev_name} {iface_name}")
                    }
                    None => iface_name.clone(),
                };
                let device = dev.map(|d| DeviceRef {
                    id: d.id,
                    name: d.name.clone().unwrap_or_else(|| format!("#{}", d.id)),
                });
                let interface = Some(InterfaceRef {
                    id: b.id,
                    name: iface_name,
                });
                (endpoint, device, interface)
            }
            None => ("(unterminated)".to_string(), None, None),
        };
        // The position is rendered by `to_plain` (its own enumerate); the view
        // itself is position-free so it round-trips through JSON identically.
        Self {
            endpoint,
            device,
            interface,
            role: t.role.map(|c| c.value),
            description: t.description.and_then(non_empty),
        }
    }
}

/// A virtual circuit, normalized to flat string fields for display, plus its
/// (multi-point) terminations.
#[derive(Debug, Clone, Serialize)]
pub struct VirtualCircuitView {
    pub cid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_network: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_account: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The multi-point terminations (no A/Z ordering — left in fetch order).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub terminations: Vec<VirtualCircuitTerminationView>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl VirtualCircuitView {
    /// Normalize a wire [`VirtualCircuit`] without its terminations (attributes
    /// only).
    pub fn from_model(vc: VirtualCircuit) -> Self {
        Self::build(vc, Vec::new())
    }

    /// Normalize a [`VirtualCircuit`] plus its terminations.
    pub fn build(vc: VirtualCircuit, terminations: Vec<VirtualCircuitTermination>) -> Self {
        let terms = terminations
            .into_iter()
            .map(VirtualCircuitTerminationView::from_model)
            .collect();
        Self {
            cid: vc.cid,
            provider_network: vc.provider_network.map(|b| b.label()),
            provider_account: vc.provider_account.map(|b| b.label()),
            type_: vc.type_.map(|b| b.label()),
            status: vc.status.map(|c| c.value),
            tenant: vc.tenant.map(|b| b.label()),
            owner: vc.owner.map(|b| b.label()),
            description: vc.description.and_then(non_empty),
            terminations: terms,
            tags: vc.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&vc.custom_fields),
        }
    }

    /// The circuit's attribute key-values (no terminations).
    fn attributes(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("cid", self.cid.clone())
            .push_opt("provider_network", self.provider_network.clone())
            .push_opt("provider_account", self.provider_account.clone())
            .push_opt("type", self.type_.clone())
            .push_opt("status", self.status.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("owner", self.owner.clone())
            .push_opt("description", self.description.clone());
        if !self.tags.is_empty() {
            kv.push("tags", self.tags.join(", "));
        }
        custom::append(&mut kv, &self.custom_fields);
        kv
    }

    /// Render as `key: value` lines (attributes only) — the TUI detail body and
    /// the back-compat plain renderer for callers that don't want terminations.
    pub fn to_key_values(&self) -> KeyValues {
        self.attributes()
    }

    /// The full CLI `nbox virtual-circuit` output: attributes followed by the
    /// terminations (one numbered row each — multi-point, no A/Z diagram).
    pub fn to_plain(&self) -> String {
        let mut out = self.attributes().render();
        if !self.terminations.is_empty() {
            out.push_str("\n\nTerminations\n");
            let rows: Vec<String> = self
                .terminations
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let mut line = format!("  {}. {}", i + 1, t.endpoint);
                    if let Some(role) = &t.role {
                        line.push_str("  ·  ");
                        line.push_str(role);
                    }
                    if let Some(desc) = &t.description {
                        line.push_str("  ·  ");
                        line.push_str(desc);
                    }
                    line
                })
                .collect();
            out.push_str(&rows.join("\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vc(value: Value) -> VirtualCircuit {
        serde_json::from_value(value).unwrap()
    }

    fn term(value: Value) -> VirtualCircuitTermination {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn flattens_virtual_circuit_attributes() {
        let v = vc(json!({
            "id": 7, "url": "u", "cid": "VC-100",
            "provider_network": {"id": 3, "name": "ACME Cloud", "display": "ACME Cloud"},
            "type": {"id": 2, "name": "MPLS", "slug": "mpls"},
            "status": {"value": "active", "label": "Active"},
            "tenant": {"id": 4, "name": "acme"},
            "owner": {"id": 1, "name": "netops"},
            "description": "east-west overlay",
            "tags": [{"id": 1, "name": "transit", "slug": "transit"}],
            "custom_fields": {"sla": "gold"}
        }));
        let view = VirtualCircuitView::from_model(v);
        assert_eq!(view.cid, "VC-100");
        assert_eq!(view.provider_network.as_deref(), Some("ACME Cloud"));
        assert_eq!(view.type_.as_deref(), Some("MPLS"));
        assert_eq!(view.status.as_deref(), Some("active"));
        assert_eq!(view.tenant.as_deref(), Some("acme"));
        assert_eq!(view.owner.as_deref(), Some("netops"));
        assert_eq!(view.tags, vec!["transit"]);
        assert!(view.terminations.is_empty());

        let plain = view.to_plain();
        assert!(plain.starts_with("cid: VC-100"));
        assert!(plain.contains("provider_network: ACME Cloud"));
        assert!(plain.contains("type: MPLS"));
        assert!(plain.contains("status: active"));
        assert!(plain.contains("owner: netops"));
        assert!(plain.contains("tags: transit"));
        assert!(plain.contains("cf.sla: gold"));
        assert!(!plain.contains("Terminations"));
    }

    #[test]
    fn empty_optionals_dropped_in_json() {
        let v = vc(json!({"id": 7, "url": "u", "cid": "VC-1", "description": ""}));
        let view = VirtualCircuitView::from_model(v);
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["cid"], "VC-1");
        // All optional scalars + the empty terminations/tags/custom_fields are
        // omitted.
        for key in [
            "provider_network",
            "provider_account",
            "type",
            "status",
            "tenant",
            "owner",
            "description",
            "terminations",
            "tags",
            "custom_fields",
        ] {
            assert!(value.get(key).is_none(), "{key} should be omitted");
        }
    }

    #[test]
    fn builds_terminations_with_device_and_interface_refs() {
        let v = vc(json!({
            "id": 7, "url": "u", "cid": "VC-100",
            "type": {"id": 2, "name": "MPLS"},
            "status": {"value": "active", "label": "Active"}
        }));
        let t1 = term(json!({
            "id": 10,
            "interface": {
                "id": 50, "name": "xe-0/0/0", "display": "edge01 xe-0/0/0",
                "device": {"id": 8, "name": "edge01"}
            },
            "role": {"value": "hub", "label": "Hub"},
            "description": "a-side"
        }));
        let t2 = term(json!({
            "id": 11,
            "interface": {
                "id": 51, "name": "xe-0/0/0",
                "device": {"id": 9, "name": "edge02"}
            }
        }));
        let view = VirtualCircuitView::build(v, vec![t1, t2]);
        assert_eq!(view.terminations.len(), 2);
        let a = &view.terminations[0];
        assert_eq!(a.endpoint, "edge01 xe-0/0/0");
        assert_eq!(a.device.as_ref().unwrap().id, 8);
        assert_eq!(a.device.as_ref().unwrap().name, "edge01");
        assert_eq!(a.interface.as_ref().unwrap().id, 50);
        assert_eq!(a.interface.as_ref().unwrap().name, "xe-0/0/0");
        assert_eq!(a.role.as_deref(), Some("hub"));
        assert_eq!(a.description.as_deref(), Some("a-side"));
        // No role/description on the second termination.
        let b = &view.terminations[1];
        assert_eq!(b.endpoint, "edge02 xe-0/0/0");
        assert!(b.role.is_none());
        assert!(b.description.is_none());

        let plain = view.to_plain();
        assert!(plain.contains("Terminations\n"));
        assert!(plain.contains("1. edge01 xe-0/0/0  ·  hub  ·  a-side"));
        assert!(plain.contains("2. edge02 xe-0/0/0"));
    }

    #[test]
    fn unterminated_termination_shows_placeholder() {
        let v = vc(json!({"id": 7, "url": "u", "cid": "VC-1"}));
        let t = term(json!({"id": 99}));
        let view = VirtualCircuitView::build(v, vec![t]);
        assert_eq!(view.terminations[0].endpoint, "(unterminated)");
        assert!(view.terminations[0].device.is_none());
        assert!(view.terminations[0].interface.is_none());
        let value = serde_json::to_value(&view).unwrap();
        // The unterminated row still serializes (endpoint present); its
        // optional refs/role/description are omitted.
        assert_eq!(value["terminations"][0]["endpoint"], "(unterminated)");
        assert!(value["terminations"][0].get("device").is_none());
    }

    #[test]
    fn interface_without_device_uses_interface_name_only() {
        let v = vc(json!({"id": 7, "url": "u", "cid": "VC-1"}));
        let t = term(json!({
            "id": 10,
            "interface": {"id": 50, "name": "xe-0/0/0"}
        }));
        let view = VirtualCircuitView::build(v, vec![t]);
        assert_eq!(view.terminations[0].endpoint, "xe-0/0/0");
        assert!(view.terminations[0].device.is_none());
        assert_eq!(
            view.terminations[0].interface.as_ref().unwrap().name,
            "xe-0/0/0"
        );
    }

    #[test]
    fn serializes_structured_device_and_interface_refs() {
        let v = vc(json!({"id": 7, "url": "u", "cid": "VC-1"}));
        let t = term(json!({
            "id": 10,
            "interface": {
                "id": 50, "name": "xe-0/0/0",
                "device": {"id": 8, "name": "edge01"}
            }
        }));
        let s = serde_json::to_value(VirtualCircuitView::build(v, vec![t])).unwrap();
        assert_eq!(s["terminations"][0]["device"]["id"], 8);
        assert_eq!(s["terminations"][0]["device"]["name"], "edge01");
        assert_eq!(s["terminations"][0]["interface"]["id"], 50);
        assert_eq!(s["terminations"][0]["interface"]["name"], "xe-0/0/0");
    }
}
