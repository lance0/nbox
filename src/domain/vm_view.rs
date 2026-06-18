//! Flattened virtual machine view for `nbox vm` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::non_empty;
use crate::netbox::models::virtualization::VirtualMachine;
use crate::output::plain::KeyValues;

/// A virtual machine, normalized to flat string fields for display.
#[derive(Debug, Clone, Serialize)]
pub struct VmView {
    pub id: u64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_ip4: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_ip6: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcpus: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl VmView {
    /// Normalize a wire [`VirtualMachine`] into a flat view.
    pub fn from_model(vm: VirtualMachine) -> Self {
        Self {
            id: vm.id,
            name: vm.name,
            status: vm.status.map(|c| c.value),
            role: vm.role.map(|b| b.label()),
            cluster: vm.cluster.map(|b| b.label()),
            device: vm.device.map(|b| b.label()),
            platform: vm.platform.map(|b| b.label()),
            site: vm.site.map(|b| b.label()),
            tenant: vm.tenant.map(|b| b.label()),
            primary_ip4: vm.primary_ip4.map(|b| b.label()),
            primary_ip6: vm.primary_ip6.map(|b| b.label()),
            vcpus: vm.vcpus,
            memory: vm.memory,
            disk: vm.disk,
            description: vm.description.and_then(non_empty),
            tags: vm.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&vm.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("name", self.name.clone())
            .push_opt("status", self.status.clone())
            .push_opt("role", self.role.clone())
            .push_opt("cluster", self.cluster.clone())
            .push_opt("device", self.device.clone())
            .push_opt("platform", self.platform.clone())
            .push_opt("site", self.site.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("primary_ip4", self.primary_ip4.clone())
            .push_opt("primary_ip6", self.primary_ip6.clone())
            .push_opt("vcpus", self.vcpus.map(|n| n.to_string()))
            .push_opt("memory", self.memory.map(|n| n.to_string()))
            .push_opt("disk", self.disk.map(|n| n.to_string()))
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
    fn flattens_vm() {
        let vm: VirtualMachine = serde_json::from_value(json!({
            "id": 5, "url": "u", "name": "web-01",
            "status": {"value": "active", "label": "Active"},
            "role": {"id": 2, "display": "App Server"},
            "cluster": {"id": 3, "display": "prod"},
            "device": {"id": 7, "display": "hv-01"},
            "platform": {"id": 4, "display": "Ubuntu 22.04"},
            "site": {"id": 1, "display": "iad1"},
            "tenant": {"id": 9, "display": "Acme"},
            "primary_ip4": {"id": 11, "display": "10.0.0.5/24"},
            "vcpus": 4.0,
            "memory": 8192,
            "disk": 100,
            "description": "primary web node",
            "tags": [{"id": 1, "name": "prod", "slug": "prod"}],
            "custom_fields": {"owner": "platform"}
        }))
        .unwrap();
        let view = VmView::from_model(vm);
        assert_eq!(view.name, "web-01");
        assert_eq!(view.status.as_deref(), Some("active"));
        assert_eq!(view.role.as_deref(), Some("App Server"));
        assert_eq!(view.cluster.as_deref(), Some("prod"));
        assert_eq!(view.device.as_deref(), Some("hv-01"));
        assert_eq!(view.platform.as_deref(), Some("Ubuntu 22.04"));
        assert_eq!(view.site.as_deref(), Some("iad1"));
        assert_eq!(view.tenant.as_deref(), Some("Acme"));
        assert_eq!(view.primary_ip4.as_deref(), Some("10.0.0.5/24"));
        assert_eq!(view.vcpus, Some(4.0));
        assert_eq!(view.memory, Some(8192));
        assert_eq!(view.disk, Some(100));
        assert_eq!(view.tags, vec!["prod"]);

        let plain = view.to_key_values().render();
        assert!(plain.starts_with("name: web-01"));
        assert!(plain.contains("status: active"));
        assert!(plain.contains("cluster: prod"));
        assert!(plain.contains("device: hv-01"));
        assert!(plain.contains("vcpus: 4"));
        assert!(plain.contains("memory: 8192"));
        assert!(plain.contains("primary_ip4: 10.0.0.5/24"));
        assert!(plain.contains("tags: prod"));
        assert!(plain.contains("cf.owner: platform"));
    }

    #[test]
    fn empty_optionals_dropped_in_json() {
        let vm: VirtualMachine = serde_json::from_value(json!({
            "id": 5, "url": "u", "name": "bare",
            "description": ""
        }))
        .unwrap();
        let view = VmView::from_model(vm);
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["name"], "bare");
        assert!(value.get("status").is_none());
        assert!(value.get("cluster").is_none());
        assert!(value.get("description").is_none());
        assert!(value.get("vcpus").is_none());
        assert!(value.get("memory").is_none());
        assert!(value.get("primary_ip4").is_none());
        assert!(value.get("tags").is_none());
        assert!(value.get("custom_fields").is_none());
    }
}
