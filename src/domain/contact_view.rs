//! Flattened contact view for `nbox contact` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::netbox::models::tenancy::Contact;
use crate::output::plain::KeyValues;

/// A contact, normalized to flat string fields for display.
#[derive(Debug, Clone, Serialize)]
pub struct ContactView {
    pub id: u64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl ContactView {
    /// Normalize a wire [`Contact`] into a flat view.
    pub fn from_model(c: Contact) -> Self {
        let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };
        Self {
            id: c.id,
            name: c.name,
            title: c.title.and_then(non_empty),
            phone: c.phone.and_then(non_empty),
            email: c.email.and_then(non_empty),
            address: c.address.and_then(non_empty),
            link: c.link.and_then(non_empty),
            group: c.group.map(|b| b.label()),
            description: c.description.and_then(non_empty),
            tags: c.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&c.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("name", self.name.clone())
            .push_opt("title", self.title.clone())
            .push_opt("phone", self.phone.clone())
            .push_opt("email", self.email.clone())
            .push_opt("address", self.address.clone())
            .push_opt("link", self.link.clone())
            .push_opt("group", self.group.clone())
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
    fn flattens_contact() {
        let c: Contact = serde_json::from_value(json!({
            "id": 7, "url": "u", "name": "Jane Doe",
            "group": {"id": 3, "display": "NOC"},
            "title": "Network Engineer",
            "phone": "+1-555-0100",
            "email": "jane@example.com",
            "address": "",
            "link": "https://example.com/jane",
            "tags": [{"id": 2, "name": "oncall", "slug": "oncall"}],
            "custom_fields": {"pager": "555-9000"}
        }))
        .unwrap();
        let view = ContactView::from_model(c);
        assert_eq!(view.name, "Jane Doe");
        assert_eq!(view.group.as_deref(), Some("NOC"));
        assert_eq!(view.title.as_deref(), Some("Network Engineer"));
        assert_eq!(view.email.as_deref(), Some("jane@example.com"));
        assert_eq!(view.link.as_deref(), Some("https://example.com/jane"));
        // Empty string normalized to None.
        assert_eq!(view.address, None);
        assert_eq!(view.tags, vec!["oncall"]);

        let plain = view.to_key_values().render();
        assert!(plain.starts_with("name: Jane Doe"));
        assert!(plain.contains("title: Network Engineer"));
        assert!(plain.contains("email: jane@example.com"));
        assert!(plain.contains("group: NOC"));
        assert!(plain.contains("tags: oncall"));
        assert!(plain.contains("cf.pager: 555-9000"));
        assert!(!plain.contains("address:"));
    }

    #[test]
    fn empty_optionals_dropped_in_json() {
        let c: Contact = serde_json::from_value(json!({
            "id": 7, "url": "u", "name": "bare"
        }))
        .unwrap();
        let view = ContactView::from_model(c);
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["name"], "bare");
        assert!(value.get("title").is_none());
        assert!(value.get("email").is_none());
        assert!(value.get("group").is_none());
        assert!(value.get("tags").is_none());
        assert!(value.get("custom_fields").is_none());
    }
}
