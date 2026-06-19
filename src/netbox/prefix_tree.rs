//! Prefix-tree data: a read-only fetch of the IPAM prefix hierarchy for the TUI
//! tree view. One paginated list call — NetBox returns prefixes in tree order
//! with a `_depth` annotation — grouped by VRF, plus pure tree logic (depth
//! mapping, collapse/expand visibility) so the interactive parts are unit-testable
//! without a live NetBox. Strictly read-only.

use std::collections::HashSet;

use anyhow::Result;

use crate::netbox::client::NetBoxClient;
use crate::netbox::dashboard::utilization_pct;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::models::ipam::Prefix;
use crate::netbox::pagination::Page;

/// How many prefixes the tree fetches in one page. NetBox's default
/// `MAX_PAGE_SIZE` is 1000, so this returns the whole tree for all but the
/// largest instances in a single request (no hammering); larger trees are capped
/// and the view says so. The local cache (a later feature) will lift this.
const PREFIX_TREE_CAP: usize = 1000;

/// One prefix in the tree: the fields the view needs, flattened from the API
/// model. `depth` is the per-VRF nesting level (0 = a top-level prefix in its
/// table); `children` is NetBox's child-prefix count (drives the disclosure
/// marker — only a prefix with children can be collapsed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixNode {
    pub id: u64,
    pub prefix: String,
    /// The VRF label, or `None` for the global table. Nodes are grouped by this.
    pub vrf: Option<String>,
    /// The status value (`active`/`reserved`/…), for status coloring.
    pub status: Option<String>,
    pub depth: u64,
    pub children: u64,
    pub utilization: Option<u8>,
    pub description: String,
}

impl PrefixNode {
    /// True when this prefix has child prefixes and so can be collapsed/expanded.
    #[must_use]
    pub fn collapsible(&self) -> bool {
        self.children > 0
    }
}

/// The loaded prefix tree: nodes in display order (grouped by VRF, each VRF's
/// own tree order preserved) plus how many of the instance's prefixes are shown.
#[derive(Debug, Clone, Default)]
pub struct PrefixTreeData {
    /// Prefixes in render order: global table first, then each VRF, each group in
    /// NetBox's tree order so `depth` lines up with the parent above it.
    pub nodes: Vec<PrefixNode>,
    /// Total prefix count the instance reports (may exceed `nodes.len()`).
    pub total: usize,
}

impl PrefixTreeData {
    /// True when the instance has more prefixes than were fetched (the view shows
    /// a capped subset). Surfaced so the count never reads as exhaustive when it
    /// isn't.
    #[must_use]
    pub fn capped(&self) -> bool {
        self.total > self.nodes.len()
    }
}

/// Flatten API prefixes into tree nodes, grouped by VRF while preserving each
/// group's incoming (tree) order so `depth` still refers to the parent printed
/// above. A stable sort by VRF label groups the global table (key `""`, sorted
/// first) ahead of named VRFs without disturbing within-VRF order — and thus the
/// depth relationship. Pure + testable.
#[must_use]
pub fn build_nodes(prefixes: Vec<Prefix>) -> Vec<PrefixNode> {
    let mut nodes: Vec<PrefixNode> = prefixes
        .into_iter()
        .map(|p| PrefixNode {
            id: p.id,
            prefix: p.prefix,
            vrf: p
                .vrf
                .as_ref()
                .map(super::models::common::BriefObject::label),
            status: p.status.map(|s| s.value),
            depth: p.depth.unwrap_or(0),
            children: p.children.unwrap_or(0),
            utilization: p.utilization.as_ref().and_then(utilization_pct),
            description: p.description.unwrap_or_default(),
        })
        .collect();
    // Group by VRF (global first) without reordering within a group, so the
    // per-VRF depth chain stays intact.
    nodes.sort_by(|a, b| match (&a.vrf, &b.vrf) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(x), Some(y)) => x.cmp(y),
    });
    fill_child_coverage(&mut nodes);
    nodes
}

/// Parse the prefix length (the number after `/`) from a CIDR. Pure.
fn prefix_len(cidr: &str) -> Option<u32> {
    cidr.rsplit('/').next()?.trim().parse().ok()
}

/// Fill in each container prefix's utilization the API no longer provides
/// (NetBox 4.5 dropped the `utilization` field): for a prefix with child
/// prefixes, utilization is the fraction of its address space those direct
/// children cover — `Σ 2^(parent_len − child_len)` — which is exactly NetBox's
/// container-utilization, computed for free from the already-fetched tree (no
/// extra calls; works on every NetBox version). A prefix that already has an
/// API-provided value (older NetBox) keeps it; a leaf (no children) is left
/// `None` (its IP-level utilization would need per-prefix queries). The nodes
/// are in VRF-grouped tree order, so a subtree is the contiguous run of deeper
/// rows following a node until the depth returns to its level. Pure.
fn fill_child_coverage(nodes: &mut [PrefixNode]) {
    for i in 0..nodes.len() {
        if nodes[i].utilization.is_some() {
            continue; // keep an API-provided value (NetBox ≤ 4.4)
        }
        let Some(parent_len) = prefix_len(&nodes[i].prefix) else {
            continue;
        };
        let parent_depth = nodes[i].depth;
        let mut fraction = 0f64;
        let mut has_child = false;
        for child in &nodes[(i + 1)..] {
            if child.depth <= parent_depth {
                break; // left this prefix's subtree (or crossed a VRF boundary)
            }
            if child.depth == parent_depth + 1
                && let Some(child_len) = prefix_len(&child.prefix)
                && child_len >= parent_len
            {
                fraction += 2f64.powi(
                    i32::try_from(parent_len).unwrap_or(0) - i32::try_from(child_len).unwrap_or(0),
                );
                has_child = true;
            }
        }
        if has_child {
            let pct = (fraction * 100.0).round().clamp(0.0, 100.0);
            // pct is already clamped to 0..=100 → fits u8.
            nodes[i].utilization = Some(pct as u8);
        }
    }
}

