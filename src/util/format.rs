//! Formatting helpers.

/// Convert a NetBox API object URL into its web UI URL by dropping the leading
/// `/api` path segment (e.g. `…/api/dcim/devices/1/` → `…/dcim/devices/1/`).
///
/// This is the single place API→web URL conversion happens; `open`/copy-link and
/// search results all route through it.
pub fn api_to_web_url(api_url: &str) -> String {
    api_url.replacen("/api/", "/", 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_api_segment() {
        assert_eq!(
            api_to_web_url("https://nb.example.com/api/dcim/devices/1/"),
            "https://nb.example.com/dcim/devices/1/"
        );
    }

    #[test]
    fn leaves_non_api_urls_unchanged() {
        assert_eq!(
            api_to_web_url("https://nb.example.com/dcim/devices/1/"),
            "https://nb.example.com/dcim/devices/1/"
        );
    }

    #[test]
    fn only_first_api_segment_is_replaced() {
        assert_eq!(
            api_to_web_url("https://h/api/ipam/prefixes/api-test/"),
            "https://h/ipam/prefixes/api-test/"
        );
    }
}
