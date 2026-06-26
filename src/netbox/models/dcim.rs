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

    /// The native owner (NetBox 4.5+); a user/group brief. `None` on older
    /// releases or when unset.
    #[serde(default)]
    pub owner: Option<BriefObject>,
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
    pub mac_address: Option<String>,

    /// When the object was last modified (ISO 8601). Carried from every NetBox
    /// release on detail responses; the write foundation uses it as the
    /// pre-4.6 optimistic-concurrency precondition when no `ETag` header is
    /// present (ADR-0001 §3). `#[serde(default)]` so it is simply `None` when a
    /// caller fetches a partial list shape that omits it.
    #[serde(default)]
    pub last_updated: Option<String>,

    #[serde(default)]
    pub mode: Option<Choice<String>>,
    #[serde(default)]
    pub untagged_vlan: Option<BriefObject>,
    #[serde(default)]
    pub tagged_vlans: Vec<BriefObject>,

    #[serde(default)]
    pub cable: Option<BriefObject>,
    /// Far-end endpoints once a cable path is traced (may be absent/null).
    #[serde(default)]
    pub connected_endpoints: Option<Vec<BriefObject>>,

    /// The native owner (NetBox 4.5+); a user/group brief. `None` on older
    /// releases or when unset.
    #[serde(default)]
    pub owner: Option<BriefObject>,
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

    /// The native owner (NetBox 4.5+); a user/group brief. `None` on older
    /// releases or when unset.
    #[serde(default)]
    pub owner: Option<BriefObject>,
    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// A region (`/api/dcim/regions/`). Minimal: just enough to resolve a ref to an
/// id for prefix scope filtering. Kept permissive — only `id`/`name`/`slug` are
/// relied on; everything else NetBox sends is ignored.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Region {
    pub id: u64,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,
    pub slug: String,
}

/// A site group (`/api/dcim/site-groups/`). Minimal, permissive — see [`Region`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SiteGroup {
    pub id: u64,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,
    pub slug: String,
}

/// A location (`/api/dcim/locations/`). Minimal, permissive — see [`Region`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Location {
    pub id: u64,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,
    pub slug: String,
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

    /// The native owner (NetBox 4.5+); a user/group brief. `None` on older
    /// releases or when unset.
    #[serde(default)]
    pub owner: Option<BriefObject>,
    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub custom_fields: serde_json::Value,
}

/// A rack group (`/api/dcim/rack-groups/`). A hierarchical container for racks
/// within a site/location; nbox treats it as a flat lookup (the tree depth is in
/// the wire object, not surfaced). Carries an `owner` (4.5+) and a cheap
/// `rack_count` the serializer reports.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RackGroup {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,
    pub slug: String,
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
    pub rack_count: Option<u64>,
}

/// A MAC address (`/api/dcim/mac-addresses/`, NetBox 4.2+). Standalone objects
/// since 4.2 — an interface or VM interface can carry several, with a primary
/// designation. The reverse-resolve query (`nbox mac <addr>`) is a top
/// operator/agent ask (which device owns this MAC?).
///
/// `assigned_object` is polymorphic: a `dcim.interface` (carries a `device`) or
/// a `virtualization.vminterface` (carries a `virtual_machine`). It's left as
/// raw JSON and labeled by the view layer (see `mac_view::assigned_label`),
/// mirroring how `IpAddress` handles its `assigned_object`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MacAddress {
    pub id: u64,
    pub url: String,
    /// NetBox's `display` (the MAC +, when assigned, the interface).
    #[serde(default)]
    pub display: Option<String>,
    /// The MAC in NetBox's canonical form (e.g. `aa:bb:cc:dd:ee:ff`).
    pub mac_address: String,
    /// The dotted content type of the assigned object (`dcim.interface`,
    /// `virtualization.vminterface`, …), or `None` for an unassigned MAC.
    #[serde(default)]
    pub assigned_object_type: Option<String>,
    /// The id of the assigned object, or `None` for an unassigned MAC.
    #[serde(default)]
    pub assigned_object_id: Option<u64>,
    /// The assigned interface/VM-interface brief (polymorphic; labeled in the
    /// view). `None` when the MAC is unassigned.
    #[serde(default)]
    pub assigned_object: Option<serde_json::Value>,
    #[serde(default)]
    pub description: Option<String>,
    /// The owner (a user/group, NetBox 4.5+); same shape as a brief object.
    #[serde(default)]
    pub owner: Option<BriefObject>,
    #[serde(default)]
    pub comments: Option<String>,
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
    fn scope_models_parse_minimally() {
        // Only id/name/slug are relied on; url/display are optional. A real NetBox
        // payload carries far more fields — they're ignored, not rejected.
        let region: Region = serde_json::from_value(json!({
            "id": 3, "url": "http://nb/api/dcim/regions/3/",
            "display": "US East", "name": "US East", "slug": "us-east",
            "parent": null, "description": "extra ignored field"
        }))
        .unwrap();
        assert_eq!(region.id, 3);
        assert_eq!(region.slug, "us-east");

        let group: SiteGroup =
            serde_json::from_value(json!({"id": 4, "name": "Campus", "slug": "campus"})).unwrap();
        assert_eq!(group.id, 4);
        assert_eq!(group.slug, "campus");

        let loc: Location =
            serde_json::from_value(json!({"id": 5, "name": "Row A", "slug": "row-a"})).unwrap();
        assert_eq!(loc.id, 5);
        assert_eq!(loc.name, "Row A");
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