/// The indices of `nodes` that are currently visible given the set of collapsed
/// prefix ids: a node is hidden when it sits under a collapsed ancestor (a
/// deeper node following it before the depth returns to the ancestor's level).
/// VRF boundaries need no special-casing — each VRF restarts at depth 0, which is
/// at or above any collapse threshold, so it always reopens visibility. Pure.
#[must_use]
pub fn visible_indices<S: std::hash::BuildHasher>(
    nodes: &[PrefixNode],
    collapsed: &HashSet<u64, S>,
) -> Vec<usize> {
    let mut out = Vec::new();
    // While set, hide every following node deeper than this depth (the subtree of
    // a collapsed node); cleared the moment depth returns to that level or above.
    let mut hide_deeper_than: Option<u64> = None;
    for (i, n) in nodes.iter().enumerate() {
        if let Some(threshold) = hide_deeper_than {
            if n.depth > threshold {
                continue;
            }
            hide_deeper_than = None;
        }
        out.push(i);
        if n.collapsible() && collapsed.contains(&n.id) {
            hide_deeper_than = Some(n.depth);
        }
    }
    out
}

/// Fetch the prefix tree: one capped page of prefixes in NetBox's default tree
/// order (which carries the `_depth` annotation), flattened and grouped by VRF.
/// Read-only.
pub async fn load_prefix_tree(client: &NetBoxClient) -> Result<PrefixTreeData> {
    let params = vec![("limit", PREFIX_TREE_CAP.to_string())];
    let page: Page<Prefix> = client.get(Endpoint::Prefixes.path(), &params).await?;
    let total = page.count;
    Ok(PrefixTreeData {
        nodes: build_nodes(page.results),
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prefix(id: u64, cidr: &str, depth: u64, children: u64, vrf: Option<&str>) -> Prefix {
        serde_json::from_value(json!({
            "id": id,
            "url": format!("http://nb/api/ipam/prefixes/{id}/"),
            "prefix": cidr,
            "status": {"value": "active", "label": "Active"},
            "vrf": vrf.map(|v| json!({"id": 1, "name": v, "display": v})),
            "children": children,
            "_depth": depth,
        }))
        .unwrap()
    }

    #[test]
    fn prefix_len_parses_the_mask() {
        assert_eq!(prefix_len("10.0.0.0/16"), Some(16));
        assert_eq!(prefix_len("2001:db8::/48"), Some(48));
        assert_eq!(prefix_len("garbage"), None);
    }

    #[test]
    fn child_coverage_fills_container_utilization() {
        // A /16 fully carved into two /17s → 100% covered (NetBox dropped the API
        // field on 4.5, so we compute it from the tree).
        let nodes = build_nodes(vec![
            prefix(1, "10.0.0.0/16", 0, 2, None),
            prefix(2, "10.0.0.0/17", 1, 0, None),
            prefix(3, "10.0.128.0/17", 1, 0, None),
        ]);
        assert_eq!(nodes[0].utilization, Some(100));
        // Leaves have no children → no computed utilization (would need IP queries).
        assert_eq!(nodes[1].utilization, None);
        assert_eq!(nodes[2].utilization, None);
    }

    #[test]
    fn child_coverage_is_partial_for_sparse_containers() {
        // A /16 with a single /18 child covers 1/4 of the space → 25%.
        let nodes = build_nodes(vec![
            prefix(1, "10.0.0.0/16", 0, 1, None),
            prefix(2, "10.0.0.0/18", 1, 0, None),
        ]);
        assert_eq!(nodes[0].utilization, Some(25));
        // A grandchild doesn't double-count against the grandparent (only direct
        // children): /16 → /18 → /20; the /16 still reads 25%.
        let nodes = build_nodes(vec![
            prefix(1, "10.0.0.0/16", 0, 1, None),
            prefix(2, "10.0.0.0/18", 1, 1, None),
            prefix(3, "10.0.0.0/20", 2, 0, None),
        ]);
        assert_eq!(nodes[0].utilization, Some(25), "only direct children count");
    }

    #[test]
    fn api_provided_utilization_is_kept_over_computed() {
        // An older NetBox that still serves `utilization` keeps its richer value.
        let p: Prefix = serde_json::from_value(json!({
            "id": 1, "url": "u", "prefix": "10.0.0.0/16", "children": 1, "_depth": 0,
            "utilization": 73,
        }))
        .unwrap();
        let nodes = build_nodes(vec![p, prefix(2, "10.0.0.0/24", 1, 0, None)]);
        assert_eq!(nodes[0].utilization, Some(73));
    }

    #[test]
    fn build_nodes_maps_depth_children_and_status() {
        let nodes = build_nodes(vec![prefix(1, "10.0.0.0/8", 0, 2, None)]);
        assert_eq!(nodes.len(), 1);
        let n = &nodes[0];
        assert_eq!(n.prefix, "10.0.0.0/8");
        assert_eq!(n.depth, 0);
        assert_eq!(n.children, 2);
        assert_eq!(n.status.as_deref(), Some("active"));
        assert!(n.vrf.is_none());
        assert!(n.collapsible());
    }

    #[test]
    fn build_nodes_groups_global_first_then_vrfs_preserving_order() {
        // VRFs interleaved, but each VRF's own rows already in NetBox tree order
        // (parent before child): a blue parent, a global prefix, then the blue child.
        let nodes = build_nodes(vec![
            prefix(1, "10.0.0.0/8", 0, 1, Some("blue")),
            prefix(2, "10.0.0.0/8", 0, 1, None),
            prefix(3, "10.1.0.0/16", 1, 0, Some("blue")),
        ]);
        // Global (None) sorts ahead of "blue"; within "blue", input order holds
        // (so the /8 parent still precedes the /16 child → depth stays valid).
        let order: Vec<(&str, Option<&str>)> = nodes
            .iter()
            .map(|n| (n.prefix.as_str(), n.vrf.as_deref()))
            .collect();
        assert_eq!(
            order,
            vec![
                ("10.0.0.0/8", None),
                ("10.0.0.0/8", Some("blue")),
                ("10.1.0.0/16", Some("blue")),
            ]
        );
    }

    #[test]
    fn visible_indices_hides_a_collapsed_subtree() {
        // 10.0.0.0/8 (d0) → 10.0.0.0/16 (d1) → 10.0.0.0/24 (d2); then 10.1.0.0/16 (d1).
        let nodes = build_nodes(vec![
            prefix(1, "10.0.0.0/8", 0, 1, None),
            prefix(2, "10.0.0.0/16", 1, 1, None),
            prefix(3, "10.0.0.0/24", 2, 0, None),
            prefix(4, "10.1.0.0/16", 1, 0, None),
        ]);
        // Nothing collapsed → all visible.
        assert_eq!(visible_indices(&nodes, &HashSet::new()), vec![0, 1, 2, 3]);
        // Collapse the /16 (id 2): its child /24 (id 3) hides; the sibling /16
        // (id 4) at the same depth stays visible.
        let collapsed: HashSet<u64> = [2].into_iter().collect();
        assert_eq!(visible_indices(&nodes, &collapsed), vec![0, 1, 3]);
        // Collapse the top /8 (id 1): everything under it hides.
        let collapsed: HashSet<u64> = [1].into_iter().collect();
        assert_eq!(visible_indices(&nodes, &collapsed), vec![0]);
    }

    #[test]
    fn collapsing_a_leaf_is_a_no_op() {
        // A childless node can't hide anything even if its id is in the set.
        let nodes = build_nodes(vec![
            prefix(1, "10.0.0.0/8", 0, 1, None),
            prefix(2, "10.0.0.0/24", 1, 0, None),
        ]);
        let collapsed: HashSet<u64> = [2].into_iter().collect();
        assert_eq!(visible_indices(&nodes, &collapsed), vec![0, 1]);
    }

    #[test]
    fn vrf_boundary_resets_visibility() {
        // A collapsed last node of the global group must not hide the first node
        // of the next VRF (it restarts at depth 0).
        let nodes = build_nodes(vec![
            prefix(1, "10.0.0.0/8", 0, 1, None),
            prefix(2, "10.0.0.0/24", 1, 0, None),
            prefix(3, "192.168.0.0/16", 0, 0, Some("blue")),
        ]);
        let collapsed: HashSet<u64> = [1].into_iter().collect();
        // /24 (child of collapsed /8) hides; the VRF prefix at depth 0 stays.
        let vis = visible_indices(&nodes, &collapsed);
        let shown: Vec<&str> = vis.iter().map(|&i| nodes[i].prefix.as_str()).collect();
        assert_eq!(shown, vec!["10.0.0.0/8", "192.168.0.0/16"]);
    }

    #[test]
    fn capped_reflects_total_vs_fetched() {
        let data = PrefixTreeData {
            nodes: build_nodes(vec![prefix(1, "10.0.0.0/8", 0, 0, None)]),
            total: 5,
        };
        assert!(data.capped(), "5 total but 1 fetched → capped");
        let data = PrefixTreeData {
            nodes: build_nodes(vec![prefix(1, "10.0.0.0/8", 0, 0, None)]),
            total: 1,
        };
        assert!(!data.capped());
    }
}
