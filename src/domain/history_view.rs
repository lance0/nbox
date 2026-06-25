//! Object-change (audit-log) view for `nbox history` (plain + JSON).

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::Serialize;

use crate::netbox::models::common::BriefObject;
use crate::netbox::models::extras::ObjectChange;

/// One audit-log entry, flattened for display. Surfaces *what changed* (the
/// top-level field names whose values differ between `prechange_data` and
/// `postchange_data`) without dumping the full before/after JSON (which can be
/// kilobytes per row). The full before/after payloads are included only when
/// `diff` is requested (`nbox history --diff` / `nbox_history` with `diff=true`),
/// so the default list stays compact; they are omitted (not null) otherwise.
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
    /// The full pre-change object state (JSON). Only populated when `diff` is
    /// requested; absent for a `create` (no prior state).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<serde_json::Value>,
    /// The full post-change object state (JSON). Only populated when `diff` is
    /// requested; absent for a `delete` (no resulting state).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<serde_json::Value>,
}

/// A list of object changes (audit-log entries) for one object, newest first.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct HistoryView {
    pub entries: Vec<HistoryRow>,
}

impl HistoryView {
    /// Normalize wire [`ObjectChange`] records into display rows. When `diff`
    /// is set, each row also carries the full `before`/`after` change payloads
    /// (kilobytes each) so a single change can be inspected in full; otherwise
    /// they are omitted to keep the list compact.
    pub fn from_models(changes: Vec<ObjectChange>, diff: bool) -> Self {
        let entries = changes
            .into_iter()
            .map(|c| {
                // `fields_changed` is always derived from the change payloads
                // (the compact list still surfaces it); only the heavy full
                // before/after payloads are gated on `diff`.
                let pre = c.prechange_data;
                let post = c.postchange_data;
                let fields_changed = changed_fields(pre.as_ref(), post.as_ref());
                let (before, after) = if diff { (pre, post) } else { (None, None) };
                HistoryRow {
                    time: c.time,
                    action: c.action.as_ref().map(|a| a.value.clone()),
                    action_label: c.action.as_ref().map(|a| a.label.clone()),
                    user: c
                        .user_name
                        .or_else(|| c.user.as_ref().map(BriefObject::label)),
                    object: c.object_repr,
                    message: c.message,
                    fields_changed,
                    request_id: c.request_id,
                    before,
                    after,
                }
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
            if let Some(before) = &e.before {
                body.push_str(&render_payload("before", before));
            }
            if let Some(after) = &e.after {
                body.push_str(&render_payload("after", after));
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
/// object state. Both sides must be object-shaped snapshots; creates/deletes or
/// absent payloads intentionally report no field list rather than treating the
/// entire object as a changed field set. Sorted for stable output.
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

/// Render a `before:`/`after:` change payload as an indented pretty-JSON block
/// (the label on its own line, the JSON indented four spaces). Large payloads
/// stay readable instead of one long line.
fn render_payload(label: &str, v: &serde_json::Value) -> String {
    let pretty = serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string());
    let indented = pretty
        .lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("  {label}:\n{indented}\n")
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
        let view = HistoryView::from_models(changes, false);
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
        let view = HistoryView::from_models(vec![], false);
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
        let view = HistoryView::from_models(changes, false);
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
        let view = HistoryView::from_models(changes, false);
        assert_eq!(
            view.entries[0].user.as_deref(),
            Some("neteng (Network Engineering)")
        );
    }

    /// `diff=true` populates the full `before`/`after` payloads (and still
    /// derives `fields_changed` from them), while `diff=false` omits them so the
    /// default list stays compact.
    #[test]
    fn diff_carries_full_before_after_payloads() {
        let changes: Vec<ObjectChange> = vec![
            serde_json::from_value(json!({
                "id": 11,
                "action": {"value": "update", "label": "Updated"},
                "time": "2025-12-08T23:56:49Z",
                "user_name": "neteng",
                "object_repr": "edge01",
                "message": "retired",
                "request_id": "bc44-0002",
                "prechange_data": {"name": "edge01", "status": "active"},
                "postchange_data": {"name": "edge01", "status": "decommissioned", "site_id": 2}
            }))
            .unwrap(),
        ];

        let compact = HistoryView::from_models(changes.clone(), false);
        assert!(compact.entries[0].before.is_none());
        assert!(compact.entries[0].after.is_none());
        assert_eq!(compact.entries[0].fields_changed, vec!["site_id", "status"]);

        let full = HistoryView::from_models(changes, true);
        assert_eq!(
            full.entries[0].before,
            Some(json!({"name": "edge01", "status": "active"}))
        );
        assert_eq!(
            full.entries[0].after,
            Some(json!({"name": "edge01", "status": "decommissioned", "site_id": 2}))
        );
        // fields_changed is still derived the same way.
        assert_eq!(full.entries[0].fields_changed, vec!["site_id", "status"]);
    }

    /// The plain render includes a `before:`/`after:` indented JSON block only
    /// when the payloads are present (diff mode); the compact list is unchanged.
    #[test]
    fn diff_plain_render_includes_before_after_blocks() {
        let changes: Vec<ObjectChange> = vec![
            serde_json::from_value(json!({
                "id": 12,
                "action": {"value": "update", "label": "Updated"},
                "time": "2025-12-08T23:56:49Z",
                "user_name": "neteng",
                "object_repr": "edge01",
                "message": "",
                "request_id": "bc44-0003",
                "prechange_data": {"status": "active"},
                "postchange_data": {"status": "decommissioned"}
            }))
            .unwrap(),
        ];

        let compact_plain = HistoryView::from_models(changes.clone(), false).to_plain();
        assert!(
            !compact_plain.contains("before:"),
            "compact render has no before block"
        );
        assert!(
            !compact_plain.contains("after:"),
            "compact render has no after block"
        );

        let diff_plain = HistoryView::from_models(changes, true).to_plain();
        assert!(
            diff_plain.contains("before:"),
            "diff render has before block"
        );
        assert!(diff_plain.contains("after:"), "diff render has after block");
        assert!(
            diff_plain.contains("\"active\""),
            "diff render shows the pre value"
        );
        assert!(
            diff_plain.contains("\"decommissioned\""),
            "diff render shows the post value"
        );
    }
}
