//! Flattened device view for `nbox device` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::non_empty;
use crate::netbox::models::dcim::Device;
use crate::output::plain::KeyValues;

/// A device, normalized to flat string fields for display.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceView {
    pub id: u64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_ip4: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_ip6: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl DeviceView {
    /// Normalize a wire [`Device`] into a flat view.
    pub fn from_model(d: Device) -> Self {
        Self {
            id: d.id,
            name: d.name,
            status: d.status.map(|c| c.value),
            role: d.role.map(|b| b.label()),
            site: d.site.map(|b| b.label()),
            rack: d.rack.map(|b| b.label()),
            platform: d.platform.map(|b| b.label()),
            tenant: d.tenant.map(|b| b.label()),
            primary_ip4: d.primary_ip4.map(|b| b.label()),
            primary_ip6: d.primary_ip6.map(|b| b.label()),
            serial: d.serial.and_then(non_empty),
            description: d.description.and_then(non_empty),
            tags: d.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&d.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("name", self.name.clone())
            .push_opt("status", self.status.clone())
            .push_opt("site", self.site.clone())
            .push_opt("rack", self.rack.clone())
            .push_opt("role", self.role.clone())
            .push_opt("platform", self.platform.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("primary_ip4", self.primary_ip4.clone())
            .push_opt("primary_ip6", self.primary_ip6.clone())
            .push_opt("serial", self.serial.clone())
            .push_opt("description", self.description.clone());
        if !self.tags.is_empty() {
            kv.push("tags", self.tags.join(", "));
        }
        custom::append(&mut kv, &self.custom_fields);
        kv
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_brief_relations_and_status() {
        let device: Device = serde_json::from_value(json!({
            "id": 123,
            "url": "http://nb/api/dcim/devices/123/",
            "name": "edge01",
            "status": {"value": "active", "label": "Active"},
            "site": {"id": 1, "display": "iad1"},
            "rack": {"id": 2, "name": "r12"},
            "primary_ip4": {"id": 9, "display": "10.44.12.9/32"},
            "serial": "",
            "tags": [{"id": 1, "name": "edge", "slug": "edge"}],
            "custom_fields": {}
        }))
        .unwrap();

        let view = DeviceView::from_model(device);
        assert_eq!(view.status.as_deref(), Some("active"));
        assert_eq!(view.site.as_deref(), Some("iad1"));
        assert_eq!(view.rack.as_deref(), Some("r12"));
        assert_eq!(view.primary_ip4.as_deref(), Some("10.44.12.9/32"));
        // Empty serial is dropped, not shown as "".
        assert_eq!(view.serial, None);
        assert_eq!(view.tags, vec!["edge"]);

        let plain = view.to_key_values().render();
        assert!(plain.starts_with("name: edge01"));
        assert!(plain.contains("primary_ip4: 10.44.12.9/32"));
        assert!(plain.contains("tags: edge"));
        assert!(!plain.contains("serial:"));
    }

    #[test]
    fn tags_dropped_when_empty() {
        let device: Device = serde_json::from_value(json!({
            "id": 1, "url": "u", "name": "edge01"
        }))
        .unwrap();
        let view = DeviceView::from_model(device);
        assert!(view.tags.is_empty());
        let value = serde_json::to_value(&view).unwrap();
        assert!(value.get("tags").is_none());
        assert!(!view.to_key_values().render().contains("tags:"));
    }

    #[test]
    fn surfaces_non_null_custom_fields() {
        let device: Device = serde_json::from_value(json!({
            "id": 1, "url": "u", "name": "edge01",
            "custom_fields": {"ticket": "INC-7", "owner": null}
        }))
        .unwrap();
        let view = DeviceView::from_model(device);
        assert_eq!(view.custom_fields.len(), 1);
        let plain = view.to_key_values().render();
        assert!(plain.contains("cf.ticket: INC-7"), "got: {plain}");
        assert!(!plain.contains("owner"));
    }
}
