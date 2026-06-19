//! Flattened VRF view for `nbox vrf` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::non_empty;
use crate::netbox::models::ipam::Vrf;
use crate::output::plain::KeyValues;

/// A VRF, normalized to flat string fields.
#[derive(Debug, Clone, Serialize)]
pub struct VrfView {
    pub id: u64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enforce_unique: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub import_targets: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub export_targets: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipaddress_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl VrfView {
    /// Normalize a wire [`Vrf`].
    pub fn from_model(v: Vrf) -> Self {
        Self {
            id: v.id,
            name: v.name,
            rd: v.rd.and_then(non_empty),
            tenant: v.tenant.map(|b| b.label()),
            enforce_unique: v.enforce_unique,
            import_targets: v.import_targets.into_iter().map(|b| b.label()).collect(),
            export_targets: v.export_targets.into_iter().map(|b| b.label()).collect(),
            prefix_count: v.prefix_count,
            ipaddress_count: v.ipaddress_count,
            description: v.description.and_then(non_empty),
            tags: v.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&v.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("name", self.name.clone())
            .push_opt("rd", self.rd.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("enforce_unique", self.enforce_unique.map(|b| b.to_string()));
        if !self.import_targets.is_empty() {
            kv.push("import_targets", self.import_targets.join(", "));
        }
        if !self.export_targets.is_empty() {
            kv.push("export_targets", self.export_targets.join(", "));
        }
        kv.push_opt("prefixes", self.prefix_count.map(|c| c.to_string()))
            .push_opt("addresses", self.ipaddress_count.map(|c| c.to_string()))
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
    fn flattens_vrf() {
        let v: Vrf = serde_json::from_value(json!({
            "id": 7, "url": "u", "name": "customer-prod",
            "rd": "65000:100",
            "tenant": {"id": 1, "display": "Acme Corp"},
            "enforce_unique": true,
            "import_targets": [{"id": 1, "name": "65000:100"}, {"id": 2, "name": "65000:200"}],
            "export_targets": [{"id": 1, "name": "65000:100"}],
            "prefix_count": 12,
            "ipaddress_count": 48,
            "tags": [{"id": 1, "name": "prod", "slug": "prod"}]
        }))
        .unwrap();
        let view = VrfView::from_model(v);
        assert_eq!(view.rd.as_deref(), Some("65000:100"));
        assert_eq!(view.tenant.as_deref(), Some("Acme Corp"));
        assert_eq!(view.enforce_unique, Some(true));
        assert_eq!(view.import_targets, vec!["65000:100", "65000:200"]);
        assert_eq!(view.prefix_count, Some(12));
        let plain = view.to_key_values().render();
        assert!(plain.contains("rd: 65000:100"));
        assert!(plain.contains("import_targets: 65000:100, 65000:200"));
        assert!(plain.contains("prefixes: 12"));
        assert!(plain.contains("tags: prod"));
    }

    #[test]
    fn empty_optionals_dropped() {
        let v: Vrf = serde_json::from_value(json!({
            "id": 1, "url": "u", "name": "global"
        }))
        .unwrap();
        let view = VrfView::from_model(v);
        assert!(view.rd.is_none());
        assert!(view.import_targets.is_empty());
        let value = serde_json::to_value(&view).unwrap();
        assert!(value.get("rd").is_none());
        assert!(value.get("import_targets").is_none());
        assert!(!view.to_key_values().render().contains("rd:"));
    }
}
