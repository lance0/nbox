//! IPAM models: IP addresses, prefixes, VLANs, VRFs.

use serde::{Deserialize, Serialize};

use super::common::{BriefObject, Choice, Tag};

/// An IP address (`/api/ipam/ip-addresses/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IpAddress {
    pub id: u64,
    pub url: String,
    pub address: String,

    #[serde(default)]
    pub status: Option<Choice<String>>,
    #[serde(default)]
    pub role: Option<Choice<String>>,
    #[serde(default)]
    pub vrf: Option<BriefObject>,
    #[serde(default)]
    pub tenant: Option<BriefObject>,

    #[serde(default)]
    pub assigned_object_type: Option<String>,
    #[serde(default)]
    pub assigned_object_id: Option<u64>,
    #[serde(default)]
    pub assigned_object: Option<serde_json::Value>,

    #[serde(default)]
    pub dns_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// A prefix (`/api/ipam/prefixes/`).
///
/// NetBox 4.2+ uses a polymorphic `scope` (`scope_type`/`scope_id`/`scope`) in
/// place of the old `site` field; we target 4.2+, so there is no legacy fallback.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Prefix {
    pub id: u64,
    pub url: String,
    pub prefix: String,

    #[serde(default)]
    pub status: Option<Choice<String>>,
    #[serde(default)]
    pub vrf: Option<BriefObject>,
    #[serde(default)]
    pub tenant: Option<BriefObject>,
    #[serde(default)]
    pub vlan: Option<BriefObject>,
    #[serde(default)]
    pub role: Option<BriefObject>,

    #[serde(default)]
    pub scope_type: Option<String>,
    #[serde(default)]
    pub scope_id: Option<u64>,
    #[serde(default)]
    pub scope: Option<BriefObject>,

    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub children: Option<u64>,
    /// Prefix utilization, when the NetBox version/serializer provides it.
    /// Kept as a permissive value (number or string) so absence never breaks
    /// deserialization; the view coerces it to a percentage when numeric.
    #[serde(default)]
    pub utilization: Option<serde_json::Value>,

    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// A VLAN (`/api/ipam/vlans/`).
///
/// A VLAN's location comes from its `group` (a scoped VLAN group) or, when
/// assigned directly, a `site`. NetBox versions that surface a polymorphic
/// `scope` (`scope_type`/`scope_id`/`scope`) on the VLAN itself are honored too;
/// all three are optional, so the common `site`/`group` shape still deserializes.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Vlan {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub vid: u16,
    pub name: String,

    #[serde(default)]
    pub status: Option<Choice<String>>,
    #[serde(default)]
    pub site: Option<BriefObject>,
    #[serde(default)]
    pub group: Option<BriefObject>,

    #[serde(default)]
    pub scope_type: Option<String>,
    #[serde(default)]
    pub scope_id: Option<u64>,
    #[serde(default)]
    pub scope: Option<BriefObject>,
    #[serde(default)]
    pub tenant: Option<BriefObject>,
    #[serde(default)]
    pub role: Option<BriefObject>,

    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// A VLAN group (`/api/ipam/vlan-groups/`).
///
/// Unlike a VLAN, a VLAN group IS polymorphically scoped: it carries
/// `scope_type`/`scope_id`/`scope`. The VLAN serializer's nested `group` brief
/// omits that scope, so surfacing a VLAN's group scope needs a follow-up fetch
/// of the group by id. Permissive — only the identity + scope fields are read.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VlanGroup {
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,

    #[serde(default)]
    pub scope_type: Option<String>,
    #[serde(default)]
    pub scope_id: Option<u64>,
    #[serde(default)]
    pub scope: Option<BriefObject>,
}

/// A VRF (`/api/ipam/vrfs/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Vrf {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,

    #[serde(default)]
    pub rd: Option<String>,
    #[serde(default)]
    pub tenant: Option<BriefObject>,
    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// An IP range (`/api/ipam/ip-ranges/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IpRange {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub start_address: String,
    pub end_address: String,

    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub status: Option<Choice<String>>,
    #[serde(default)]
    pub vrf: Option<BriefObject>,
    #[serde(default)]
    pub tenant: Option<BriefObject>,
    #[serde(default)]
    pub role: Option<BriefObject>,
    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// A service (`/api/ipam/services/`) — a port/protocol on a device or VM.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Service {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,

    #[serde(default)]
    pub protocol: Option<Choice<String>>,
    #[serde(default)]
    pub ports: Vec<u32>,
    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// An aggregate (`/api/ipam/aggregates/`) — a top-level allocation from a RIR.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Aggregate {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub prefix: String,

    #[serde(default)]
    pub rir: Option<BriefObject>,
    #[serde(default)]
    pub tenant: Option<BriefObject>,
    #[serde(default)]
    pub date_added: Option<String>,
    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// An ASN (`/api/ipam/asns/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Asn {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    /// The AS number (supports 32-bit ASNs).
    pub asn: u32,

    #[serde(default)]
    pub rir: Option<BriefObject>,
    #[serde(default)]
    pub tenant: Option<BriefObject>,
    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// An available IP within a prefix (`…/available-ips/`). NetBox returns a bare
/// array of these; only `address` is needed (other fields are ignored).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AvailableIp {
    pub address: String,
}

/// An available (free) child prefix within a prefix (`…/available-prefixes/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AvailablePrefix {
    pub prefix: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ip_address_with_assignment() {
        let ip: IpAddress = serde_json::from_value(json!({
            "id": 7,
            "url": "http://nb/api/ipam/ip-addresses/7/",
            "address": "10.44.208.55/24",
            "status": {"value": "active", "label": "Active"},
            "assigned_object_type": "dcim.interface",
            "assigned_object_id": 42,
            "dns_name": "printer-55.example.com",
            "custom_fields": {}
        }))
        .unwrap();
        assert_eq!(ip.address, "10.44.208.55/24");
        assert_eq!(ip.assigned_object_type.as_deref(), Some("dcim.interface"));
        assert_eq!(ip.assigned_object_id, Some(42));
        assert_eq!(ip.dns_name.as_deref(), Some("printer-55.example.com"));
    }

    #[test]
    fn prefix_with_polymorphic_scope() {
        let p: Prefix = serde_json::from_value(json!({
            "id": 5,
            "url": "http://nb/api/ipam/prefixes/5/",
            "prefix": "10.44.208.0/24",
            "status": {"value": "active", "label": "Active"},
            "scope_type": "dcim.site",
            "scope_id": 1,
            "scope": {"id": 1, "name": "iad1", "display": "iad1"},
            "children": 4
        }))
        .unwrap();
        assert_eq!(p.prefix, "10.44.208.0/24");
        assert_eq!(p.scope_type.as_deref(), Some("dcim.site"));
        assert_eq!(p.scope.unwrap().label(), "iad1");
        assert_eq!(p.children, Some(4));
    }

    #[test]
    fn vlan_minimal() {
        let v: Vlan = serde_json::from_value(json!({
            "id": 3,
            "url": "http://nb/api/ipam/vlans/3/",
            "vid": 208,
            "name": "users",
            "status": {"value": "active", "label": "Active"}
        }))
        .unwrap();
        assert_eq!(v.vid, 208);
        assert_eq!(v.name, "users");
    }
}
