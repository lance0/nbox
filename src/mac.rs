//! MAC address normalization for the `nbox mac` reverse-lookup.
//!
//! Operators paste MACs in every format — `aa:bb:cc:dd:ee:ff`,
//! `AA:BB:CC:DD:EE:FF`, `aabb.ccdd.eeff` (Cisco), `aa-bb-cc-dd-ee-ff`,
//! `aabbccddeeff` (bare), even with a trailing `/48`. NetBox stores and matches
//! the canonical lowercase colon-separated form, so the input is normalized
//! before it's sent as the `mac_address=` filter — a miss here means a
//! reverse-lookup that *should* hit returns nothing.

/// Normalize a MAC address to NetBox's canonical form: lowercase, colon-separated
/// octets (`aa:bb:cc:dd:ee:ff`). Accepts the common separator forms (`:` `-` `.`),
/// the bare 12-hex-digit form, and a trailing `/48` (the interface suffix some
/// tools append). Returns `None` for anything that isn't exactly six octets of
/// hex after stripping separators — the caller surfaces that as a usage error.
///
/// # Examples
/// ```
/// assert_eq!(nbox::mac::normalize("aa:bb:cc:dd:ee:ff"), Some("aa:bb:cc:dd:ee:ff".to_string()));
/// assert_eq!(nbox::mac::normalize("AABB.CCDD.EEFF"), Some("aa:bb:cc:dd:ee:ff".to_string()));
/// assert_eq!(nbox::mac::normalize("aa-bb-cc-dd-ee-ff/48"), Some("aa:bb:cc:dd:ee:ff".to_string()));
/// assert_eq!(nbox::mac::normalize("aabbccddeeff"), Some("aa:bb:cc:dd:ee:ff".to_string()));
/// assert_eq!(nbox::mac::normalize("not-a-mac"), None);
/// ```
#[must_use]
pub fn normalize(input: &str) -> Option<String> {
    // Strip a trailing interface-length suffix (`/48`) — some tools paste it.
    let s = input.trim().trim_end_matches("/48");
    // Keep only hex digits; any non-hex char is treated as a separator.
    let hex: String = s
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    // A MAC is exactly six octets (12 hex digits).
    if hex.len() != 12 || hex.chars().any(|c| !c.is_ascii_hexdigit()) {
        return None;
    }
    let octets: Vec<&str> = (0..6).map(|i| &hex[i * 2..i * 2 + 2]).collect();
    Some(octets.join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_colon_separated() {
        assert_eq!(
            normalize("aa:bb:cc:dd:ee:ff"),
            Some("aa:bb:cc:dd:ee:ff".to_string())
        );
    }

    #[test]
    fn lowercases_uppercase_hex() {
        assert_eq!(
            normalize("AA:BB:CC:DD:EE:FF"),
            Some("aa:bb:cc:dd:ee:ff".to_string())
        );
    }

    #[test]
    fn normalizes_cisco_dotted_form() {
        assert_eq!(
            normalize("aabb.ccdd.eeff"),
            Some("aa:bb:cc:dd:ee:ff".to_string())
        );
    }

    #[test]
    fn normalizes_dash_separated() {
        assert_eq!(
            normalize("aa-bb-cc-dd-ee-ff"),
            Some("aa:bb:cc:dd:ee:ff".to_string())
        );
    }

    #[test]
    fn normalizes_bare_twelve_hex_digits() {
        assert_eq!(
            normalize("aabbccddeeff"),
            Some("aa:bb:cc:dd:ee:ff".to_string())
        );
    }

    #[test]
    fn strips_a_trailing_interface_length_suffix() {
        assert_eq!(
            normalize("aa:bb:cc:dd:ee:ff/48"),
            Some("aa:bb:cc:dd:ee:ff".to_string())
        );
    }

    #[test]
    fn rejects_a_non_mac() {
        assert_eq!(normalize("not-a-mac"), None);
    }

    #[test]
    fn rejects_too_few_digits() {
        assert_eq!(normalize("aa:bb:cc:dd:ee"), None);
    }

    #[test]
    fn rejects_too_many_digits() {
        assert_eq!(normalize("aa:bb:cc:dd:ee:ff:00"), None);
    }

    #[test]
    fn rejects_non_hex() {
        assert_eq!(normalize("aa:bb:cc:dd:ee:gg"), None);
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            normalize("  aa:bb:cc:dd:ee:ff  "),
            Some("aa:bb:cc:dd:ee:ff".to_string())
        );
    }
}
