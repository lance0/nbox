//! Small filtering helpers shared across the flattened detail views.
//!
//! NetBox serializers report empty strings and zero relation counts rather than
//! omitting them; the views drop those (they are noise, not information). These
//! two helpers express that "treat empty/zero as absent" rule in one place so
//! every view filters identically.

/// An owned string, mapped to `None` when empty. Designed for
/// `opt_field.and_then(non_empty)`, dropping a `Some("")` from the wire.
pub(crate) fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

/// Keep a count only when it is present and non-zero (zero counts are noise).
pub(crate) fn non_zero(count: Option<u64>) -> Option<u64> {
    count.filter(|&n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_drops_empty_string() {
        assert_eq!(non_empty(String::new()), None);
        assert_eq!(non_empty("x".to_string()), Some("x".to_string()));
    }

    #[test]
    fn non_empty_composes_with_and_then() {
        let present: Option<String> = Some("kept".to_string());
        let blank: Option<String> = Some(String::new());
        let absent: Option<String> = None;
        assert_eq!(present.and_then(non_empty), Some("kept".to_string()));
        assert_eq!(blank.and_then(non_empty), None);
        assert_eq!(absent.and_then(non_empty), None);
    }

    #[test]
    fn non_zero_drops_zero_and_none() {
        assert_eq!(non_zero(Some(3)), Some(3));
        assert_eq!(non_zero(Some(0)), None);
        assert_eq!(non_zero(None), None);
    }
}
