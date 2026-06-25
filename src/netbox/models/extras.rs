//! Extras models: journal entries, tags, etc.

use serde::{Deserialize, Serialize};

use super::common::{BriefObject, Choice, Tag};

/// A tag, as returned by the listing endpoint (`/api/extras/tags/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TagInfo {
    pub id: u64,
    #[serde(default)]
    pub url: Option<String>,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Number of objects tagged (NetBox `tagged_items`).
    #[serde(default)]
    pub tagged_items: Option<u64>,
}

/// One row from `/api/extras/tagged-objects/` (NetBox 4.3+): an object carrying
/// a tag, across kinds. The `object` brief is polymorphic (its shape depends on
/// `object_type`); nbox reads only the display/name/id/url it carries.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaggedObject {
    pub id: u64,
    #[serde(default)]
    pub url: Option<String>,
    /// The dotted content type of the tagged object, e.g. `dcim.device`,
    /// `ipam.ipaddress`. Drives the friendly `kind` label nbox renders.
    pub object_type: String,
    pub object_id: u64,
    /// The tagged object itself — a brief carrying at least `id`/`url`/`display`
    /// (and often `name`). Shape varies by `object_type`. `None` only if NetBox
    /// omits it (it is present in practice).
    #[serde(default)]
    pub object: Option<BriefObject>,
    /// The tag this row is about (id/name/slug/color).
    pub tag: Tag,
    /// NetBox's one-line label, e.g. `"edge01 tagged with prod"`.
    #[serde(default)]
    pub display: Option<String>,
}

/// A journal entry (`/api/extras/journal-entries/`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JournalEntry {
    pub id: u64,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub kind: Option<Choice<String>>,
    /// The author — a nested user object (4.x) or a bare id, kept permissive.
    #[serde(default)]
    pub created_by: Option<serde_json::Value>,
    #[serde(default)]
    pub comments: String,
}

/// An object change (audit-log entry) from `/api/core/object-changes/` (NetBox
/// 4.x; was `extras/object-changes/` pre-4.0). One row per atomic write to an
/// object: the action (create/update/delete), who did it, when, and the
/// before/after state. Scoped to one object via `changed_object_type` (dotted,
/// e.g. `dcim.device`) + `changed_object_id`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ObjectChange {
    pub id: u64,
    #[serde(default)]
    pub url: Option<String>,
    /// The action: `{value: "create"|"update"|"delete", label: "Created"|…}`.
    #[serde(default)]
    pub action: Option<Choice<String>>,
    /// When the change was recorded (ISO 8601, UTC).
    #[serde(default)]
    pub time: Option<String>,
    /// The username of the actor (a convenience flat field alongside `user`).
    #[serde(default)]
    pub user_name: Option<String>,
    /// The nested user object (id/display/url), when present.
    #[serde(default)]
    pub user: Option<BriefObject>,
    /// The object's human label at change time (e.g. `edge01 (asset-tag)`).
    #[serde(default)]
    pub object_repr: Option<String>,
    /// A free-text message (often empty; some integrations annotate here).
    #[serde(default)]
    pub message: String,
    /// Groups all object-changes from one atomic request (a single user action
    /// can write several objects; they share this UUID).
    #[serde(default)]
    pub request_id: Option<String>,
    /// The full pre-change object state (JSON). Large; not rendered by default.
    #[serde(default)]
    pub prechange_data: Option<serde_json::Value>,
    /// The full post-change object state (JSON). Large; not rendered by default.
    #[serde(default)]
    pub postchange_data: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn journal_entry_with_nested_author() {
        let e: JournalEntry = serde_json::from_value(json!({
            "id": 5,
            "created": "2024-01-02T03:04:05Z",
            "kind": {"value": "info", "label": "Info"},
            "created_by": {"id": 1, "username": "admin", "display": "admin"},
            "comments": "rebooted"
        }))
        .unwrap();
        assert_eq!(e.kind.unwrap().value, "info");
        assert_eq!(e.comments, "rebooted");
    }
}
