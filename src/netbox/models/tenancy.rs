//! Tenancy models: tenants and contacts.

use serde::{Deserialize, Serialize};

use super::common::{BriefObject, Tag};

/// A tenant (`/api/tenancy/tenants/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tenant {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,
    pub slug: String,

    #[serde(default)]
    pub group: Option<BriefObject>,
    #[serde(default)]
    pub description: Option<String>,

    // Cheap relation counts the serializer always reports (read-only).
    #[serde(default)]
    pub circuit_count: Option<u64>,
    #[serde(default)]
    pub device_count: Option<u64>,
    #[serde(default)]
    pub ipaddress_count: Option<u64>,
    #[serde(default)]
    pub prefix_count: Option<u64>,
    #[serde(default)]
    pub rack_count: Option<u64>,
    #[serde(default)]
    pub site_count: Option<u64>,
    #[serde(default)]
    pub vlan_count: Option<u64>,
    #[serde(default)]
    pub vrf_count: Option<u64>,
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

/// A contact (`/api/tenancy/contacts/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Contact {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,

    #[serde(default)]
    pub group: Option<BriefObject>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub link: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tenant_deserializes() {
        let t: Tenant = serde_json::from_value(json!({
            "id": 4,
            "url": "http://nb/api/tenancy/tenants/4/",
            "name": "corp",
            "slug": "corp"
        }))
        .unwrap();
        assert_eq!(t.name, "corp");
        assert_eq!(t.slug, "corp");
        assert!(t.description.is_none());
        assert!(t.group.is_none());
    }

    #[test]
    fn tenant_full_deserializes_with_group_and_counts() {
        let t: Tenant = serde_json::from_value(json!({
            "id": 4,
            "url": "http://nb/api/tenancy/tenants/4/",
            "display": "Acme Corp",
            "name": "Acme Corp",
            "slug": "acme",
            "group": {"id": 2, "url": "u", "display": "Customers", "name": "Customers", "slug": "customers"},
            "description": "primary customer",
            "device_count": 12,
            "prefix_count": 5,
            "site_count": 0,
            "tags": [{"id": 1, "name": "vip", "slug": "vip"}],
            "custom_fields": {"account_id": "A-100"}
        }))
        .unwrap();
        assert_eq!(t.name, "Acme Corp");
        assert_eq!(t.group.unwrap().label(), "Customers");
        assert_eq!(t.device_count, Some(12));
        assert_eq!(t.prefix_count, Some(5));
        assert_eq!(t.site_count, Some(0));
        assert_eq!(t.tags[0].slug, "vip");
    }

    #[test]
    fn contact_deserializes() {
        let c: Contact = serde_json::from_value(json!({
            "id": 7,
            "url": "http://nb/api/tenancy/contacts/7/",
            "name": "Jane Doe"
        }))
        .unwrap();
        assert_eq!(c.name, "Jane Doe");
        assert!(c.email.is_none());
        assert!(c.group.is_none());
    }

    #[test]
    fn contact_full_deserializes() {
        let c: Contact = serde_json::from_value(json!({
            "id": 7,
            "url": "http://nb/api/tenancy/contacts/7/",
            "display": "Jane Doe",
            "name": "Jane Doe",
            "group": {"id": 3, "url": "u", "display": "NOC", "name": "NOC", "slug": "noc"},
            "title": "Network Engineer",
            "phone": "+1-555-0100",
            "email": "jane@example.com",
            "address": "1 Main St",
            "link": "https://example.com/jane",
            "description": "on-call",
            "tags": [{"id": 2, "name": "oncall", "slug": "oncall"}],
            "custom_fields": {"pager": "555-9000"}
        }))
        .unwrap();
        assert_eq!(c.name, "Jane Doe");
        assert_eq!(c.group.unwrap().label(), "NOC");
        assert_eq!(c.title.as_deref(), Some("Network Engineer"));
        assert_eq!(c.phone.as_deref(), Some("+1-555-0100"));
        assert_eq!(c.email.as_deref(), Some("jane@example.com"));
        assert_eq!(c.link.as_deref(), Some("https://example.com/jane"));
        assert_eq!(c.tags[0].slug, "oncall");
    }
}
