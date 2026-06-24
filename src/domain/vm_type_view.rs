//! Flattened virtual-machine-type view for `nbox vm-type` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::{non_empty, non_zero};
use crate::netbox::models::virtualization::VirtualMachineType;
use crate::output::plain::KeyValues;

/// A virtual machine type (a reusable VM template), normalized to flat string
/// fields for display.
#[derive(Debug, Clone, Serialize)]
pub struct VirtualMachineTypeView {
    pub id: u64,
    pub name: String,
    pub slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_vcpus: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_memory: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The native owner (NetBox 4.5+); a user/group brief label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Relation count the serializer reports — only surfaced when non-zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub virtual_machine_count: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl VirtualMachineTypeView {
    /// Normalize a wire [`VirtualMachineType`] into a flat view.
    pub fn from_model(t: VirtualMachineType) -> Self {
        Self {
            id: t.id,
            name: t.name,
            slug: t.slug,
            default_platform: t.default_platform.map(|b| b.label()),
            default_vcpus: t.default_vcpus,
            default_memory: t.default_memory,
            description: t.description.and_then(non_empty),
            owner: t.owner.map(|b| b.label()),
            virtual_machine_count: non_zero(t.virtual_machine_count),
            tags: t.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&t.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("name", self.name.clone())
            .push("slug", self.slug.clone())
            .push_opt("default_platform", self.default_platform.clone())
            .push_opt("default_vcpus", self.default_vcpus.map(|n| n.to_string()))
            .push_opt("default_memory", self.default_memory.map(|n| n.to_string()))
            .push_opt("description", self.description.clone())
            .push_opt("owner", self.owner.clone())
            .push_opt(
                "virtual_machine_count",
                self.virtual_machine_count.map(|n| n.to_string()),
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
    fn flattens_vm_type() {
        let t: VirtualMachineType = serde_json::from_value(json!({
            "id": 1, "url": "u", "name": "Web Tier", "slug": "web-tier",
            "default_platform": {"id": 2, "name": "debian-12", "slug": "debian-12"},
            "default_vcpus": 4.0,
            "default_memory": 8192,
            "description": "front-end web servers",
            "owner": {"id": 3, "name": "netops"},
            "virtual_machine_count": 24,
            "tags": [{"id": 1, "name": "prod", "slug": "prod"}],
            "custom_fields": {"tier": "web"}
        }))
        .unwrap();
        let view = VirtualMachineTypeView::from_model(t);
        assert_eq!(view.name, "Web Tier");
        assert_eq!(view.default_platform.as_deref(), Some("debian-12"));
        assert_eq!(view.default_vcpus, Some(4.0));
        assert_eq!(view.default_memory, Some(8192));
        assert_eq!(view.owner.as_deref(), Some("netops"));
        assert_eq!(view.virtual_machine_count, Some(24));
        assert_eq!(view.tags, vec!["prod"]);

        let plain = view.to_key_values().render();
        assert!(plain.starts_with("name: Web Tier\nslug: web-tier"));
        assert!(plain.contains("default_platform: debian-12"));
        assert!(plain.contains("default_vcpus: 4"));
        assert!(plain.contains("default_memory: 8192"));
        assert!(plain.contains("owner: netops"));
        assert!(plain.contains("virtual_machine_count: 24"));
        assert!(plain.contains("tags: prod"));
        assert!(plain.contains("cf.tier: web"));
    }

    #[test]
    fn empty_optionals_dropped_in_json() {
        let t: VirtualMachineType =
            serde_json::from_value(json!({"id": 1, "url": "u", "name": "bare", "slug": "bare"}))
                .unwrap();
        let view = VirtualMachineTypeView::from_model(t);
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["name"], "bare");
        for key in [
            "default_platform",
            "default_vcpus",
            "default_memory",
            "description",
            "owner",
            "virtual_machine_count",
            "tags",
            "custom_fields",
        ] {
            assert!(value.get(key).is_none(), "{key} should be omitted");
        }
    }
}
