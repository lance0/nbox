//! Virtualization models: virtual machines and clusters.

use serde::{Deserialize, Serialize};

use super::common::{BriefObject, Choice, Tag};

/// A virtual machine (`/api/virtualization/virtual-machines/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VirtualMachine {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,

    #[serde(default)]
    pub status: Option<Choice<String>>,
    #[serde(default)]
    pub role: Option<BriefObject>,
    #[serde(default)]
    pub cluster: Option<BriefObject>,
    #[serde(default)]
    pub device: Option<BriefObject>,
    #[serde(default)]
    pub platform: Option<BriefObject>,
    #[serde(default)]
    pub site: Option<BriefObject>,
    #[serde(default)]
    pub tenant: Option<BriefObject>,
    #[serde(default)]
    pub primary_ip4: Option<BriefObject>,
    #[serde(default)]
    pub primary_ip6: Option<BriefObject>,

    #[serde(default)]
    pub vcpus: Option<f64>,
    #[serde(default)]
    pub memory: Option<u64>,
    #[serde(default)]
    pub disk: Option<u64>,
    #[serde(default)]
    pub description: Option<String>,

    /// The native owner (NetBox 4.5+); a user/group brief. `None` on older
    /// releases or when unset.
    #[serde(default)]
    pub owner: Option<BriefObject>,
    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// A virtual machine type (`/api/virtualization/virtual-machine-types/`, 4.6+).
/// A reusable template for VMs — default platform/vCPUs/memory — referenced by
/// VMs. Carries an `owner` (4.5+) and a cheap `virtual_machine_count`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VirtualMachineType {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,
    pub slug: String,
    /// Default platform for VMs of this type (a platform brief).
    #[serde(default)]
    pub default_platform: Option<BriefObject>,
    /// Default vCPU count (a decimal in the OpenAPI schema).
    #[serde(default)]
    pub default_vcpus: Option<f64>,
    /// Default memory, in megabytes.
    #[serde(default)]
    pub default_memory: Option<u64>,
    #[serde(default)]
    pub description: Option<String>,
    /// The native owner (NetBox 4.5+); a user/group brief. `None` on older
    /// releases or when unset.
    #[serde(default)]
    pub owner: Option<BriefObject>,
    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
    /// Relation count the serializer reports (read-only).
    #[serde(default)]
    pub virtual_machine_count: Option<u64>,
}

