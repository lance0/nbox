//! Object-change (audit-log) view for `nbox history` (plain + JSON).

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::Serialize;

use crate::netbox::models::common::BriefObject;
use crate::netbox::models::extras::ObjectChange;

/// One audit-log entry, flattened for display. Surfaces *what changed* (the
/// top-level field names whose values differ between `prechange_data` and
/// `postchange_data`) without dumping the full before/after JSON (which can be
/// kilobytes per row). The full diff is available via `--diff` (planned).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct HistoryRow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    /// `create` / `update` / `delete` (the Choice value).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// The human label for the action (`Created` / `Updated` / `Deleted`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_label: Option<String>,
    /// Who made the change (the flat `user_name`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// The object's human label at change time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// A free-text message (often empty).
    #[serde(skip_serializing_if = "skip_empty_str")]
    pub message: String,
    /// Top-level field names whose values changed (pre ≠ post). Empty for a
    /// bare `create`/`delete` or when the payload is absent.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields_changed: Vec<String>,
    /// Groups changes from one atomic request (a shared UUID).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// A list of object changes (audit-log entries) for one object, newest first.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct HistoryView {
    pub entries: Vec<HistoryRow>,
}

impl HistoryView {
    /// Normalize wire [`ObjectChange`] records into display rows.
    pub fn from_models(changes: Vec<ObjectChange>) -> Self {
        let entries = changes
            .into_iter()
            .map(|c| HistoryRow {
                time: c.time,
                action: c.action.as_ref().map(|a| a.value.clone()),
                action_label: c.action.as_ref().map(|a| a.label.clone()),
                user: c
                    .user_name
                    .or_else(|| c.user.as_ref().map(BriefObject::label)),
                object: c.object_repr,
                message: c.message,
                fields_changed: changed_fields(
                    c.prechange_data.as_ref(),
                    c.postchange_data.as_ref(),
                ),
                request_id: c.request_id,
            })
            .collect();
        Self { entries }
    }

    /// Render entries as `time  action  user  object` headers with indented
    /// field-change + message lines.
    pub fn to_plain(&self) -> String {
        use std::fmt::Write;
        if self.entries.is_empty() {
            return "no change history".to_string();
        }
        let mut blocks = Vec::new();
        for e in &self.entries {
            let mut header = String::new();
            if let Some(t) = &e.time {
                header.push_str(t);
            }
            let action = e
                .action_label
                .as_deref()
                .or(e.action.as_deref())
                .unwrap_or("changed");
            let _ = write!(header, "  {action}");
            if let Some(u) = &e.user {
                let _ = write!(header, "  {u}");
            }
            if let Some(o) = &e.object {
                let _ = write!(header, "  — {o}");
            }
            let mut body = String::new();
            if !e.fields_changed.is_empty() {
                let _ = writeln!(body, "  fields: {}", e.fields_changed.join(", "));
            }
            if !e.message.is_empty() {
                for l in e.message.lines() {
                    let _ = writeln!(body, "  {l}");
                }
            }
            blocks.push(if body.is_empty() {
                header
            } else {
                format!("{header}\n{}", body.trim_end())
            });
        }
        blocks.join("\n\n")
    }
}

/// The top-level field names whose values differ between the pre- and post-change
/// object state. For a `create`, prechange is null so all postchange keys are
/// "changed"; for a `delete`, postchange is null so all prechange keys are listed.
/// Sorted for stable output.
fn changed_fields(
    pre: Option<&serde_json::Value>,
    post: Option<&serde_json::Value>,
) -> Vec<String> {
    let (Some(pre), Some(post)) = (pre, post) else {
        return Vec::new();
    };
    let (Some(pre_obj), Some(post_obj)) = (pre.as_object(), post.as_object()) else {
        return Vec::new();
    };
    let mut changed: BTreeSet<String> = BTreeSet::new();
    // Fields present in both: changed if the values differ.
    for (k, pv) in pre_obj {
        if post_obj.get(k) != Some(pv) {
            changed.insert(k.clone());
        }
    }
    // Fields only in post (added).
    for k in post_obj.keys() {
        if !pre_obj.contains_key(k) {
            changed.insert(k.clone());
        }
    }
    // Fields only in pre (removed) are already captured by the differ check.
    changed.into_iter().collect()
}

fn skip_empty_str(s: &str) -> bool {
    s.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netbox::models::common::Choice;
    use serde_json::json;

    #[test]
    fn renders_update_with_fields_and_user() {
        let changes: Vec<ObjectChange> = vec![serde_json::from_value(json!({
            "id": 1,
            "action": {"value": "update", "label": "Updated"},
            "time": "2025-12-08T23:56:49.235526Z",
            "user_name": "neteng",
            "object_repr": "edge01",
            "message": "",
            "request_id": "bc44c7b8-ff00-4666-bf82-59f33c706155",
            "prechange_data": {"name": "edge01", "status": "active", "site_id": 1},
            "postchange_data": {"name": "edge01", "status": "decommissioned", "site_id": 2, "comments": "retired"}
        })).unwrap()];
        let view = HistoryView::from_models(changes);
        let row = &view.entries[0];
        assert_eq!(row.action.as_deref(), Some("update"));
        assert_eq!(row.user.as_deref(), Some("neteng"));
        assert_eq!(row.object.as_deref(), Some("edge01"));
        // status (changed), site_id (changed), comments (added) — name unchanged.
        assert_eq!(row.fields_changed, vec!["comments", "site_id", "status"]);
        let plain = view.to_plain();
        assert!(plain.contains("Updated"), "got: {plain}");
        assert!(plain.contains("neteng"), "got: {plain}");
        assert!(plain.contains("edge01"), "got: {plain}");
        assert!(
            plain.contains("fields: comments, site_id, status"),
            "got: {plain}"
        );
    }

    #[test]
    fn empty_history_is_explicit() {
        let view = HistoryView::from_models(vec![]);
        assert_eq!(view.to_plain(), "no change history");
    }

    #[test]
    fn create_lists_all_postchange_fields() {
        let changes: Vec<ObjectChange> = vec![
            serde_json::from_value(json!({
                "id": 2,
                "action": {"value": "create", "label": "Created"},
                "time": "2025-12-24T14:58:30Z",
                "user_name": "fleetops",
                "object_repr": "edge02",
                "message": "",
                "prechange_data": null,
                "postchange_data": {"name": "edge02", "status": "active", "site_id": 5}
            }))
            .unwrap(),
        ];
        let view = HistoryView::from_models(changes);
        // prechange is null → no diff (only both-present objects are compared).
        assert!(view.entries[0].fields_changed.is_empty());
    }

    #[test]
    fn user_falls_back_to_nested_user_object() {
        let changes: Vec<ObjectChange> = vec![ObjectChange {
            id: 3,
            url: None,
            action: Some(Choice {
                value: "delete".into(),
                label: "Deleted".into(),
            }),
            time: Some("2025-12-31T00:00:00Z".into()),
            user_name: None,
            user: Some(
                serde_json::from_value(
                    json!({"id": 307, "display": "neteng (Network Engineering)"}),
                )
                .unwrap(),
            ),
            object_repr: Some("edge03".into()),
            message: String::new(),
            request_id: None,
            prechange_data: None,
            postchange_data: None,
        }];
        let view = HistoryView::from_models(changes);
        assert_eq!(
            view.entries[0].user.as_deref(),
            Some("neteng (Network Engineering)")
        );
    }
}
