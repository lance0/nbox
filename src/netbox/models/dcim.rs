//! DCIM models: devices, interfaces, sites, racks.

use serde::{Deserialize, Serialize};

use super::common::{BriefObject, Choice, Tag};

/// A device (`/api/dcim/devices/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Device {
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
    pub device_type: Option<BriefObject>,
    #[serde(default)]
    pub platform: Option<BriefObject>,
    #[serde(default)]
    pub site: Option<BriefObject>,
    #[serde(default)]
    pub rack: Option<BriefObject>,
    #[serde(default)]
    pub tenant: Option<BriefObject>,
    #[serde(default)]
    pub primary_ip4: Option<BriefObject>,
    #[serde(default)]
    pub primary_ip6: Option<BriefObject>,

    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub asset_tag: Option<String>,
    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// An interface (`/api/dcim/interfaces/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Interface {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,

    #[serde(default)]
    pub device: Option<BriefObject>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(rename = "type", default)]
    pub type_: Option<Choice<String>>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub mtu: Option<u32>,

    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// A site (`/api/dcim/sites/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Site {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,
    pub slug: String,

    #[serde(default)]
    pub status: Option<Choice<String>>,
    #[serde(default)]
    pub region: Option<BriefObject>,
    #[serde(default)]
    pub group: Option<BriefObject>,
    #[serde(default)]
    pub tenant: Option<BriefObject>,
    #[serde(default)]
    pub facility: Option<String>,
    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// A rack (`/api/dcim/racks/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Rack {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,

    #[serde(default)]
    pub status: Option<Choice<String>>,
    #[serde(default)]
    pub site: Option<BriefObject>,
    #[serde(default)]
    pub location: Option<BriefObject>,
    #[serde(default)]
    pub tenant: Option<BriefObject>,
    #[serde(default)]
    pub role: Option<BriefObject>,
    #[serde(default)]
    pub u_height: Option<u32>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub asset_tag: Option<String>,
    #[serde(default)]
    pub description: Option<String>,

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
    fn device_with_brief_relations() {
        let d: Device = serde_json::from_value(json!({
            "id": 123,
            "url": "http://nb/api/dcim/devices/123/",
            "name": "edge01",
            "status": {"value": "active", "label": "Active"},
            "site": {"id": 1, "name": "iad1", "slug": "iad1", "display": "iad1"},
            "primary_ip4": {"id": 9, "display": "10.44.12.9/32", "address": "10.44.12.9/32"},
            "tags": [],
            "custom_fields": {}
        }))
        .unwrap();

        assert_eq!(d.name, "edge01");
        assert_eq!(d.status.unwrap().value, "active");
        assert_eq!(d.site.unwrap().label(), "iad1");
        assert_eq!(d.primary_ip4.unwrap().label(), "10.44.12.9/32");
        assert!(d.rack.is_none());
    }

    #[test]
    fn interface_type_field_is_renamed() {
        let i: Interface = serde_json::from_value(json!({
            "id": 42,
            "url": "http://nb/api/dcim/interfaces/42/",
            "name": "xe-0/0/1",
            "enabled": true,
            "type": {"value": "10gbase-x-sfpp", "label": "SFP+ (10GE)"}
        }))
        .unwrap();
        assert_eq!(i.name, "xe-0/0/1");
        assert_eq!(i.enabled, Some(true));
        assert_eq!(i.type_.unwrap().value, "10gbase-x-sfpp");
    }
}
