//! Flattened VRF view for `nbox vrf` (plain + JSON).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::non_empty;
use crate::netbox::models::common::BriefObject;
use crate::netbox::models::ipam::Vrf;
use crate::output::plain::KeyValues;

/// A navigable route-target reference (id + name) on a VRF's import/export lists.
/// The id lets the VRF view's `targets` tab jump to the route target's detail.
#[derive(Debug, Clone, Serialize)]
pub struct RouteTargetRef {
    pub id: u64,
    pub name: String,
}

impl RouteTargetRef {
    fn from_brief(b: BriefObject) -> Self {
        Self {
            id: b.id,
            name: b.label(),
        }
    }
}

/// Join route-target names for the compact `key: value` plain output.
fn join_target_names(targets: &[RouteTargetRef]) -> String {
    targets
        .iter()
        .map(|r| r.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A VRF, normalized to flat string fields.
#[derive(Debug, Clone, Serialize)]
pub struct VrfView {
    pub id: u64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enforce_unique: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub import_targets: Vec<RouteTargetRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub export_targets: Vec<RouteTargetRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipaddress_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl VrfView {
    /// Normalize a wire [`Vrf`].
    pub fn from_model(v: Vrf) -> Self {
        Self {
            id: v.id,
            name: v.name,
            rd: v.rd.and_then(non_empty),
            tenant: v.tenant.map(|b| b.label()),
            enforce_unique: v.enforce_unique,
            import_targets: v
                .import_targets
                .into_iter()
                .map(RouteTargetRef::from_brief)
                .collect(),
            export_targets: v
                .export_targets
                .into_iter()
                .map(RouteTargetRef::from_brief)
                .collect(),
            prefix_count: v.prefix_count,
            ipaddress_count: v.ipaddress_count,
            description: v.description.and_then(non_empty),
            tags: v.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&v.custom_fields),
        }
    }

    /// Render as `key: value` lines for plain output.
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("name", self.name.clone())
            .push_opt("rd", self.rd.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("enforce_unique", self.enforce_unique.map(|b| b.to_string()));
        if !self.import_targets.is_empty() {
            kv.push("import_targets", join_target_names(&self.import_targets));
        }
        if !self.export_targets.is_empty() {
            kv.push("export_targets", join_target_names(&self.export_targets));
        }
        kv.push_opt("prefixes", self.prefix_count.map(|c| c.to_string()))
            .push_opt("addresses", self.ipaddress_count.map(|c| c.to_string()))
            .push_opt("description", self.description.clone());
        if !self.tags.is_empty() {
            kv.push("tags", self.tags.join(", "));
        }
        custom::append(&mut kv, &self.custom_fields);
        kv
    }
}

/// A backend-neutral VRF routing-context view: the VRF summary plus its scoped
/// prefixes (as a tree), addresses, and counts. REST and GraphQL both fill this
/// identical shape, so the CLI/MCP/TUI never depend on which backend produced it.
#[derive(Debug, Clone, Serialize)]
pub struct VrfDetail {
    pub summary: VrfView,
    pub prefixes: Vec<VrfPrefixRow>,
    pub addresses: Vec<VrfAddressRow>,
    /// Total prefixes in the VRF (may exceed `prefixes.len()` when capped).
    pub prefix_total: u64,
    /// Total addresses in the VRF (may exceed `addresses.len()` when capped).
    pub address_total: u64,
}

/// One prefix in a VRF's tree: enough to render the indented tree row and to
/// open the prefix on `Enter` (TUI) or list it (CLI/JSON).
#[derive(Debug, Clone, Serialize)]
pub struct VrfPrefixRow {
    pub id: u64,
    pub prefix: String,
    /// Per-VRF tree depth (0 = top level), driving indentation.
    pub depth: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Utilization percent when known (a container's child coverage); leaves have none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utilization: Option<u8>,
}

/// One IP address scoped to a VRF.
#[derive(Debug, Clone, Serialize)]
pub struct VrfAddressRow {
    pub id: u64,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dns_name: Option<String>,
}

/// A 7-cell utilization bar (`▓▓▓░░░░`), matching the prefix view's bar style.
fn util_bar(pct: u8) -> String {
    const WIDTH: usize = 7;
    let filled = ((f64::from(pct).clamp(0.0, 100.0) / 100.0) * WIDTH as f64).round() as usize;
    let filled = filled.min(WIDTH);
    let mut bar = String::with_capacity(WIDTH * 3);
    for i in 0..WIDTH {
        bar.push_str(if i < filled { "▓" } else { "░" });
    }
    bar
}

impl VrfPrefixRow {
    /// The aligned tree row: indented CIDR, status, then a utilization bar when
    /// known (else the description) — the single source of the row text shared by
    /// CLI plain output and the TUI's navigable rows.
    #[must_use]
    pub fn display_line(&self) -> String {
        let label = format!("{}{}", "  ".repeat(self.depth as usize), self.prefix);
        let status = self.status.as_deref().unwrap_or("");
        let tail = match self.utilization {
            Some(pct) => format!("{}  {pct}%", util_bar(pct)),
            None if !self.description.is_empty() => self.description.clone(),
            None => String::new(),
        };
        format!("{label:<28}{status:<10}{tail}")
            .trim_end()
            .to_string()
    }
}

impl VrfAddressRow {
    /// The aligned address row: address then DNS name or status.
    #[must_use]
    pub fn display_line(&self) -> String {
        let note = self
            .dns_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.status.as_deref())
            .unwrap_or("");
        format!("{:<24}{note}", self.address).trim_end().to_string()
    }
}

impl VrfDetail {
    /// Render summary fields plus the prefix tree, address, and route-target
    /// sections for plain CLI output (mirrors how `prefix`/`vlan` show sections).
    #[must_use]
    pub fn to_plain(&self) -> String {
        use std::fmt::Write as _;
        let mut out = self.summary.to_key_values().render();

        let _ = write!(out, "\n\nPrefixes ({})", self.prefix_total);
        if self.prefixes.is_empty() {
            out.push_str("\n  (none)");
        } else {
            for row in &self.prefixes {
                let _ = write!(out, "\n  {}", row.display_line());
            }
            if self.prefix_total as usize > self.prefixes.len() {
                let _ = write!(
                    out,
                    "\n  … {} more",
                    self.prefix_total as usize - self.prefixes.len()
                );
            }
        }

        let _ = write!(out, "\n\nAddresses ({})", self.address_total);
        if self.addresses.is_empty() {
            out.push_str("\n  (none)");
        } else {
            for row in &self.addresses {
                let _ = write!(out, "\n  {}", row.display_line());
            }
            if self.address_total as usize > self.addresses.len() {
                let _ = write!(
                    out,
                    "\n  … {} more",
                    self.address_total as usize - self.addresses.len()
                );
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_vrf() {
        let v: Vrf = serde_json::from_value(json!({
            "id": 7, "url": "u", "name": "customer-prod",
            "rd": "65000:100",
            "tenant": {"id": 1, "display": "Acme Corp"},
            "enforce_unique": true,
            "import_targets": [{"id": 1, "name": "65000:100"}, {"id": 2, "name": "65000:200"}],
            "export_targets": [{"id": 1, "name": "65000:100"}],
            "prefix_count": 12,
            "ipaddress_count": 48,
            "tags": [{"id": 1, "name": "prod", "slug": "prod"}]
        }))
        .unwrap();
        let view = VrfView::from_model(v);
        assert_eq!(view.rd.as_deref(), Some("65000:100"));
        assert_eq!(view.tenant.as_deref(), Some("Acme Corp"));
        assert_eq!(view.enforce_unique, Some(true));
        assert_eq!(
            view.import_targets
                .iter()
                .map(|r| (r.id, r.name.as_str()))
                .collect::<Vec<_>>(),
            vec![(1, "65000:100"), (2, "65000:200")]
        );
        assert_eq!(view.export_targets.len(), 1);
        assert_eq!(view.prefix_count, Some(12));
        let plain = view.to_key_values().render();
        assert!(plain.contains("rd: 65000:100"));
        assert!(plain.contains("import_targets: 65000:100, 65000:200"));
        assert!(plain.contains("prefixes: 12"));
        assert!(plain.contains("tags: prod"));
    }

    #[test]
    fn empty_optionals_dropped() {
        let v: Vrf = serde_json::from_value(json!({
            "id": 1, "url": "u", "name": "global"
        }))
        .unwrap();
        let view = VrfView::from_model(v);
        assert!(view.rd.is_none());
        assert!(view.import_targets.is_empty());
        let value = serde_json::to_value(&view).unwrap();
        assert!(value.get("rd").is_none());
        assert!(value.get("import_targets").is_none());
        assert!(!view.to_key_values().render().contains("rd:"));
    }
}