/// A cluster (`/api/virtualization/clusters/`).
///
/// NetBox 4.2+ scopes a cluster polymorphically (`scope_type`/`scope_id`/`scope`)
/// rather than via a plain `site` FK, mirroring prefixes/VLANs.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Cluster {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,

    #[serde(rename = "type", default)]
    pub type_: Option<BriefObject>,
    #[serde(default)]
    pub group: Option<BriefObject>,
    #[serde(default)]
    pub status: Option<Choice<String>>,
    #[serde(default)]
    pub tenant: Option<BriefObject>,

    #[serde(default)]
    pub scope_type: Option<String>,
    #[serde(default)]
    pub scope_id: Option<u64>,
    #[serde(default)]
    pub scope: Option<BriefObject>,

    #[serde(default)]
    pub description: Option<String>,

    // Cheap relation counts the serializer always reports (read-only).
    #[serde(default)]
    pub device_count: Option<u64>,
    #[serde(default)]
    pub virtualmachine_count: Option<u64>,

    /// The native owner (NetBox 4.5+); a user/group brief. `None` on older
    /// releases or when unset.
    #[serde(default)]
    pub owner: Option<BriefObject>,
    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn virtual_machine_deserializes() {
        let vm: VirtualMachine = serde_json::from_value(json!({
            "id": 5,
            "url": "http://nb/api/virtualization/virtual-machines/5/",
            "name": "web-01"
        }))
        .unwrap();
        assert_eq!(vm.name, "web-01");
        assert!(vm.status.is_none());
        assert!(vm.cluster.is_none());
        assert!(vm.primary_ip4.is_none());
    }

    #[test]
    fn virtual_machine_full_deserializes() {
        let vm: VirtualMachine = serde_json::from_value(json!({
            "id": 5,
            "url": "http://nb/api/virtualization/virtual-machines/5/",
            "display": "web-01",
            "name": "web-01",
            "status": {"value": "active", "label": "Active"},
            "role": {"id": 2, "url": "u", "display": "App Server", "name": "App Server", "slug": "app"},
            "cluster": {"id": 3, "url": "u", "display": "prod", "name": "prod"},
            "device": {"id": 7, "url": "u", "display": "hv-01", "name": "hv-01"},
            "platform": {"id": 4, "url": "u", "display": "Ubuntu 22.04", "name": "Ubuntu 22.04", "slug": "ubuntu"},
            "site": {"id": 1, "url": "u", "display": "iad1", "name": "iad1", "slug": "iad1"},
            "tenant": {"id": 9, "url": "u", "display": "Acme", "name": "Acme", "slug": "acme"},
            "primary_ip4": {"id": 11, "url": "u", "display": "10.0.0.5/24", "address": "10.0.0.5/24"},
            "primary_ip6": {"id": 12, "url": "u", "display": "2001:db8::5/64", "address": "2001:db8::5/64"},
            "vcpus": 4.0,
            "memory": 8192,
            "disk": 100,
            "description": "primary web node",
            "tags": [{"id": 1, "name": "prod", "slug": "prod"}],
            "custom_fields": {"owner": "platform"}
        }))
        .unwrap();
        assert_eq!(vm.name, "web-01");
        assert_eq!(vm.status.unwrap().value, "active");
        assert_eq!(vm.role.unwrap().label(), "App Server");
        assert_eq!(vm.cluster.unwrap().label(), "prod");
        assert_eq!(vm.device.unwrap().label(), "hv-01");
        assert_eq!(vm.platform.unwrap().label(), "Ubuntu 22.04");
        assert_eq!(vm.site.unwrap().label(), "iad1");
        assert_eq!(vm.tenant.unwrap().label(), "Acme");
        assert_eq!(vm.primary_ip4.unwrap().label(), "10.0.0.5/24");
        assert_eq!(vm.primary_ip6.unwrap().label(), "2001:db8::5/64");
        assert_eq!(vm.vcpus, Some(4.0));
        assert_eq!(vm.memory, Some(8192));
        assert_eq!(vm.disk, Some(100));
        assert_eq!(vm.tags[0].slug, "prod");
    }

    #[test]
    fn cluster_deserializes() {
        let c: Cluster = serde_json::from_value(json!({
            "id": 3,
            "url": "http://nb/api/virtualization/clusters/3/",
            "name": "prod"
        }))
        .unwrap();
        assert_eq!(c.name, "prod");
        assert!(c.type_.is_none());
        assert!(c.scope.is_none());
    }

    #[test]
    fn cluster_full_deserializes_with_scope_and_counts() {
        let c: Cluster = serde_json::from_value(json!({
            "id": 3,
            "url": "http://nb/api/virtualization/clusters/3/",
            "display": "prod",
            "name": "prod",
            "type": {"id": 1, "url": "u", "display": "VMware", "name": "VMware", "slug": "vmware"},
            "group": {"id": 2, "url": "u", "display": "us-east", "name": "us-east", "slug": "us-east"},
            "status": {"value": "active", "label": "Active"},
            "tenant": {"id": 9, "url": "u", "display": "Acme", "name": "Acme", "slug": "acme"},
            "scope_type": "dcim.site",
            "scope_id": 1,
            "scope": {"id": 1, "url": "u", "display": "iad1", "name": "iad1", "slug": "iad1"},
            "description": "primary cluster",
            "device_count": 4,
            "virtualmachine_count": 0,
            "tags": [{"id": 1, "name": "prod", "slug": "prod"}],
            "custom_fields": {"sla": "gold"}
        }))
        .unwrap();
        assert_eq!(c.name, "prod");
        assert_eq!(c.type_.unwrap().label(), "VMware");
        assert_eq!(c.group.unwrap().label(), "us-east");
        assert_eq!(c.status.unwrap().value, "active");
        assert_eq!(c.tenant.unwrap().label(), "Acme");
        assert_eq!(c.scope_type.as_deref(), Some("dcim.site"));
        assert_eq!(c.scope.unwrap().label(), "iad1");
        assert_eq!(c.device_count, Some(4));
        assert_eq!(c.virtualmachine_count, Some(0));
        assert_eq!(c.tags[0].slug, "prod");
    }
}
