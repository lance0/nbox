//! Flattened VLAN view for `nbox vlan` (plain + JSON), with associated prefixes.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::non_empty;
use crate::netbox::models::ipam::{Prefix, Vlan, VlanGroup};
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
    /// Name of the VLAN *group*'s scope object, when the VLAN belongs to a group
    /// and that group is itself scoped. A VLAN group is polymorphically scoped
    /// (the VLAN is not), so this is distinct from the VLAN's own
    /// [`scope`](Self::scope) and is surfaced separately rather than overriding it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_scope: Option<String>,
    /// Friendly scope type of the VLAN group's scope, e.g.
    /// `site`/`location`/`region`/`site-group`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_scope_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
    pub prefixes: Vec<String>,
}

impl VlanView {
    /// Build a view from a VLAN plus the prefixes that reference it.
    ///
    /// `group` is the VLAN's resolved VLAN group, fetched only when the VLAN has
    /// one (see [`vlan_group_by_id`](crate::netbox::client::NetBoxClient::vlan_group_by_id)).
    /// Pass `None` when the VLAN has no group, or for callers that don't surface
    /// the group scope — the existing fields/output are then unchanged.
    pub fn build(v: Vlan, prefixes: Vec<Prefix>, group: Option<VlanGroup>) -> Self {
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
        // The VLAN *group*'s own scope (a group is polymorphically scoped; the
        // VLAN is not). Surfaced as separate fields rather than overriding the
        // VLAN's `scope`, and only when the group actually carries a scope.
        let (group_scope, group_scope_type) = match group {
            Some(g) => match g.scope {
                Some(b) => (
                    Some(b.label()),
                    g.scope_type.as_deref().map(friendly_scope_type),
                ),
                None => (None, None),
            },
            None => (None, None),
        };
        Self {
            vid: v.vid,
            name: v.name,
            status: v.status.map(|c| c.value),
            group: v.group.map(|b| b.label()),
            scope,
            scope_type,
            group_scope,
            group_scope_type,
            tenant: v.tenant.map(|b| b.label()),
            role: v.role.map(|b| b.label()),
            description: v.description.and_then(non_empty),
            tags: v.tags.into_iter().map(|tag| tag.slug).collect(),
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
            .push_opt("group_scope", self.group_scope.clone())
            .push_opt("group_scope_type", self.group_scope_type.clone())
            .push_opt("tenant", self.tenant.clone())
            .push_opt("role", self.role.clone())
            .push_opt("description", self.description.clone());
        if !self.tags.is_empty() {
            kv.push("tags", self.tags.join(", "));
        }
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
            "group": {"id": 1, "display": "iad1-campus"},
            "tags": [{"id": 1, "name": "users", "slug": "users"}]
        }))
        .unwrap();
        let prefixes: Vec<Prefix> = vec![
            serde_json::from_value(json!({"id": 1, "url": "u", "prefix": "10.44.208.0/24"}))
                .unwrap(),
            serde_json::from_value(json!({"id": 2, "url": "u", "prefix": "10.45.208.0/24"}))
                .unwrap(),
        ];

        let view = VlanView::build(v, prefixes, None);
        assert_eq!(view.vid, 208);
        assert_eq!(view.group.as_deref(), Some("iad1-campus"));
        assert_eq!(view.prefixes.len(), 2);
        assert_eq!(view.tags, vec!["users"]);

        let plain = view.to_plain();
        assert!(plain.contains("vid: 208"));
        assert!(plain.contains("tags: users"));
        assert!(plain.contains("Prefixes\n  10.44.208.0/24\n  10.45.208.0/24"));
    }

    #[test]
    fn tags_dropped_when_empty() {
        let v: Vlan = serde_json::from_value(json!({
            "id": 3, "url": "u", "vid": 208, "name": "users"
        }))
        .unwrap();
        let view = VlanView::build(v, vec![], None);
        assert!(view.tags.is_empty());
        let value = serde_json::to_value(&view).unwrap();
        assert!(value.get("tags").is_none());
        assert!(!view.to_plain().contains("tags:"));
    }

    #[test]
    fn direct_site_assignment_surfaces_as_site_scope() {
        let v: Vlan = serde_json::from_value(json!({
            "id": 3, "url": "u", "vid": 208, "name": "users",
            "site": {"id": 1, "display": "iad1"}
        }))
        .unwrap();
        let view = VlanView::build(v, vec![], None);
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
        let view = VlanView::build(v, vec![], None);
        assert_eq!(view.scope.as_deref(), Some("row-a"));
        assert_eq!(view.scope_type.as_deref(), Some("location"));

        let value = serde_json::to_value(&view).unwrap();
        assert!(value.get("site").is_none(), "site key must be gone");
        assert_eq!(value["scope"], "row-a");
        assert_eq!(value["scope_type"], "location");
    }

    #[test]
    fn group_scope_is_surfaced_separately_from_the_vlan_scope() {
        // The VLAN has a group AND a direct site; the group is scoped to a
        // region. The VLAN's own `site` scope is unchanged, and the group's
        // scope is surfaced on the NEW additive fields.
        let v: Vlan = serde_json::from_value(json!({
            "id": 3, "url": "u", "vid": 208, "name": "users",
            "site": {"id": 1, "display": "iad1"},
            "group": {"id": 9, "display": "iad1-campus"}
        }))
        .unwrap();
        let group: VlanGroup = serde_json::from_value(json!({
            "id": 9, "name": "iad1-campus", "slug": "iad1-campus",
            "scope_type": "dcim.region",
            "scope": {"id": 5, "display": "us-east"}
        }))
        .unwrap();

        let view = VlanView::build(v, vec![], Some(group));
        // Existing fields untouched.
        assert_eq!(view.scope.as_deref(), Some("iad1"));
        assert_eq!(view.scope_type.as_deref(), Some("site"));
        assert_eq!(view.group.as_deref(), Some("iad1-campus"));
        // New additive fields populated from the group's scope.
        assert_eq!(view.group_scope.as_deref(), Some("us-east"));
        assert_eq!(view.group_scope_type.as_deref(), Some("region"));

        let plain = view.to_plain();
        assert!(plain.contains("group_scope: us-east"), "got: {plain}");
        assert!(plain.contains("group_scope_type: region"), "got: {plain}");
    }

    #[test]
    fn no_group_omits_group_scope_fields() {
        let v: Vlan = serde_json::from_value(json!({
            "id": 3, "url": "u", "vid": 208, "name": "users",
            "site": {"id": 1, "display": "iad1"}
        }))
        .unwrap();
        let view = VlanView::build(v, vec![], None);
        assert!(view.group_scope.is_none());
        assert!(view.group_scope_type.is_none());

        let value = serde_json::to_value(&view).unwrap();
        assert!(value.get("group_scope").is_none());
        assert!(value.get("group_scope_type").is_none());
        let plain = view.to_plain();
        assert!(!plain.contains("group_scope"), "got: {plain}");
    }

    #[test]
    fn group_without_a_scope_omits_group_scope_fields() {
        // The VLAN has a group, but the group itself is not scoped → omit.
        let v: Vlan = serde_json::from_value(json!({
            "id": 3, "url": "u", "vid": 208, "name": "users",
            "group": {"id": 9, "display": "campus"}
        }))
        .unwrap();
        let group: VlanGroup = serde_json::from_value(json!({
            "id": 9, "name": "campus", "slug": "campus"
        }))
        .unwrap();
        let view = VlanView::build(v, vec![], Some(group));
        assert!(view.group_scope.is_none());
        assert!(view.group_scope_type.is_none());
    }
}
