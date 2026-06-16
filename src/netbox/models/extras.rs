//! Extras models: journal entries, tags, etc.

use serde::{Deserialize, Serialize};

use super::common::Choice;

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
