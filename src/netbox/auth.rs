//! NetBox API authentication.
//!
//! NetBox 4.5 added v2 API tokens (recommended): v2 auth uses
//! `Authorization: Bearer nbt_<key>.<token>`, while legacy v1 tokens use
//! `Authorization: Token <token>`. [`AuthScheme::Auto`] detects which to use.

use serde::{Deserialize, Serialize};

/// How to format the `Authorization` header for a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthScheme {
    /// Detect from the token shape: v2 (`nbt_*.*`) → `Bearer`, else `Token`.
    #[default]
    Auto,
    /// Always `Authorization: Bearer <token>`.
    Bearer,
    /// Always `Authorization: Token <token>`.
    Token,
}

impl AuthScheme {
    /// Build the `Authorization` header value for `token` under this scheme.
    pub fn header_value(&self, token: &str) -> String {
        match self {
            AuthScheme::Bearer => format!("Bearer {token}"),
            AuthScheme::Token => format!("Token {token}"),
            AuthScheme::Auto => {
                if Self::looks_like_v2(token) {
                    format!("Bearer {token}")
                } else {
                    format!("Token {token}")
                }
            }
        }
    }

    /// A v2 token is shaped like `nbt_<key>.<secret>`.
    fn looks_like_v2(token: &str) -> bool {
        token.starts_with("nbt_") && token.contains('.')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_detects_v2_token_as_bearer() {
        let scheme = AuthScheme::Auto;
        assert_eq!(
            scheme.header_value("nbt_abc.def123"),
            "Bearer nbt_abc.def123"
        );
    }

    #[test]
    fn auto_falls_back_to_legacy_token() {
        let scheme = AuthScheme::Auto;
        assert_eq!(
            scheme.header_value("0123456789abcdef"),
            "Token 0123456789abcdef"
        );
    }

    #[test]
    fn explicit_schemes_are_honored() {
        assert_eq!(AuthScheme::Bearer.header_value("x"), "Bearer x");
        assert_eq!(AuthScheme::Token.header_value("x"), "Token x");
        // Bearer is forced even for a legacy-shaped token.
        assert_eq!(AuthScheme::Bearer.header_value("legacy"), "Bearer legacy");
    }

    #[test]
    fn serde_uses_lowercase_names() {
        assert_eq!(
            serde_json::to_string(&AuthScheme::Auto).unwrap(),
            "\"auto\""
        );
        let parsed: AuthScheme = serde_json::from_str("\"bearer\"").unwrap();
        assert_eq!(parsed, AuthScheme::Bearer);
    }

    #[test]
    fn default_is_auto() {
        assert_eq!(AuthScheme::default(), AuthScheme::Auto);
    }
}
