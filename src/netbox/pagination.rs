//! Paginated NetBox list responses.
//!
//! NetBox list endpoints return `count`, `next`, `previous`, and `results`.
//! Clients page through with `limit`/`offset` (default page size 50; default
//! maximum 1000).

use serde::Deserialize;

/// One page of a NetBox list response.
#[derive(Debug, Clone, Deserialize)]
pub struct Page<T> {
    /// Total number of objects matching the query, across all pages.
    pub count: usize,

    /// URL of the next page, if any.
    #[serde(default)]
    pub next: Option<String>,

    /// URL of the previous page, if any.
    #[serde(default)]
    pub previous: Option<String>,

    /// The objects on this page. (NetBox always includes this key.)
    pub results: Vec<T>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_a_page() {
        let json = r#"{
            "count": 2,
            "next": "http://nb/api/dcim/sites/?offset=2",
            "previous": null,
            "results": [{"id": 1}, {"id": 2}]
        }"#;

        #[derive(Deserialize)]
        struct Item {
            id: u64,
        }

        let page: Page<Item> = serde_json::from_str(json).unwrap();
        assert_eq!(page.count, 2);
        assert!(page.next.is_some());
        assert!(page.previous.is_none());
        assert_eq!(page.results.len(), 2);
        assert_eq!(page.results[1].id, 2);
    }
}
