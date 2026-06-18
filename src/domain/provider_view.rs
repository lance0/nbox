//! Flattened provider view for `nbox provider` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::{non_empty, non_zero};
use crate::netbox::models::circuits::{Provider, ProviderAccount};
use crate::output::plain::KeyValues;

/// A provider, normalized to flat string fields for display.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderView {
    pub id: u64,
    pub name: String,
    pub slug: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub asns: Vec<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    // Relation count the serializer reports — only surfaced when non-zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub circuit_count: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl ProviderView {
    /// Normalize a wire [`Provider`] into a flat view.
    pub fn from_model(p: Provider) -> Self {
        Self {
            id: p.id,
            name: p.name,
            slug: p.slug,
            asns: p.asns.into_iter().map(|a| a.asn).collect(),
            accounts: p.accounts.iter().map(ProviderAccount::label).collect(),
            description: p.description.and_then(non_empty),
            circuit_count: non_zero(p.circuit_count),
            tags: p.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&p.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("name", self.name.clone())
            .push("slug", self.slug.clone());
        if !self.asns.is_empty() {
            kv.push(
                "asns",
                self.asns
                    .iter()
                    .map(|a| format!("AS{a}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        if !self.accounts.is_empty() {
            kv.push("accounts", self.accounts.join(", "));
        }
        kv.push_opt("description", self.description.clone())
            .push_opt("circuit_count", self.circuit_count.map(|n| n.to_string()));
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
    fn flattens_provider() {
        let p: Provider = serde_json::from_value(json!({
            "id": 1, "url": "u", "name": "ACME Telecom", "slug": "acme-telecom",
            "asns": [
                {"id": 5, "url": "u", "asn": 64512},
                {"id": 6, "url": "u", "asn": 64513}
            ],
            "accounts": [
                {"id": 3, "display": "ACME-001", "name": "primary", "account": "ACME-001"}
            ],
            "description": "upstream transit",
            "circuit_count": 7,
            "tags": [{"id": 1, "name": "transit", "slug": "transit"}],
            "custom_fields": {"noc_email": "noc@acme.example"}
        }))
        .unwrap();
        let view = ProviderView::from_model(p);
        assert_eq!(view.name, "ACME Telecom");
        assert_eq!(view.slug, "acme-telecom");
        assert_eq!(view.asns, vec![64512, 64513]);
        assert_eq!(view.accounts, vec!["ACME-001"]);
        assert_eq!(view.circuit_count, Some(7));
        assert_eq!(view.tags, vec!["transit"]);

        let plain = view.to_key_values().render();
        assert!(plain.starts_with("name: ACME Telecom\nslug: acme-telecom"));
        assert!(plain.contains("asns: AS64512, AS64513"));
        assert!(plain.contains("accounts: ACME-001"));
        assert!(plain.contains("circuit_count: 7"));
        assert!(plain.contains("tags: transit"));
        assert!(plain.contains("cf.noc_email: noc@acme.example"));
    }

    #[test]
    fn empty_optionals_dropped_in_json() {
        let p: Provider = serde_json::from_value(json!({
            "id": 1, "url": "u", "name": "bare", "slug": "bare",
            "description": "",
            "circuit_count": 0
        }))
        .unwrap();
        let view = ProviderView::from_model(p);
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["name"], "bare");
        assert!(value.get("asns").is_none());
        assert!(value.get("accounts").is_none());
        assert!(value.get("description").is_none());
        // Zero circuit_count is dropped.
        assert!(value.get("circuit_count").is_none());
        assert!(value.get("tags").is_none());
        assert!(value.get("custom_fields").is_none());
    }
}
