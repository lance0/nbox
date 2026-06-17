//! Journal-entry view for `nbox journal` (plain + JSON).

use schemars::JsonSchema;
use serde::Serialize;

use crate::netbox::models::extras::JournalEntry;

/// One journal entry, flattened for display.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct JournalEntryRow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub comments: String,
}

/// A list of journal entries for an object.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct JournalView {
    pub entries: Vec<JournalEntryRow>,
}

impl JournalView {
    /// Normalize wire [`JournalEntry`] records into display rows.
    pub fn from_models(entries: Vec<JournalEntry>) -> Self {
        let entries = entries
            .into_iter()
            .map(|e| JournalEntryRow {
                created: e.created,
                kind: e.kind.map(|c| c.value),
                author: e.created_by.as_ref().and_then(author_label),
                comments: e.comments,
            })
            .collect();
        Self { entries }
    }

    /// Render entries as `created  kind  (author)` headers with indented comments.
    pub fn to_plain(&self) -> String {
        if self.entries.is_empty() {
            return "no journal entries".to_string();
        }
        let mut blocks = Vec::new();
        for e in &self.entries {
            let mut header = String::new();
            if let Some(c) = &e.created {
                header.push_str(c);
            }
            if let Some(k) = &e.kind {
                header.push_str(&format!("  {k}"));
            }
            if let Some(a) = &e.author {
                header.push_str(&format!("  ({a})"));
            }
            let body: String = e
                .comments
                .lines()
                .map(|l| format!("  {l}"))
                .collect::<Vec<_>>()
                .join("\n");
            blocks.push(if body.is_empty() {
                header
            } else {
                format!("{header}\n{body}")
            });
        }
        blocks.join("\n\n")
    }
}

/// Extract an author label from a journal entry's `created_by` (nested user or id).
fn author_label(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v
        .get("display")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("username").and_then(|x| x.as_str()))
        .or_else(|| v.get("name").and_then(|x| x.as_str()))
    {
        return Some(s.to_string());
    }
    v.as_u64().map(|n| format!("#{n}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_entries_with_author_and_comments() {
        let entries: Vec<JournalEntry> = vec![
            serde_json::from_value(json!({
                "id": 1, "created": "2024-01-02", "kind": {"value": "info", "label": "Info"},
                "created_by": {"username": "admin", "display": "admin"}, "comments": "rebooted"
            }))
            .unwrap(),
        ];
        let view = JournalView::from_models(entries);
        assert_eq!(view.entries[0].author.as_deref(), Some("admin"));
        let plain = view.to_plain();
        assert!(plain.contains("2024-01-02  info  (admin)"), "got: {plain}");
        assert!(plain.contains("\n  rebooted"));
    }

    #[test]
    fn empty_journal_is_explicit() {
        let view = JournalView::from_models(vec![]);
        assert_eq!(view.to_plain(), "no journal entries");
    }
}
