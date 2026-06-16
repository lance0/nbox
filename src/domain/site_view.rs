//! Flattened site view for `nbx site` (plain + JSON).

use serde::Serialize;

use crate::netbox::models::dcim::Site;
use crate::output::plain::KeyValues;

/// A site, normalized to flat string fields.
#[derive(Debug, Clone, Serialize)]
pub struct SiteView {
    pub id: u64,
    pub name: String,
    pub slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facility: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl SiteView {
    /// Normalize a wire [`Site`].
    pub fn from_model(s: Site) -> Self {
        let non_empty = |x: String| if x.is_empty() { None } else { Some(x) };
        Self {
            id: s.id,
            name: s.name,
            slug: s.slug,
            status: s.status.map(|c| c.value),
            region: s.region.map(|b| b.label()),
            group: s.group.map(|b| b.label()),
            tenant: s.tenant.map(|b| b.label()),
            facility: s.facility.and_then(non_empty),
            description: s.description.and_then(non_empty),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("name", self.name.clone())
            .push("slug", self.slug.clone())
            .push_opt("status", self.status.clone())
            .push_opt("region", self.region.clone())
            .push_opt("group", self.group.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("facility", self.facility.clone())
            .push_opt("description", self.description.clone());
        kv
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_site() {
        let s: Site = serde_json::from_value(json!({
            "id": 1, "url": "u", "name": "iad1", "slug": "iad1",
            "status": {"value": "active", "label": "Active"},
            "region": {"id": 2, "display": "us-east"},
            "facility": ""
        }))
        .unwrap();
        let view = SiteView::from_model(s);
        assert_eq!(view.status.as_deref(), Some("active"));
        assert_eq!(view.region.as_deref(), Some("us-east"));
        assert_eq!(view.facility, None);
        assert!(
            view.to_key_values()
                .render()
                .starts_with("name: iad1\nslug: iad1")
        );
    }
}
