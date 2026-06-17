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
                .map(str::to_lowercase)
                .is_some_and(|d| d == q || d.contains(&q))
    }

    /// A strict, identity-level match: case-insensitive equality on `name` or
    /// `slug`, or `id` equality when `query` parses to a number. Unlike
    /// [`matches`](Self::matches) it never substring-matches `display`, so a
    /// reference like `ci-site` won't match a prefix sibling such as `ci-site2`.
    /// Scope disambiguation prefers this and only falls back to the looser
    /// [`matches`](Self::matches) when nothing matches exactly (keeping
    /// `--vrf <rd>`, which relies on the display substring, working).
    pub fn matches_exact(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return false;
        }
        let eq = |s: &Option<String>| s.as_deref().is_some_and(|x| x.to_lowercase() == q);
        eq(&self.name) || eq(&self.slug) || q.parse::<u64>().is_ok_and(|n| n == self.id)
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
    fn matches_exact_is_strict_no_display_substring() {
        let site: BriefObject = serde_json::from_value(
            json!({"id": 2, "name": "CI Site", "slug": "ci-site", "display": "CI Site"}),
        )
        .unwrap();
        assert!(site.matches_exact("ci-site")); // slug, case-insensitive
        assert!(site.matches_exact("CI Site")); // name
        assert!(site.matches_exact("2")); // id
        // A prefix sibling's reference must NOT match this one (the bug).
        assert!(!site.matches_exact("ci-site2"));
        assert!(!site.matches_exact("")); // empty never matches

        // And, conversely, `ci-site` must not match the sibling whose display
        // happens to contain it as a substring.
        let sibling: BriefObject = serde_json::from_value(
            json!({"id": 3, "name": "CI Site 2", "slug": "ci-site2", "display": "CI Site 2"}),
        )
        .unwrap();
        assert!(!sibling.matches_exact("ci-site"));
        assert!(sibling.matches_exact("ci-site2"));

        // A VRF's RD lives only in `display`, so exact does NOT match it — that
        // case relies on the looser `matches` fallback in `retain_scope`.
        let vrf: BriefObject =
            serde_json::from_value(json!({"id": 1, "name": "blue", "display": "blue (65000:1)"}))
                .unwrap();
        assert!(vrf.matches_exact("blue")); // name still matches exactly
        assert!(!vrf.matches_exact("65000:1")); // RD substring does not
    }

    #[test]
    fn choice_deserializes() {
        let c: Choice<String> =
            serde_json::from_value(json!({"value": "active", "label": "Active"})).unwrap();
        assert_eq!(c.value, "active");
        assert_eq!(c.label, "Active");
    }
}
