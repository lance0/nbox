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

/// One end (A or Z) of a circuit (`/api/circuits/circuit-terminations/`). The
/// endpoint it lands on is the polymorphic `termination` (a site or a provider
/// network, distinguished by `termination_type`); the physical hand-off is the
/// `cable` and its `link_peers` (the device port it's patched into, if any).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CircuitTermination {
    pub id: u64,
    /// `"A"` or `"Z"`.
    #[serde(default)]
    pub term_side: Option<String>,
    /// The endpoint: a `dcim.site` or a `circuits.providernetwork` brief.
    #[serde(default)]
    pub termination: Option<BriefObject>,
    /// The endpoint's content type, e.g. `"dcim.site"` / `"circuits.providernetwork"`.
    #[serde(default)]
    pub termination_type: Option<String>,
    /// Port speed in kbps (`None` when unset).
    #[serde(default)]
    pub port_speed: Option<u64>,
    /// Upstream speed in kbps (`None` when unset).
    #[serde(default)]
    pub upstream_speed: Option<u64>,
    #[serde(default)]
    pub xconnect_id: Option<String>,
    /// Patch-panel / port info.
    #[serde(default)]
    pub pp_info: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// The cable from this termination (carries its id for the diagram).
    #[serde(default)]
    pub cable: Option<BriefObject>,
    /// What the cable connects to — a device port brief (`device` + port name).
    #[serde(default)]
    pub link_peers: Vec<BriefObject>,
}

/// A nested ASN as it appears in a provider's `asns` array. The serializer
/// returns the full ASN object; we keep only the AS number for the brief list.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderAsn {
    pub id: u64,
    /// The AS number (supports 32-bit ASNs).
    pub asn: u32,
}

/// A nested provider account as it appears in a provider's `accounts` array.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderAccount {
    pub id: u64,
    #[serde(default)]
    pub display: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// The account identifier.
    #[serde(default)]
    pub account: Option<String>,
}

impl ProviderAccount {
    /// Best available label for an account: the account id, else its name,
    /// else `display`, else `#id`.
    pub fn label(&self) -> String {
        self.account
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| self.name.clone().filter(|s| !s.is_empty()))
            .or_else(|| self.display.clone().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| format!("#{}", self.id))
    }
}

/// A provider (`/api/circuits/providers/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Provider {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub display: Option<String>,
    pub name: String,
    pub slug: String,

    #[serde(default)]
    pub accounts: Vec<ProviderAccount>,
    #[serde(default)]
    pub asns: Vec<ProviderAsn>,
    #[serde(default)]
    pub description: Option<String>,

    // Cheap relation count the serializer always reports (read-only).
    #[serde(default)]
    pub circuit_count: Option<u64>,

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

    #[test]
    fn provider_bare_deserializes() {
        let p: Provider = serde_json::from_value(json!({
            "id": 1,
            "url": "http://nb/api/circuits/providers/1/",
            "name": "ACME Telecom",
            "slug": "acme-telecom"
        }))
        .unwrap();
        assert_eq!(p.name, "ACME Telecom");
        assert_eq!(p.slug, "acme-telecom");
        assert!(p.accounts.is_empty());
        assert!(p.asns.is_empty());
        assert!(p.description.is_none());
        assert!(p.circuit_count.is_none());
    }

    #[test]
    fn provider_full_deserializes_with_asns_accounts_and_count() {
        let p: Provider = serde_json::from_value(json!({
            "id": 1,
            "url": "http://nb/api/circuits/providers/1/",
            "display": "ACME Telecom",
            "name": "ACME Telecom",
            "slug": "acme-telecom",
            "accounts": [
                {"id": 3, "display": "ACME-001 (primary)", "name": "primary", "account": "ACME-001"}
            ],
            "asns": [
                {"id": 5, "url": "u", "asn": 64512, "display": "AS64512"},
                {"id": 6, "url": "u", "asn": 64513, "display": "AS64513"}
            ],
            "description": "upstream transit",
            "circuit_count": 7,
            "tags": [{"id": 1, "name": "transit", "slug": "transit"}],
            "custom_fields": {"noc_email": "noc@acme.example"}
        }))
        .unwrap();
        assert_eq!(p.name, "ACME Telecom");
        assert_eq!(p.asns.len(), 2);
        assert_eq!(p.asns[0].asn, 64512);
        assert_eq!(p.accounts.len(), 1);
        assert_eq!(p.accounts[0].label(), "ACME-001");
        assert_eq!(p.circuit_count, Some(7));
        assert_eq!(p.tags[0].slug, "transit");
    }

    #[test]
    fn provider_account_label_falls_back() {
        let by_name: ProviderAccount =
            serde_json::from_value(json!({"id": 1, "name": "main", "account": ""})).unwrap();
        assert_eq!(by_name.label(), "main");
        let bare: ProviderAccount = serde_json::from_value(json!({"id": 9})).unwrap();
        assert_eq!(bare.label(), "#9");
    }
}
