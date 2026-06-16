//! Shared model types: brief relations, choices, and tags.

use serde::{Deserialize, Serialize};

/// A nested "brief" representation of a related object.
///
/// NetBox embeds related objects as `{id, url, display, ...}`; depending on the
/// object type one of `name`/`slug` (or, for IPs, `address`) carries the label.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BriefObject {
    pub id: u64,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub display: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
}

impl BriefObject {
    /// Best available human label: `display`, else `name`, else `slug`, else `#id`.
    pub fn label(&self) -> String {
        self.display
            .clone()
            .or_else(|| self.name.clone())
            .or_else(|| self.slug.clone())
            .unwrap_or_else(|| format!("#{}", self.id))
    }

    /// Whether this object matches a user-supplied scope reference, case-insensitively.
    /// Matches `name` or `slug` exactly, or `display` exactly or by substring (the
    /// latter catches values embedded in a label, e.g. a VRF's RD in its display).
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return false;
        }
        let eq = |s: &Option<String>| s.as_deref().is_some_and(|x| x.to_lowercase() == q);
        eq(&self.name)
            || eq(&self.slug)
            || self
                .display
                .as_deref()
                .map(|d| d.to_lowercase())
                .is_some_and(|d| d == q || d.contains(&q))
    }
}

/// A NetBox choice field: `{value, label}` (e.g. status, role).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Choice<T> {
    pub value: T,
    pub label: String,
}

/// A tag in its nested representation.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tag {
    pub id: u64,
    pub name: String,
    pub slug: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn brief_object_label_prefers_display_then_name_then_slug() {
        let with_display: BriefObject =
            serde_json::from_value(json!({"id": 1, "display": "iad1", "name": "iad1-name"}))
                .unwrap();
        assert_eq!(with_display.label(), "iad1");

        let with_name: BriefObject =
            serde_json::from_value(json!({"id": 2, "name": "rack-12"})).unwrap();
        assert_eq!(with_name.label(), "rack-12");

        let bare: BriefObject = serde_json::from_value(json!({"id": 7})).unwrap();
        assert_eq!(bare.label(), "#7");
    }

    #[test]
    fn matches_is_case_insensitive_across_fields() {
        let vrf: BriefObject =
            serde_json::from_value(json!({"id": 1, "name": "blue", "display": "blue (65000:1)"}))
                .unwrap();
        assert!(vrf.matches("blue")); // name
        assert!(vrf.matches("BLUE")); // case-insensitive
        assert!(vrf.matches("65000:1")); // substring of display (the RD)
        assert!(!vrf.matches("red"));
        assert!(!vrf.matches("")); // empty never matches

        let site: BriefObject =
            serde_json::from_value(json!({"id": 2, "name": "IAD1", "slug": "iad1"})).unwrap();
        assert!(site.matches("iad1")); // slug
    }

    #[test]
    fn choice_deserializes() {
        let c: Choice<String> =
            serde_json::from_value(json!({"value": "active", "label": "Active"})).unwrap();
        assert_eq!(c.value, "active");
        assert_eq!(c.label, "Active");
    }
}
