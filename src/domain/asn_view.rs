//! Flattened ASN view for `nbox asn` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::netbox::models::ipam::Asn;
use crate::output::plain::KeyValues;

/// An ASN, normalized to flat fields for display.
#[derive(Debug, Clone, Serialize)]
pub struct AsnView {
    pub asn: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl AsnView {
    /// Normalize a wire [`Asn`] into a flat view.
    pub fn from_model(a: Asn) -> Self {
        let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };
        Self {
            asn: a.asn,
            rir: a.rir.map(|b| b.label()),
            tenant: a.tenant.map(|b| b.label()),
            description: a.description.and_then(non_empty),
            custom_fields: custom::fields(&a.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("asn", self.asn.to_string())
            .push_opt("rir", self.rir.clone())
            .push_opt("tenant", self.tenant.clone())
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
    fn flattens_asn() {
        let asn: Asn = serde_json::from_value(json!({
            "id": 1, "url": "u", "asn": 64512,
            "rir": {"id": 1, "display": "RFC 6996"},
            "custom_fields": {}
        }))
        .unwrap();
        let view = AsnView::from_model(asn);
        assert_eq!(view.asn, 64512);
        assert_eq!(view.rir.as_deref(), Some("RFC 6996"));
        let plain = view.to_key_values().render();
        assert!(plain.starts_with("asn: 64512"));
    }
}
