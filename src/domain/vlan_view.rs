//! Flattened VLAN view for `nbx vlan` (plain + JSON), with associated prefixes.

use serde::Serialize;

use crate::netbox::models::ipam::{Prefix, Vlan};
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub prefixes: Vec<String>,
}

impl VlanView {
    /// Build a view from a VLAN plus the prefixes that reference it.
    pub fn build(v: Vlan, prefixes: Vec<Prefix>) -> Self {
        let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };
        Self {
            vid: v.vid,
            name: v.name,
            status: v.status.map(|c| c.value),
            group: v.group.map(|b| b.label()),
            site: v.site.map(|b| b.label()),
            tenant: v.tenant.map(|b| b.label()),
            role: v.role.map(|b| b.label()),
            description: v.description.and_then(non_empty),
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
            .push_opt("site", self.site.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("role", self.role.clone())
            .push_opt("description", self.description.clone());
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
}
