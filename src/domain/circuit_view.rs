//! Flattened circuit view for `nbox circuit` (plain + JSON), with its A/Z
//! terminations and an A↔Z path diagram.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::non_empty;
use crate::netbox::models::circuits::{Circuit, CircuitTermination};
use crate::netbox::models::common::BriefObject;
use crate::output::plain::KeyValues;

/// One end (A or Z) of a circuit, normalized for display.
#[derive(Debug, Clone, Serialize)]
pub struct CircuitTerminationView {
    /// `"A"` or `"Z"` (or `"?"` if the side is missing).
    pub side: String,
    /// The endpoint it lands on (a site or provider network), by label.
    pub endpoint: String,
    /// `"site"` / `"provider network"` — the endpoint's kind.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub endpoint_kind: String,
    /// The device port it's patched into (`device port`), when cabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_to: Option<String>,
    /// The connecting cable's label (e.g. `#2378128`), when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xconnect_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pp_info: Option<String>,
    /// Port speed, humanized (e.g. `10 Gbps`), when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port_speed: Option<String>,
}

impl CircuitTerminationView {
    fn from_model(t: CircuitTermination) -> Self {
        let endpoint = t
            .termination
            .as_ref()
            .map_or_else(|| "(unterminated)".to_string(), BriefObject::label);
        let connected_to = t.link_peers.first().map(BriefObject::endpoint_label);
        Self {
            side: t.term_side.unwrap_or_else(|| "?".to_string()),
            endpoint,
            endpoint_kind: endpoint_kind(t.termination_type.as_deref()),
            connected_to,
            cable: t.cable.as_ref().map(BriefObject::label),
            xconnect_id: t.xconnect_id.and_then(non_empty),
            pp_info: t.pp_info.and_then(non_empty),
            port_speed: t.port_speed.map(humanize_kbps),
        }
    }
}

/// A circuit, normalized to flat string fields for display, plus its A/Z
/// terminations and an A↔Z path diagram.
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
    /// The A/Z terminations (A first), each with its endpoint + physical patch.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub terminations: Vec<CircuitTerminationView>,
    /// The circuit path as a multi-line ASCII A↔Z diagram, derived from the
    /// terminations. Not serialized — it's a rendering for the plain/TUI surfaces,
    /// while the structured `terminations` remain the machine-readable form.
    #[serde(skip)]
    pub diagram: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl CircuitView {
    /// Normalize a wire [`Circuit`] without its terminations (attributes only).
    pub fn from_model(c: Circuit) -> Self {
        Self::build(c, Vec::new())
    }

    /// Normalize a [`Circuit`] plus its A/Z [`CircuitTermination`]s, building the
    /// flat attributes, the structured terminations (A before Z), and the A↔Z
    /// path diagram.
    pub fn build(c: Circuit, terminations: Vec<CircuitTermination>) -> Self {
        let type_ = c.type_.map(|b| b.label());
        let status = c.status.map(|c| c.value);
        let commit_rate_kbps = c.commit_rate;

        // Order A before Z (the API doesn't guarantee order); unknown sides last.
        let mut terms: Vec<CircuitTerminationView> = terminations
            .into_iter()
            .map(CircuitTerminationView::from_model)
            .collect();
        terms.sort_by(|a, b| a.side.cmp(&b.side));

        let diagram = format_circuit_diagram(
            &circuit_segment(type_.as_deref(), commit_rate_kbps, status.as_deref()),
            &terms,
        );

        Self {
            cid: c.cid,
            provider: c.provider.map(|b| b.label()),
            type_,
            status,
            tenant: c.tenant.map(|b| b.label()),
            install_date: c.install_date.and_then(non_empty),
            commit_rate_kbps,
            description: c.description.and_then(non_empty),
            terminations: terms,
            diagram,
            tags: c.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&c.custom_fields),
        }
    }

    /// The circuit's attribute key-values (no path).
    fn attributes(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("cid", self.cid.clone())
            .push_opt("provider", self.provider.clone())
            .push_opt("type", self.type_.clone())
            .push_opt("status", self.status.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("install_date", self.install_date.clone())
            .push_opt("commit_rate", self.commit_rate_kbps.map(humanize_kbps))
            .push_opt("description", self.description.clone());
        if !self.tags.is_empty() {
            kv.push("tags", self.tags.join(", "));
        }
        custom::append(&mut kv, &self.custom_fields);
        kv
    }

    /// Render as `key: value` lines (attributes only) — the TUI detail body and
    /// the back-compat plain renderer for callers that don't want the path.
    pub fn to_key_values(&self) -> KeyValues {
        self.attributes()
    }

    /// The full CLI `nbox circuit` output: attributes followed by the A↔Z path.
    pub fn to_plain(&self) -> String {
        let mut out = self.attributes().render();
        out.push_str(&section("Circuit Path", &self.diagram));
        out
    }
}

