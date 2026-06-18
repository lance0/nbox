//! Flattened rack view for `nbox rack` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::non_empty;
use crate::netbox::models::dcim::Rack;
use crate::output::plain::KeyValues;

/// A rack, normalized to flat string fields.
#[derive(Debug, Clone, Serialize)]
pub struct RackView {
    pub id: u64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub u_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl RackView {
    /// Normalize a wire [`Rack`].
    pub fn from_model(r: Rack) -> Self {
        Self {
            id: r.id,
            name: r.name,
            status: r.status.map(|c| c.value),
            site: r.site.map(|b| b.label()),
            location: r.location.map(|b| b.label()),
            role: r.role.map(|b| b.label()),
            tenant: r.tenant.map(|b| b.label()),
            u_height: r.u_height,
            serial: r.serial.and_then(non_empty),
            asset_tag: r.asset_tag.and_then(non_empty),
            description: r.description.and_then(non_empty),
            tags: r.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&r.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("name", self.name.clone())
            .push_opt("status", self.status.clone())
            .push_opt("site", self.site.clone())
            .push_opt("location", self.location.clone())
            .push_opt("role", self.role.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("u_height", self.u_height.map(|u| u.to_string()))
            .push_opt("serial", self.serial.clone())
            .push_opt("asset_tag", self.asset_tag.clone())
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
    fn flattens_rack() {
        let r: Rack = serde_json::from_value(json!({
            "id": 12, "url": "u", "name": "r12",
            "status": {"value": "active", "label": "Active"},
            "site": {"id": 1, "display": "iad1"},
            "u_height": 42,
            "tags": [{"id": 1, "name": "row-a", "slug": "row-a"}]
        }))
        .unwrap();
        let view = RackView::from_model(r);
        assert_eq!(view.site.as_deref(), Some("iad1"));
        assert_eq!(view.u_height, Some(42));
        assert_eq!(view.tags, vec!["row-a"]);
        let plain = view.to_key_values().render();
        assert!(plain.contains("u_height: 42"));
        assert!(plain.contains("tags: row-a"));
    }

    #[test]
    fn tags_dropped_when_empty() {
        let r: Rack = serde_json::from_value(json!({
            "id": 12, "url": "u", "name": "r12"
        }))
        .unwrap();
        let view = RackView::from_model(r);
        assert!(view.tags.is_empty());
        let value = serde_json::to_value(&view).unwrap();
        assert!(value.get("tags").is_none());
        assert!(!view.to_key_values().render().contains("tags:"));
    }
}
