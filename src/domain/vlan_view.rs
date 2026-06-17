//! Flattened VLAN view for `nbox vlan` (plain + JSON), with associated prefixes.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::netbox::models::ipam::{Prefix, Vlan};
use crate::netbox::query::friendly_scope_type;
use crate::output::plain::KeyValues;

/// A VLAN with the prefixes that reference it.
#[derive(Debug, Clone, Serialize)]
pub struct VlanView {
    pub vid: u16,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Name of the VLAN's scope object for any scope type. Prefers a polymorphic
    /// `scope` (see [`scope_type`](Self::scope_type)); falls back to a directly
    /// assigned `site`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Friendly scope type, e.g. `site`/`location`/`region`/`site-group`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
    pub prefixes: Vec<String>,
}

impl VlanView {
    /// Build a view from a VLAN plus the prefixes that reference it.
    pub fn build(v: Vlan, prefixes: Vec<Prefix>) -> Self {
        let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };
        // Prefer a polymorphic scope; fall back to a directly assigned site
        // (the common case on current NetBox, where `scope_type` is "site").
        let (scope, scope_type) = match (v.scope.as_ref(), v.scope_type.as_deref()) {
            (Some(b), Some(t)) => (Some(b.label()), Some(friendly_scope_type(t))),
            (Some(b), None) => (Some(b.label()), None),
            (None, _) => (
                v.site
                    .as_ref()
                    .map(super::super::netbox::models::common::BriefObject::label),
                v.site.as_ref().map(|_| "site".to_string()),
            ),
        };
        Self {
            vid: v.vid,
            name: v.name,
            status: v.status.map(|c| c.value),
            group: v.group.map(|b| b.label()),
            scope,
            scope_type,
            tenant: v.tenant.map(|b| b.label()),
            role: v.role.map(|b| b.label()),
            description: v.description.and_then(non_empty),
            custom_fields: custom::fields(&v.custom_fields),
            prefixes: prefixes.into_iter().map(|p| p.prefix).collect(),
        }
    }

    /// Render header fields plus a prefixes section for plain output.
    pub fn to_plain(&self) -> String {
        let mut kv = KeyValues::new();
        kv.push("vid", self.vid.to_string())
            .push("name", self.name.clone())
            .push_opt("status", self.status.clone())
            .push_opt("group", self.group.clone())
            .push_opt("scope", self.scope.clone())
            .push_opt("scope_type", self.scope_type.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("role", self.role.clone())
            .push_opt("description", self.description.clone());
        custom::append(&mut kv, &self.custom_fields);
        let mut out = kv.render();

        if !self.prefixes.is_empty() {
            out.push_str("\n\nPrefixes\n");
            let lines: Vec<String> = self.prefixes.iter().map(|p| format!("  {p}")).collect();
            out.push_str(&lines.join("\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_collects_prefixes() {
        let v: Vlan = serde_json::from_value(json!({
            "id": 3, "url": "u", "vid": 208, "name": "users",
            "status": {"value": "active", "label": "Active"},
            "group": {"id": 1, "display": "iad1-campus"}
        }))
        .unwrap();
        let prefixes: Vec<Prefix> = vec![
            serde_json::from_value(json!({"id": 1, "url": "u", "prefix": "10.44.208.0/24"}))
                .unwrap(),
            serde_json::from_value(json!({"id": 2, "url": "u", "prefix": "10.45.208.0/24"}))
                .unwrap(),
        ];

        let view = VlanView::build(v, prefixes);
        assert_eq!(view.vid, 208);
        assert_eq!(view.group.as_deref(), Some("iad1-campus"));
        assert_eq!(view.prefixes.len(), 2);

        let plain = view.to_plain();
        assert!(plain.contains("vid: 208"));
        assert!(plain.contains("Prefixes\n  10.44.208.0/24\n  10.45.208.0/24"));
    }

    #[test]
    fn direct_site_assignment_surfaces_as_site_scope() {
        let v: Vlan = serde_json::from_value(json!({
            "id": 3, "url": "u", "vid": 208, "name": "users",
            "site": {"id": 1, "display": "iad1"}
        }))
        .unwrap();
        let view = VlanView::build(v, vec![]);
        assert_eq!(view.scope.as_deref(), Some("iad1"));
        assert_eq!(view.scope_type.as_deref(), Some("site"));
        let plain = view.to_plain();
        assert!(plain.contains("scope: iad1"), "got: {plain}");
        assert!(plain.contains("scope_type: site"), "got: {plain}");
    }

    #[test]
    fn polymorphic_non_site_scope_is_surfaced_with_friendly_type() {
        let v: Vlan = serde_json::from_value(json!({
            "id": 3, "url": "u", "vid": 208, "name": "users",
            "scope_type": "dcim.location",
            "scope": {"id": 1, "display": "row-a"}
        }))
        .unwrap();
        let view = VlanView::build(v, vec![]);
        assert_eq!(view.scope.as_deref(), Some("row-a"));
        assert_eq!(view.scope_type.as_deref(), Some("location"));

        let value = serde_json::to_value(&view).unwrap();
        assert!(value.get("site").is_none(), "site key must be gone");
        assert_eq!(value["scope"], "row-a");
        assert_eq!(value["scope_type"], "location");
    }
}
