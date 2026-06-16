//! Flattened aggregate view for `nbox aggregate` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::netbox::models::ipam::Aggregate;
use crate::output::plain::KeyValues;

/// An aggregate, normalized to flat string fields for display.
#[derive(Debug, Clone, Serialize)]
pub struct AggregateView {
    pub prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_added: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl AggregateView {
    /// Normalize a wire [`Aggregate`] into a flat view.
    pub fn from_model(a: Aggregate) -> Self {
        let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };
        Self {
            prefix: a.prefix,
            rir: a.rir.map(|b| b.label()),
            tenant: a.tenant.map(|b| b.label()),
            date_added: a.date_added.and_then(non_empty),
            description: a.description.and_then(non_empty),
            custom_fields: custom::fields(&a.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("prefix", self.prefix.clone())
            .push_opt("rir", self.rir.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("date_added", self.date_added.clone())
            .push_opt("description", self.description.clone());
        custom::append(&mut kv, &self.custom_fields);
        kv
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_aggregate() {
        let agg: Aggregate = serde_json::from_value(json!({
            "id": 1, "url": "u", "prefix": "10.0.0.0/8",
            "rir": {"id": 1, "display": "RFC 1918"},
            "custom_fields": {}
        }))
        .unwrap();
        let view = AggregateView::from_model(agg);
        assert_eq!(view.prefix, "10.0.0.0/8");
        assert_eq!(view.rir.as_deref(), Some("RFC 1918"));
        let plain = view.to_key_values().render();
        assert!(plain.starts_with("prefix: 10.0.0.0/8"));
        assert!(plain.contains("rir: RFC 1918"));
    }
}