/// A `\n\nTitle\n  item` block, or empty when there are no items.
fn section(title: &str, items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = items.iter().map(|i| format!("  {i}")).collect();
    format!("\n\n{title}\n{}", lines.join("\n"))
}

/// The circuit's mid-segment descriptor: `type · rate · status` (present parts).
fn circuit_segment(
    type_: Option<&str>,
    commit_rate_kbps: Option<u64>,
    status: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = type_ {
        parts.push(t.to_string());
    }
    if let Some(r) = commit_rate_kbps {
        parts.push(humanize_kbps(r));
    }
    if let Some(s) = status {
        parts.push(title_first(s));
    }
    if parts.is_empty() {
        "circuit".to_string()
    } else {
        parts.join(" · ")
    }
}

/// Draw the circuit as a vertical A↔Z diagram: the A termination on top, the Z on
/// the bottom, the circuit segment (`type · rate · status`) between them. Each
/// termination shows its endpoint (site / provider network) and, when cabled, the
/// device port it's patched into.
///
/// ```text
///  A  US-CHI02  (site)
///     ↳ 355.M03.01.02.PNL.01 13  ·  #2378128
///     │
///     ┿ Direct Connect · 400 Gbps · Active
///     │
///  Z  314BCE DX  (provider network)
/// ```
fn format_circuit_diagram(segment: &str, terms: &[CircuitTerminationView]) -> Vec<String> {
    if terms.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let last = terms.len() - 1;
    for (i, t) in terms.iter().enumerate() {
        let kind = if t.endpoint_kind.is_empty() {
            String::new()
        } else {
            format!("  ({})", t.endpoint_kind)
        };
        lines.push(format!(" {}  {}{kind}", t.side, t.endpoint));
        if let Some(conn) = &t.connected_to {
            let cable = t
                .cable
                .as_deref()
                .map(|c| format!("  ·  {c}"))
                .unwrap_or_default();
            lines.push(format!("    ↳ {conn}{cable}"));
        }
        // Cross-connect / patch-panel detail, when present.
        let mut xparts: Vec<String> = Vec::new();
        if let Some(x) = &t.xconnect_id {
            xparts.push(format!("xconnect {x}"));
        }
        if let Some(pp) = &t.pp_info {
            xparts.push(format!("pp {pp}"));
        }
        if !xparts.is_empty() {
            lines.push(format!("    {}", xparts.join(" · ")));
        }
        if i < last {
            lines.push("    │".to_string());
            lines.push(format!("    ┿ {segment}"));
            lines.push("    │".to_string());
        }
    }
    lines
}

/// Map a termination's content type to a short kind label.
fn endpoint_kind(termination_type: Option<&str>) -> String {
    match termination_type {
        Some("dcim.site") => "site".to_string(),
        Some("circuits.providernetwork") => "provider network".to_string(),
        Some(other) => other.rsplit('.').next().unwrap_or(other).replace('_', " "),
        None => String::new(),
    }
}

/// Humanize a kbps speed: `400000000 → "400 Gbps"`, `500000 → "500 Mbps"`. Falls
/// back to one decimal for non-round values, and to bare kbps below 1 Mbps.
fn humanize_kbps(kbps: u64) -> String {
    if kbps == 0 {
        return "0".to_string();
    }
    if kbps.is_multiple_of(1_000_000) {
        format!("{} Gbps", kbps / 1_000_000)
    } else if kbps >= 1_000_000 {
        format!("{:.1} Gbps", kbps as f64 / 1_000_000.0)
    } else if kbps.is_multiple_of(1000) {
        format!("{} Mbps", kbps / 1000)
    } else if kbps >= 1000 {
        format!("{:.1} Mbps", kbps as f64 / 1000.0)
    } else {
        format!("{kbps} kbps")
    }
}

