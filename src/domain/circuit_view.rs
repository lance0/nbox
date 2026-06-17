//! Flattened circuit view for `nbox circuit` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::netbox::models::circuits::Circuit;
use crate::output::plain::KeyValues;

/// A circuit, normalized to flat string fields for display.
#[derive(Debug, Clone, Serialize)]
pub struct CircuitView {
    pub cid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_rate_kbps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl CircuitView {
    /// Normalize a wire [`Circuit`] into a flat view.
    pub fn from_model(c: Circuit) -> Self {
        let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };
        Self {
            cid: c.cid,
            provider: c.provider.map(|b| b.label()),
            type_: c.type_.map(|b| b.label()),
            status: c.status.map(|c| c.value),
            tenant: c.tenant.map(|b| b.label()),
            install_date: c.install_date.and_then(non_empty),
            commit_rate_kbps: c.commit_rate,
            description: c.description.and_then(non_empty),
            custom_fields: custom::fields(&c.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("cid", self.cid.clone())
            .push_opt("provider", self.provider.clone())
            .push_opt("type", self.type_.clone())
            .push_opt("status", self.status.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("install_date", self.install_date.clone())
            .push_opt(
                "commit_rate_kbps",
                self.commit_rate_kbps.map(|r| r.to_string()),
            )
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
    fn flattens_circuit() {
        let circuit: Circuit = serde_json::from_value(json!({
            "id": 3, "url": "u", "cid": "ACME-1234",
            "provider": {"id": 1, "display": "ACME"},
            "type": {"id": 2, "display": "Internet"},
            "status": {"value": "active", "label": "Active"},
            "commit_rate": 1_000_000,
            "custom_fields": {}
        }))
        .unwrap();
        let view = CircuitView::from_model(circuit);
        assert_eq!(view.cid, "ACME-1234");
        assert_eq!(view.provider.as_deref(), Some("ACME"));
        assert_eq!(view.type_.as_deref(), Some("Internet"));
        assert_eq!(view.commit_rate_kbps, Some(1_000_000));

        let plain = view.to_key_values().render();
        assert!(plain.starts_with("cid: ACME-1234"));
        assert!(plain.contains("provider: ACME"));
        assert!(plain.contains("commit_rate_kbps: 1000000"));
    }
}
