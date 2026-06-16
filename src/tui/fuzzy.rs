//! Client-side fuzzy ranking of in-memory lists (via `nucleo`).
//!
//! Used to filter/rank already-fetched results live as the user types — purely
//! presentational, never on the network path.

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};

/// Return the indices of `items` that fuzzy-match `query`, best score first.
/// An empty query returns every index in original order.
pub fn rank(query: &str, items: &[&str]) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..items.len()).collect();
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

    let mut scored: Vec<(usize, u32)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let mut buf = Vec::new();
            let haystack = Utf32Str::new(s, &mut buf);
            pattern
                .score(haystack, &mut matcher)
                .map(|score| (i, score))
        })
        .collect();

    // Highest score first; ties keep original order.
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_all_in_order() {
        assert_eq!(rank("", &["a", "b", "c"]), vec![0, 1, 2]);
    }

    #[test]
    fn matches_are_kept_and_nonmatches_dropped() {
        let items = ["edge01", "core02", "edge-router"];
        let ranked = rank("edge", &items);
        assert!(ranked.contains(&0));
        assert!(ranked.contains(&2));
        assert!(!ranked.contains(&1), "core02 should not match 'edge'");
    }

    #[test]
    fn closer_matches_rank_higher() {
        let items = ["edge-distribution-01", "edge01"];
        let ranked = rank("edge01", &items);
        assert_eq!(
            ranked.first(),
            Some(&1),
            "exact-ish 'edge01' should rank first"
        );
    }
}
