//! Flattened tenant view for `nbox tenant` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::{non_empty, non_zero};
use crate::netbox::models::tenancy::Tenant;
use crate::output::plain::KeyValues;

/// A tenant, normalized to flat string fields for display.
#[derive(Debug, Clone, Serialize)]
pub struct TenantView {
    pub id: u64,
    pub name: String,
    pub slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    // Relation counts the serializer reports — only surfaced when non-zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub circuit_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipaddress_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rack_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vlan_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vrf_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub virtualmachine_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl TenantView {
    /// Normalize a wire [`Tenant`] into a flat view.
    pub fn from_model(t: Tenant) -> Self {
        Self {
            id: t.id,
            name: t.name,
            slug: t.slug,
            group: t.group.map(|b| b.label()),
            description: t.description.and_then(non_empty),
            circuit_count: non_zero(t.circuit_count),
            device_count: non_zero(t.device_count),
            ipaddress_count: non_zero(t.ipaddress_count),
            prefix_count: non_zero(t.prefix_count),
            rack_count: non_zero(t.rack_count),
            site_count: non_zero(t.site_count),
            vlan_count: non_zero(t.vlan_count),
            vrf_count: non_zero(t.vrf_count),
            virtualmachine_count: non_zero(t.virtualmachine_count),
            owner: t.owner.map(|bo| bo.label()),
            tags: t.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&t.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("name", self.name.clone())
            .push("slug", self.slug.clone())
            .push_opt("group", self.group.clone())
            .push_opt("description", self.description.clone())
            .push_opt("circuit_count", self.circuit_count.map(|n| n.to_string()))
            .push_opt("device_count", self.device_count.map(|n| n.to_string()))
            .push_opt(
                "ipaddress_count",
                self.ipaddress_count.map(|n| n.to_string()),
            )
            .push_opt("prefix_count", self.prefix_count.map(|n| n.to_string()))
            .push_opt("rack_count", self.rack_count.map(|n| n.to_string()))
            .push_opt("site_count", self.site_count.map(|n| n.to_string()))
            .push_opt("vlan_count", self.vlan_count.map(|n| n.to_string()))
            .push_opt("vrf_count", self.vrf_count.map(|n| n.to_string()))
            .push_opt(
                "virtualmachine_count",
                self.virtualmachine_count.map(|n| n.to_string()),
            );
        if !self.tags.is_empty() {
            kv.push("tags", self.tags.join(", "));
        }
        kv.push_opt("owner", self.owner.clone());
        custom::append(&mut kv, &self.custom_fields);
        kv
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_tenant() {
        let t: Tenant = serde_json::from_value(json!({
            "id": 4, "url": "u", "name": "Acme Corp", "slug": "acme",
            "group": {"id": 2, "display": "Customers"},
            "description": "primary customer",
            "device_count": 12,
            "prefix_count": 5,
            "site_count": 0,
            "tags": [{"id": 1, "name": "vip", "slug": "vip"}],
            "custom_fields": {"account_id": "A-100"}
        }))
        .unwrap();
        let view = TenantView::from_model(t);
        assert_eq!(view.name, "Acme Corp");
        assert_eq!(view.group.as_deref(), Some("Customers"));
        assert_eq!(view.device_count, Some(12));
        assert_eq!(view.prefix_count, Some(5));
        // Zero counts are dropped (noise).
        assert_eq!(view.site_count, None);
        assert_eq!(view.tags, vec!["vip"]);

        let plain = view.to_key_values().render();
        assert!(plain.starts_with("name: Acme Corp\nslug: acme"));
        assert!(plain.contains("group: Customers"));
        assert!(plain.contains("device_count: 12"));
        assert!(plain.contains("tags: vip"));
        assert!(plain.contains("cf.account_id: A-100"));
        // Dropped fields don't appear.
        assert!(!plain.contains("site_count"));
    }

    #[test]
    fn empty_optionals_dropped_in_json() {
        let t: Tenant = serde_json::from_value(json!({
            "id": 4, "url": "u", "name": "bare", "slug": "bare",
            "description": ""
        }))
        .unwrap();
        let view = TenantView::from_model(t);
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["name"], "bare");
        assert!(value.get("group").is_none());
        assert!(value.get("description").is_none());
        assert!(value.get("device_count").is_none());
        assert!(value.get("tags").is_none());
        assert!(value.get("custom_fields").is_none());
    }
}
