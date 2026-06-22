//! Shared model types: brief relations, choices, and tags.

use serde::{Deserialize, Serialize};

/// A nested "brief" representation of a related object.
///
/// NetBox embeds related objects as `{id, url, display, ...}`; depending on the
/// object type one of `name`/`slug` (or, for IPs, `address`) carries the label.
/// A VRF brief additionally carries `rd` (its route distinguisher), so a
/// `--vrf <rd>` reference can resolve by that dedicated field rather than by a
/// loose substring of `display`.
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
    /// Route distinguisher — present on VRF briefs (`None` otherwise).
    #[serde(default)]
    pub rd: Option<String>,
    /// The owning device — present on interface briefs (e.g. an interface's
    /// `connected_endpoints`/`link_peers`), `None` otherwise. Boxed to keep
    /// `BriefObject` a fixed size despite the self-reference.
    #[serde(default)]
    pub device: Option<Box<BriefObject>>,
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

    /// Like [`label`](Self::label) but preferring the stable `name` over `display`.
    /// NetBox's device `display` can append an asset tag (e.g. `edge01 (m075216)`);
    /// lists, diagrams, and titles read cleaner with the bare name, and the asset
    /// tag still lives in the object's own detail fields. Falls back to `display`,
    /// then `slug`, then `#id`.
    pub fn name_label(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.display.clone())
            .or_else(|| self.slug.clone())
            .unwrap_or_else(|| format!("#{}", self.id))
    }

    /// A cable endpoint's label as `device port` — the nested device's (bare) name
    /// plus this object's own (its interface name), so "where the cable goes" names
    /// the far *device*, not just its port. Falls back to the bare label when
    /// there's no device (a non-interface termination).
    pub fn endpoint_label(&self) -> String {
        match &self.device {
            Some(dev) => format!("{} {}", dev.name_label(), self.label()),
            None => self.label(),
        }
    }

    /// Whether this object matches a user-supplied scope reference, case-insensitively.
    /// Matches `name`/`slug`/`rd` exactly, or `display` exactly or by substring
    /// (the substring is a last-resort catch for values embedded in a label).
    /// A VRF's RD now matches via the dedicated `rd` field; the display-substring
    /// path remains only as a fallback.
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return false;
        }
        let eq = |s: &Option<String>| s.as_deref().is_some_and(|x| x.to_lowercase() == q);
        eq(&self.name)
            || eq(&self.slug)
            || eq(&self.rd)
            || self
                .display
                .as_deref()
                .map(str::to_lowercase)
                .is_some_and(|d| d == q || d.contains(&q))
    }

    /// A strict, identity-level match: case-insensitive equality on `name`,
    /// `slug`, or `rd`, or `id` equality when `query` parses to a number. Unlike
    /// [`matches`](Self::matches) it never substring-matches `display`, so a
    /// reference like `ci-site` won't match a prefix sibling such as `ci-site2`.
    /// Scope disambiguation prefers this and only falls back to the looser
    /// [`matches`](Self::matches) when nothing matches exactly. `--vrf <rd>` now
    /// resolves here, exactly, via the dedicated `rd` field.
    pub fn matches_exact(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return false;
        }
        let eq = |s: &Option<String>| s.as_deref().is_some_and(|x| x.to_lowercase() == q);
        eq(&self.name)
            || eq(&self.slug)
            || eq(&self.rd)
            || q.parse::<u64>().is_ok_and(|n| n == self.id)
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
    fn name_label_prefers_name_and_endpoint_label_strips_asset_tag() {
        // A device brief whose display carries an asset tag, with a bare name.
        let dev: BriefObject = serde_json::from_value(
            json!({"id": 1, "name": "dsr1-us-west-01a", "display": "dsr1-us-west-01a (m057545)"}),
        )
        .unwrap();
        // label() keeps NetBox's display; name_label() prefers the stable name.
        assert_eq!(dev.label(), "dsr1-us-west-01a (m057545)");
        assert_eq!(dev.name_label(), "dsr1-us-west-01a");

        // A connected endpoint (an interface brief) with that device nested.
        let endpoint: BriefObject = serde_json::from_value(json!({
            "id": 2, "display": "1/1/c13/1",
            "device": {"id": 1, "name": "dsr1-us-west-01a", "display": "dsr1-us-west-01a (m057545)"}
        }))
        .unwrap();
        // "where it goes" names the far device (bare name) + port, no asset tag.
        assert_eq!(endpoint.endpoint_label(), "dsr1-us-west-01a 1/1/c13/1");

        // No device nested → just the port label.
        let bare: BriefObject =
            serde_json::from_value(json!({"id": 3, "display": "xe-0/0/0"})).unwrap();
        assert_eq!(bare.endpoint_label(), "xe-0/0/0");
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
    fn brief_object_deserializes_rd() {
        // NetBox's VRF brief serializer includes `rd`.
        let vrf: BriefObject = serde_json::from_value(
            json!({"id": 1, "name": "blue", "rd": "65000:1", "display": "blue (65000:1)"}),
        )
        .unwrap();
        assert_eq!(vrf.rd.as_deref(), Some("65000:1"));

        // A non-VRF brief carries no `rd`.
        let site: BriefObject =
            serde_json::from_value(json!({"id": 2, "name": "IAD1", "slug": "iad1"})).unwrap();
        assert!(site.rd.is_none());
    }

    #[test]
    fn matches_vrf_by_exact_rd_field_not_display_substring() {
        // The RD lives in its own field now: `--vrf 65000:1` matches it exactly
        // via `rd`, and the looser `matches` agrees.
        let vrf: BriefObject = serde_json::from_value(
            json!({"id": 1, "name": "blue", "rd": "65000:1", "display": "blue (65000:1)"}),
        )
        .unwrap();
        assert!(vrf.matches_exact("65000:1")); // exact via the `rd` field
        assert!(vrf.matches_exact("65000:1".to_uppercase().as_str())); // case-insensitive
        assert!(vrf.matches("65000:1")); // loose path agrees too
        assert!(vrf.matches_exact("blue")); // name still matches

        // A different RD must not match.
        assert!(!vrf.matches_exact("65000:2"));
        assert!(!vrf.matches("65000:2"));
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

        // When a VRF brief carries no `rd` field, its RD lives only in `display`,
        // so an RD reference does NOT match exactly (the display substring is a
        // `matches`-only fallback). The proper RD-via-`rd`-field match is covered
        // by `matches_vrf_by_exact_rd_field_not_display_substring`.
        let vrf: BriefObject =
            serde_json::from_value(json!({"id": 1, "name": "blue", "display": "blue (65000:1)"}))
                .unwrap();
        assert!(vrf.matches_exact("blue")); // name still matches exactly
        assert!(!vrf.matches_exact("65000:1")); // RD substring of display does not
    }

    #[test]
    fn choice_deserializes() {
        let c: Choice<String> =
            serde_json::from_value(json!({"value": "active", "label": "Active"})).unwrap();
        assert_eq!(c.value, "active");
        assert_eq!(c.label, "Active");
    }
}
