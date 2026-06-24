//! Route-target view models for `nbox route-target` (plain + JSON) and the TUI
//! detail.
//!
//! A route target's relationship to VRFs lives on the *VRF* side
//! (`import_targets`/`export_targets`), so the detail resolves the importing and
//! exporting VRFs by filtering `/api/ipam/vrfs/` and presents them as navigable
//! references — the route target's relation graph.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::non_empty;
use crate::netbox::models::ipam::{RouteTarget, Vrf};
use crate::output::plain::KeyValues;

/// A navigable VRF reference (id + name + optional RD) on a route target's
/// importing/exporting lists.
#[derive(Debug, Clone, Serialize)]
pub struct VrfRef {
    pub id: u64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rd: Option<String>,
}

impl VrfRef {
    /// Normalize a wire [`Vrf`] into a lightweight navigable reference.
    #[must_use]
    pub fn from_model(v: &Vrf) -> Self {
        Self {
            id: v.id,
            name: v.name.clone(),
            rd: v.rd.clone().and_then(non_empty),
        }
    }

    /// A one-line label: `name (rd)` when an RD is present, else just the name.
    #[must_use]
    pub fn display_line(&self) -> String {
        match &self.rd {
            Some(rd) => format!("{} ({rd})", self.name),
            None => self.name.clone(),
        }
    }
}

/// The header summary of a route target.
#[derive(Debug, Clone, Serialize)]
pub struct RouteTargetView {
    pub id: u64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl RouteTargetView {
    /// Normalize a wire [`RouteTarget`].
    #[must_use]
    pub fn from_model(rt: RouteTarget) -> Self {
        Self {
            id: rt.id,
            name: rt.name,
            tenant: rt.tenant.map(|b| b.label()),
            description: rt.description.and_then(non_empty),
            owner: rt.owner.map(|bo| bo.label()),
            tags: rt.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&rt.custom_fields),
        }
    }

    fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("name", self.name.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("description", self.description.clone());
        if !self.tags.is_empty() {
            kv.push("tags", self.tags.join(", "));
        }
        kv
    }
}

/// A route target plus the VRFs that import/export it — its relation graph.
#[derive(Debug, Clone, Serialize)]
pub struct RouteTargetDetail {
    pub summary: RouteTargetView,
    pub importing_vrfs: Vec<VrfRef>,
    pub exporting_vrfs: Vec<VrfRef>,
}

impl RouteTargetDetail {
    /// Render the full route target — header plus its importing/exporting VRFs —
    /// for plain (`-o plain`) output.
    #[must_use]
    pub fn to_plain(&self) -> String {
        let mut out = self.summary.to_key_values().render();
        out.push('\n');
        push_vrf_section(&mut out, "Importing VRFs", &self.importing_vrfs);
        push_vrf_section(&mut out, "Exporting VRFs", &self.exporting_vrfs);
        out
    }
}

fn push_vrf_section(out: &mut String, title: &str, vrfs: &[VrfRef]) {
    use std::fmt::Write as _;
    let _ = write!(out, "\n{title} ({})", vrfs.len());
    if vrfs.is_empty() {
        out.push_str("\n  (none)");
        return;
    }
    for v in vrfs {
        let _ = write!(out, "\n  {}", v.display_line());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_model_flattens_tenant_and_drops_empty_description() {
        let rt: RouteTarget = serde_json::from_value(json!({
            "id": 5, "url": "u", "name": "65000:100",
            "tenant": {"id": 1, "display": "Acme"},
            "description": "",
            "tags": [{"id": 1, "name": "prod", "slug": "prod"}]
        }))
        .unwrap();
        let v = RouteTargetView::from_model(rt);
        assert_eq!(v.name, "65000:100");
        assert_eq!(v.tenant.as_deref(), Some("Acme"));
        assert!(v.description.is_none(), "empty description is dropped");
        assert_eq!(v.tags, vec!["prod"]);
    }

    #[test]
    fn to_plain_renders_both_vrf_directions() {
        let detail = RouteTargetDetail {
            summary: RouteTargetView {
                id: 5,
                name: "65000:100".into(),
                tenant: Some("Acme".into()),
                description: None,
                owner: None,
                tags: vec![],
                custom_fields: BTreeMap::new(),
            },
            importing_vrfs: vec![
                VrfRef {
                    id: 1,
                    name: "customer-prod".into(),
                    rd: Some("65000:100".into()),
                },
                VrfRef {
                    id: 2,
                    name: "customer-dev".into(),
                    rd: None,
                },
            ],
            exporting_vrfs: vec![],
        };
        let plain = detail.to_plain();
        assert!(plain.contains("name: 65000:100"));
        assert!(plain.contains("Importing VRFs (2)"));
        assert!(plain.contains("customer-prod (65000:100)"));
        assert!(plain.contains("customer-dev"));
        assert!(plain.contains("Exporting VRFs (0)"));
        assert!(plain.contains("(none)"));
    }
}
