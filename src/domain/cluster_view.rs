//! Flattened cluster view for `nbox cluster` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::{non_empty, non_zero};
use crate::netbox::models::virtualization::Cluster;
use crate::netbox::query::friendly_scope_type;
use crate::output::plain::KeyValues;

/// A cluster, normalized to flat string fields for display.
#[derive(Debug, Clone, Serialize)]
pub struct ClusterView {
    pub id: u64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Name of the cluster's scope object (site, location, region, …) for any
    /// scope type — see [`scope_type`](Self::scope_type) for which kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Friendly scope type, e.g. `site`/`location`/`region`/`site-group`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    // Relation counts the serializer reports — only surfaced when non-zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub virtualmachine_count: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl ClusterView {
    /// Normalize a wire [`Cluster`] into a flat view.
    pub fn from_model(c: Cluster) -> Self {
        Self {
            id: c.id,
            name: c.name,
            r#type: c.type_.map(|b| b.label()),
            group: c.group.map(|b| b.label()),
            status: c.status.map(|s| s.value),
            tenant: c.tenant.map(|b| b.label()),
            scope: c.scope.map(|b| b.label()),
            scope_type: c.scope_type.as_deref().map(friendly_scope_type),
            description: c.description.and_then(non_empty),
            device_count: non_zero(c.device_count),
            virtualmachine_count: non_zero(c.virtualmachine_count),
            tags: c.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&c.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("name", self.name.clone())
            .push_opt("type", self.r#type.clone())
            .push_opt("group", self.group.clone())
            .push_opt("status", self.status.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("scope", self.scope.clone())
            .push_opt("scope_type", self.scope_type.clone())
            .push_opt("description", self.description.clone())
            .push_opt("device_count", self.device_count.map(|n| n.to_string()))
            .push_opt(
                "virtualmachine_count",
                self.virtualmachine_count.map(|n| n.to_string()),
            );
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
    fn flattens_cluster() {
        let c: Cluster = serde_json::from_value(json!({
            "id": 3, "url": "u", "name": "prod",
            "type": {"id": 1, "display": "VMware"},
            "group": {"id": 2, "display": "us-east"},
            "status": {"value": "active", "label": "Active"},
            "tenant": {"id": 9, "display": "Acme"},
            "scope_type": "dcim.site",
            "scope": {"id": 1, "display": "iad1"},
            "description": "primary cluster",
            "device_count": 4,
            "virtualmachine_count": 0,
            "tags": [{"id": 1, "name": "prod", "slug": "prod"}],
            "custom_fields": {"sla": "gold"}
        }))
        .unwrap();
        let view = ClusterView::from_model(c);
        assert_eq!(view.name, "prod");
        assert_eq!(view.r#type.as_deref(), Some("VMware"));
        assert_eq!(view.group.as_deref(), Some("us-east"));
        assert_eq!(view.status.as_deref(), Some("active"));
        assert_eq!(view.tenant.as_deref(), Some("Acme"));
        assert_eq!(view.scope.as_deref(), Some("iad1"));
        assert_eq!(view.scope_type.as_deref(), Some("site"));
        assert_eq!(view.device_count, Some(4));
        // Zero counts are dropped (noise).
        assert_eq!(view.virtualmachine_count, None);
        assert_eq!(view.tags, vec!["prod"]);

        let plain = view.to_key_values().render();
        assert!(plain.starts_with("name: prod"));
        assert!(plain.contains("type: VMware"));
        assert!(plain.contains("group: us-east"));
        assert!(plain.contains("status: active"));
        assert!(plain.contains("scope: iad1"));
        assert!(plain.contains("scope_type: site"));
        assert!(plain.contains("device_count: 4"));
        assert!(plain.contains("tags: prod"));
        assert!(plain.contains("cf.sla: gold"));
        // Dropped fields don't appear.
        assert!(!plain.contains("virtualmachine_count"));
    }

    #[test]
    fn empty_optionals_dropped_in_json() {
        let c: Cluster = serde_json::from_value(json!({
            "id": 3, "url": "u", "name": "bare",
            "description": "",
            "virtualmachine_count": 0
        }))
        .unwrap();
        let view = ClusterView::from_model(c);
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["name"], "bare");
        assert!(value.get("type").is_none());
        assert!(value.get("group").is_none());
        assert!(value.get("status").is_none());
        assert!(value.get("scope").is_none());
        assert!(value.get("description").is_none());
        // Zero virtualmachine_count is dropped.
        assert!(value.get("virtualmachine_count").is_none());
        assert!(value.get("tags").is_none());
        assert!(value.get("custom_fields").is_none());
    }
}
