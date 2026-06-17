//! Tag listing view for `nbox tags` (plain + JSON).

use schemars::JsonSchema;
use serde::Serialize;

use crate::netbox::models::extras::TagInfo;

/// One tag row.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TagRow {
    pub name: String,
    pub slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

/// A list of tags.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TagsView {
    pub tags: Vec<TagRow>,
}

impl TagsView {
    /// Normalize wire [`TagInfo`] records into rows.
    pub fn from_models(tags: Vec<TagInfo>) -> Self {
        let tags = tags
            .into_iter()
            .map(|t| TagRow {
                name: t.name,
                slug: t.slug,
                color: t.color,
                count: t.tagged_items,
            })
            .collect();
        Self { tags }
    }

    /// Render one `slug  name  (N)` line per tag.
    pub fn to_plain(&self) -> String {
        if self.tags.is_empty() {
            return "no tags".to_string();
        }
        self.tags
            .iter()
            .map(|t| match t.count {
                Some(n) => format!("{}  {}  ({n})", t.slug, t.name),
                None => format!("{}  {}", t.slug, t.name),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_tag_rows() {
        let tags: Vec<TagInfo> = vec![
            serde_json::from_value(json!({
                "id": 1, "name": "Critical", "slug": "critical",
                "color": "ff0000", "tagged_items": 12
            }))
            .unwrap(),
        ];
        let view = TagsView::from_models(tags);
        assert_eq!(view.tags[0].slug, "critical");
        assert_eq!(view.to_plain(), "critical  Critical  (12)");
    }

    #[test]
    fn empty_tags_is_explicit() {
        assert_eq!(TagsView::from_models(vec![]).to_plain(), "no tags");
    }
}