/// Uppercase the first character of `s` (idempotent), so a bare status value
/// (`active`) reads as `Active`.
fn title_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn circuit(value: Value) -> Circuit {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn flattens_circuit() {
        let c = circuit(json!({
            "id": 3, "url": "u", "cid": "ACME-1234",
            "provider": {"id": 1, "display": "ACME"},
            "type": {"id": 2, "display": "Internet"},
            "status": {"value": "active", "label": "Active"},
            "commit_rate": 1_000_000,
            "tags": [{"id": 1, "name": "transit", "slug": "transit"}],
            "custom_fields": {}
        }));
        let view = CircuitView::from_model(c);
        assert_eq!(view.cid, "ACME-1234");
        assert_eq!(view.provider.as_deref(), Some("ACME"));
        assert_eq!(view.type_.as_deref(), Some("Internet"));
        assert_eq!(view.commit_rate_kbps, Some(1_000_000));
        assert_eq!(view.tags, vec!["transit"]);
        assert!(view.terminations.is_empty());
        assert!(view.diagram.is_empty());

        let plain = view.to_plain();
        assert!(plain.starts_with("cid: ACME-1234"));
        assert!(plain.contains("provider: ACME"));
        // commit_rate is humanized for display.
        assert!(plain.contains("commit_rate: 1 Gbps"));
        assert!(plain.contains("tags: transit"));
        // No terminations ⇒ no path section.
        assert!(!plain.contains("Circuit Path"));
    }

    #[test]
    fn tags_dropped_when_empty() {
        let c = circuit(json!({"id": 3, "url": "u", "cid": "ACME-1234"}));
        let view = CircuitView::from_model(c);
        assert!(view.tags.is_empty());
        let value = serde_json::to_value(&view).unwrap();
        assert!(value.get("tags").is_none());
        assert!(!view.to_key_values().render().contains("tags:"));
    }

    #[test]
    fn builds_az_diagram_with_terminations() {
        let c = circuit(json!({
            "id": 1636, "url": "u", "cid": "FC-208420188",
            "provider": {"id": 1, "display": "314BCE"},
            "type": {"id": 2, "display": "Direct Connect"},
            "status": {"value": "active", "label": "Active"},
            "commit_rate": 400_000_000,
            "custom_fields": {}
        }));
        let terms: Vec<CircuitTermination> = serde_json::from_value(json!([
            {
                "id": 2391, "term_side": "Z",
                "termination": {"id": 317, "display": "314BCE DX"},
                "termination_type": "circuits.providernetwork",
                "link_peers": []
            },
            {
                "id": 2390, "term_side": "A",
                "termination": {"id": 433, "display": "US-CHI02", "name": "US-CHI02"},
                "termination_type": "dcim.site",
                "cable": {"id": 2_378_128, "display": "#2378128"},
                "link_peers": [
                    {"id": 35_640, "name": "13", "device": {"id": 307_818, "name": "355.M03.01.02.PNL.01"}}
                ]
            }
        ]))
        .unwrap();

        let view = CircuitView::build(c, terms);
        // A is ordered before Z.
        assert_eq!(view.terminations[0].side, "A");
        assert_eq!(view.terminations[0].endpoint, "US-CHI02");
        assert_eq!(view.terminations[0].endpoint_kind, "site");
        assert_eq!(
            view.terminations[0].connected_to.as_deref(),
            Some("355.M03.01.02.PNL.01 13")
        );
        assert_eq!(view.terminations[0].cable.as_deref(), Some("#2378128"));
        assert_eq!(view.terminations[1].side, "Z");
        assert_eq!(view.terminations[1].endpoint_kind, "provider network");

        // The diagram is a vertical A↔Z with the circuit segment between.
        assert_eq!(view.diagram[0], " A  US-CHI02  (site)");
        assert!(
            view.diagram
                .iter()
                .any(|l| l == "    ↳ 355.M03.01.02.PNL.01 13  ·  #2378128"),
            "{:?}",
            view.diagram
        );
        assert!(
            view.diagram
                .iter()
                .any(|l| l == "    ┿ Direct Connect · 400 Gbps · Active"),
            "{:?}",
            view.diagram
        );
        assert!(
            view.diagram
                .iter()
                .any(|l| l == " Z  314BCE DX  (provider network)"),
            "{:?}",
            view.diagram
        );

        // Plain output renders the path under the Circuit Path heading.
        let plain = view.to_plain();
        assert!(plain.contains("Circuit Path"));
        assert!(plain.contains("┿ Direct Connect · 400 Gbps · Active"));
    }

    #[test]
    fn humanizes_speeds() {
        assert_eq!(humanize_kbps(400_000_000), "400 Gbps");
        assert_eq!(humanize_kbps(10_000_000), "10 Gbps");
        assert_eq!(humanize_kbps(1_000_000), "1 Gbps");
        assert_eq!(humanize_kbps(500_000), "500 Mbps");
        assert_eq!(humanize_kbps(1500), "1.5 Mbps");
        assert_eq!(humanize_kbps(0), "0");
    }
}
