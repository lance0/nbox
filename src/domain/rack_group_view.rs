//! Flattened rack-group view for `nbox rack-group` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::{non_empty, non_zero};
use crate::netbox::models::dcim::RackGroup;
use crate::output::plain::KeyValues;

/// A rack group, normalized to flat string fields for display.
#[derive(Debug, Clone, Serialize)]
pub struct RackGroupView {
    pub id: u64,
    pub name: String,
    pub slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The native owner (NetBox 4.5+); a user/group brief label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Relation count the serializer reports — only surfaced when non-zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rack_count: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl RackGroupView {
    /// Normalize a wire [`RackGroup`] into a flat view.
    pub fn from_model(rg: RackGroup) -> Self {
        Self {
            id: rg.id,
            name: rg.name,
            slug: rg.slug,
            description: rg.description.and_then(non_empty),
            owner: rg.owner.map(|b| b.label()),
            rack_count: non_zero(rg.rack_count),
            tags: rg.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&rg.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("name", self.name.clone())
            .push("slug", self.slug.clone())
            .push_opt("description", self.description.clone())
            .push_opt("owner", self.owner.clone())
            .push_opt("rack_count", self.rack_count.map(|n| n.to_string()));
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
    fn flattens_rack_group() {
        let rg: RackGroup = serde_json::from_value(json!({
            "id": 1, "url": "u", "name": "Row A", "slug": "row-a",
            "description": "top of row 1",
            "owner": {"id": 3, "name": "netops"},
            "rack_count": 12,
            "tags": [{"id": 1, "name": "prod", "slug": "prod"}],
            "custom_fields": {"floor": "2"}
        }))
        .unwrap();
        let view = RackGroupView::from_model(rg);
        assert_eq!(view.name, "Row A");
        assert_eq!(view.slug, "row-a");
        assert_eq!(view.owner.as_deref(), Some("netops"));
        assert_eq!(view.rack_count, Some(12));
        assert_eq!(view.tags, vec!["prod"]);

        let plain = view.to_key_values().render();
        assert!(plain.starts_with("name: Row A\nslug: row-a"));
        assert!(plain.contains("description: top of row 1"));
        assert!(plain.contains("owner: netops"));
        assert!(plain.contains("rack_count: 12"));
        assert!(plain.contains("tags: prod"));
        assert!(plain.contains("cf.floor: 2"));
    }

    #[test]
    fn empty_optionals_dropped_in_json() {
        let rg: RackGroup = serde_json::from_value(json!({
            "id": 1, "url": "u", "name": "bare", "slug": "bare",
            "description": "", "rack_count": 0
        }))
        .unwrap();
        let view = RackGroupView::from_model(rg);
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["name"], "bare");
        for key in [
            "description",
            "owner",
            "rack_count",
            "tags",
            "custom_fields",
        ] {
            assert!(value.get(key).is_none(), "{key} should be omitted");
        }
    }
}
