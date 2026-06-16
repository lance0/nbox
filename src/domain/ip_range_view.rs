//! Flattened IP-range view for `nbox ip-range` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::netbox::models::ipam::IpRange;
use crate::output::plain::KeyValues;

/// An IP range, normalized to flat fields for display.
#[derive(Debug, Clone, Serialize)]
pub struct IpRangeView {
    pub start_address: String,
    pub end_address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vrf: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl IpRangeView {
    /// Normalize a wire [`IpRange`] into a flat view.
    pub fn from_model(r: IpRange) -> Self {
        let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };
        Self {
            start_address: r.start_address,
            end_address: r.end_address,
            size: r.size,
            status: r.status.map(|c| c.value),
            vrf: r.vrf.map(|b| b.label()),
            tenant: r.tenant.map(|b| b.label()),
            role: r.role.map(|b| b.label()),
            description: r.description.and_then(non_empty),
            custom_fields: custom::fields(&r.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("start_address", self.start_address.clone())
            .push("end_address", self.end_address.clone())
            .push_opt("size", self.size.map(|s| s.to_string()))
            .push_opt("status", self.status.clone())
            .push_opt("vrf", self.vrf.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("role", self.role.clone())
            .push_opt("description", self.description.clone());
        custom::append(&mut kv, &self.custom_fields);
        kv
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_ip_range() {
        let range: IpRange = serde_json::from_value(json!({
            "id": 1, "url": "u",
            "start_address": "10.0.0.10/24", "end_address": "10.0.0.20/24",
            "size": 11,
            "status": {"value": "active", "label": "Active"},
            "custom_fields": {}
        }))
        .unwrap();
        let view = IpRangeView::from_model(range);
        assert_eq!(view.start_address, "10.0.0.10/24");
        assert_eq!(view.end_address, "10.0.0.20/24");
        assert_eq!(view.size, Some(11));
        let plain = view.to_key_values().render();
        assert!(plain.starts_with("start_address: 10.0.0.10/24"));
        assert!(plain.contains("size: 11"));
    }
}
