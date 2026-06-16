//! Tenancy models: tenants.

use serde::{Deserialize, Serialize};

use super::common::Tag;

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
    }
}
