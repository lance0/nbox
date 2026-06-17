//! Circuits models.

use serde::{Deserialize, Serialize};

use super::common::{BriefObject, Choice, Tag};

/// A circuit (`/api/circuits/circuits/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Circuit {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    /// The provider's circuit ID.
    pub cid: String,

    #[serde(default)]
    pub provider: Option<BriefObject>,
    #[serde(rename = "type", default)]
    pub type_: Option<BriefObject>,
    #[serde(default)]
    pub status: Option<Choice<String>>,
    #[serde(default)]
    pub tenant: Option<BriefObject>,

    #[serde(default)]
    pub install_date: Option<String>,
    /// Committed information rate, in kbps.
    #[serde(default)]
    pub commit_rate: Option<u64>,
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
    fn circuit_with_provider_and_type() {
        let c: Circuit = serde_json::from_value(json!({
            "id": 3,
            "url": "http://nb/api/circuits/circuits/3/",
            "cid": "ACME-1234",
            "provider": {"id": 1, "name": "ACME", "slug": "acme"},
            "type": {"id": 2, "name": "Internet", "slug": "internet"},
            "status": {"value": "active", "label": "Active"},
            "commit_rate": 1_000_000,
            "custom_fields": {}
        }))
        .unwrap();
        assert_eq!(c.cid, "ACME-1234");
        assert_eq!(c.provider.unwrap().label(), "ACME");
        assert_eq!(c.type_.unwrap().label(), "Internet");
        assert_eq!(c.status.unwrap().value, "active");
        assert_eq!(c.commit_rate, Some(1_000_000));
    }
}
