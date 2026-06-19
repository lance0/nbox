//! Cache keys: the profile partition and the within-profile object key.
//!
//! A cached value is addressed by two strings — a *partition* (which connection
//! it belongs to) and a *key* (which object within it). [`profile_partition`]
//! builds the former; [`CacheKey`] the latter. The orchestrator
//! ([`super::Cache`]) holds the partition and prepends it, so call sites name
//! only the object.

use crate::config::BackendKind;
use crate::netbox::search::ObjectKind;

/// A stable identifier for one connection's cache partition: the profile name,
/// its normalized base URL, and the read backend.
///
/// Re-pointing a profile's URL or switching REST↔GraphQL changes this string, so
/// cached view models can never bleed across distinct targets. The NetBox version
/// is deliberately *not* part of the key — cached values are nbox's own assembled
/// structs (not NetBox's raw responses), so a version difference can't corrupt a
/// deserialized view, and leaving it out keeps reads stable across a probe.
pub fn profile_partition(name: &str, base_url: &str, backend: BackendKind) -> String {
    let url = base_url.trim_end_matches('/');
    format!("{name}|{url}|{}", backend.as_str())
}

/// A within-profile cache key: a namespace plus an object reference. Opaque and
/// stable — equal inputs always produce equal keys. The partition is added by the
/// [`Cache`](super::Cache).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey(String);

impl CacheKey {
    /// A detail view addressed by kind + numeric id — the TUI's primary
    /// navigation key, and the form cross-object jumps resolve to.
    pub fn detail(kind: ObjectKind, id: u64) -> Self {
        Self(format!("detail|{}|id:{id}", kind.as_str()))
    }

    /// A detail view addressed by a user reference (slug / cidr / address / vid).
    ///
    /// The reference is only trimmed, never case-folded: NetBox object names can
    /// be case-sensitive, so `Edge01` and `edge01` cache separately (two correct
    /// entries) rather than risk collapsing two distinct objects onto one key.
    /// The id form ([`detail`](Self::detail)) is a separate key — a ref lookup
    /// and an id lookup of the same object are not deduplicated.
    pub fn detail_ref(kind: ObjectKind, value: &str) -> Self {
        Self(format!("detail|{}|ref:{}", kind.as_str(), value.trim()))
    }

    /// A key for an MCP `nbox_get` object lookup: the kind label, the reference,
    /// and any disambiguators folded into `scope` (vrf/site/group), so e.g. the
    /// same CIDR resolved in two VRFs caches separately. `kind` is the caller's
    /// own slug (the MCP `GetKind`), so this doesn't depend on `ObjectKind`.
    pub fn object(kind: &str, reference: &str, scope: &str) -> Self {
        Self(format!("get|{kind}|ref:{}|{scope}", reference.trim()))
    }

    /// The opaque key string (what the store sees).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_separates_url_and_backend() {
        let a = profile_partition("prod", "https://nb.example/", BackendKind::Rest);
        let c = profile_partition("prod", "https://nb.example/", BackendKind::Graphql);
        let d = profile_partition("prod", "https://other.example/", BackendKind::Rest);
        assert_ne!(a, c, "backend segregates");
        assert_ne!(a, d, "base url segregates");
    }

    #[test]
    fn partition_is_trailing_slash_insensitive() {
        let with = profile_partition("p", "https://nb/", BackendKind::Rest);
        let without = profile_partition("p", "https://nb", BackendKind::Rest);
        assert_eq!(with, without);
    }

    #[test]
    fn detail_key_distinguishes_kind_and_id() {
        let dev = CacheKey::detail(ObjectKind::Device, 7);
        let site = CacheKey::detail(ObjectKind::Site, 7);
        let dev2 = CacheKey::detail(ObjectKind::Device, 8);
        assert_ne!(dev, site);
        assert_ne!(dev, dev2);
        assert_eq!(dev, CacheKey::detail(ObjectKind::Device, 7), "stable");
    }

    #[test]
    fn detail_ref_trims_but_does_not_case_fold() {
        let a = CacheKey::detail_ref(ObjectKind::Device, "  edge01 ");
        let b = CacheKey::detail_ref(ObjectKind::Device, "edge01");
        assert_eq!(a, b, "surrounding whitespace is normalized away");
        let upper = CacheKey::detail_ref(ObjectKind::Device, "Edge01");
        assert_ne!(
            a, upper,
            "case is preserved to avoid collapsing distinct names"
        );
    }

    #[test]
    fn detail_id_and_ref_are_distinct_keys() {
        let by_id = CacheKey::detail(ObjectKind::Device, 7);
        let by_ref = CacheKey::detail_ref(ObjectKind::Device, "7");
        assert_ne!(by_id, by_ref);
    }

    #[test]
    fn object_key_includes_scope_so_disambiguators_dont_collide() {
        let vrf_a = CacheKey::object("prefix", "10.0.0.0/24", "vrf=a;site=;group=");
        let vrf_b = CacheKey::object("prefix", "10.0.0.0/24", "vrf=b;site=;group=");
        assert_ne!(vrf_a, vrf_b, "same CIDR in two VRFs must not share a key");
        // Stable + reference-trimmed.
        assert_eq!(
            CacheKey::object("device", "  edge01 ", "vrf=;site=;group="),
            CacheKey::object("device", "edge01", "vrf=;site=;group=")
        );
    }
}
