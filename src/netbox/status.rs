//! NetBox version probe (`/api/status/`) and minimum-version enforcement.
//!
//! nbox targets NetBox 4.2+ (the polymorphic `scope` model). The TUI calls
//! [`NetBoxClient::verify_compatible`] on launch to fail fast against older
//! instances and surface the version in the status line. One-shot CLI commands
//! skip the probe to avoid an extra round-trip per invocation.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::error::NboxError;
use crate::netbox::client::NetBoxClient;

/// Minimum supported NetBox major version.
pub const MIN_MAJOR: u32 = 4;
/// Minimum supported NetBox minor version.
pub const MIN_MINOR: u32 = 2;

/// The subset of `/api/status/` that nbox cares about. NetBox returns more keys
/// (plugins, workers, …); the rest are ignored.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Status {
    #[serde(rename = "netbox-version")]
    pub netbox_version: String,
    #[serde(
        rename = "django-version",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub django_version: Option<String>,
    #[serde(
        rename = "python-version",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub python_version: Option<String>,
}

impl NetBoxClient {
    /// Fetch `/api/status/`.
    pub async fn status(&self) -> Result<Status> {
        self.get("/api/status/", &[]).await
    }

    /// Probe the instance and fail if it is below the supported floor.
    pub async fn verify_compatible(&self) -> Result<Status> {
        let status = self.status().await?;
        if !meets_minimum(&status.netbox_version, MIN_MAJOR, MIN_MINOR) {
            bail!(
                "NetBox {} is unsupported; nbox requires {MIN_MAJOR}.{MIN_MINOR}+",
                status.netbox_version
            );
        }
        Ok(status)
    }

    /// Best-effort credential preflight against `/api/authentication-check/`
    /// (NetBox 4.5+). Answers "is this token valid, and who does it resolve to?"
    /// without inferring it from `/api/status/` — which is reachable *without* a
    /// valid token on instances configured with `LOGIN_REQUIRED=False`, so a bad
    /// token can hide behind a 200 status response. `nbox status` surfaces this as
    /// an explicit `token` verdict; the exit-code contract for the status fetch
    /// is unchanged (an auth-*required* instance still rejects the token at
    /// `/api/status/` with exit 3 before this runs).
    ///
    /// Never errors: a rejected token → [`AuthCheck::Invalid`], an absent endpoint
    /// (NetBox < 4.5 returns 404) or a non-auth failure → [`AuthCheck::Unverified`],
    /// so a missing/dedicated probe never fails an otherwise-good status report.
    /// The endpoint returns the flat `UserSerializer` body on 200 (NetBox 4.5+).
    pub async fn authentication_check(&self) -> AuthCheck {
        match self
            .get_optional::<AuthUser>("/api/authentication-check/", &[])
            .await
        {
            Ok(Some(user)) => AuthCheck::Valid {
                username: user.username,
                display: user.display,
            },
            // NetBox < 4.5 has no such endpoint → 404 → "unverified", not an error.
            Ok(None) => AuthCheck::Unverified {
                reason: "endpoint absent (NetBox < 4.5)".to_string(),
            },
            Err(err) => match err.chain().find_map(|e| e.downcast_ref::<NboxError>()) {
                // 401/403 → the token was rejected; carry the server's reason.
                Some(NboxError::Authentication(reason) | NboxError::PermissionDenied(reason)) => {
                    let reason = reason.strip_prefix(": ").unwrap_or(reason);
                    let reason = if reason.is_empty() {
                        "rejected by NetBox".to_string()
                    } else {
                        reason.to_string()
                    };
                    AuthCheck::Invalid { reason }
                }
                // Anything else (network, 5xx, …) → couldn't verify, don't guess.
                _ => AuthCheck::Unverified {
                    reason: format!("{err:#}"),
                },
            },
        }
    }
}

/// The outcome of a credential preflight ([`NetBoxClient::authentication_check`]).
/// Surfaced as the `token` field of `nbox status` / MCP `nbox_status`, giving an
/// operator/agent an unambiguous "is this token valid?" answer.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AuthCheck {
    /// The token authenticated; carries the identity it resolved to.
    Valid {
        /// The authenticated user's username.
        username: String,
        /// The user's display name (`username`, or `"username (full name)"`),
        /// when NetBox reports one distinct from the username.
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<String>,
    },
    /// The token was rejected (HTTP 401/403). Carries the server's reason.
    Invalid { reason: String },
    /// The preflight couldn't run: the endpoint is absent (NetBox < 4.5) or
    /// errored for a non-auth reason. The token is left unverified, not guessed.
    Unverified { reason: String },
}

impl AuthCheck {
    /// A one-line plain-text verdict for `nbox status`'s `token:` row, e.g.
    /// `valid (admin)`, `invalid/rejected (Invalid token)`, `unverified (...)`.
    #[must_use]
    pub fn plain(&self) -> String {
        match self {
            Self::Valid { username, display } => {
                let who = display
                    .as_deref()
                    .filter(|d| !d.is_empty())
                    .unwrap_or(username);
                format!("valid ({who})")
            }
            Self::Invalid { reason } => {
                if reason.is_empty() {
                    "invalid/rejected".to_string()
                } else {
                    format!("invalid/rejected ({reason})")
                }
            }
            Self::Unverified { reason } => {
                if reason.is_empty() {
                    "unverified".to_string()
                } else {
                    format!("unverified ({reason})")
                }
            }
        }
    }
}

/// The identity fields nbox reads from the `/api/authentication-check/` body
/// (the flat `UserSerializer` output, NetBox 4.5+). Only the fields the `token`
/// verdict needs are captured; the rest (`id`/`url`/`email`/`groups`/…) are
/// ignored by serde.
#[derive(Debug, Clone, Deserialize)]
struct AuthUser {
    username: String,
    #[serde(default)]
    display: Option<String>,
}

/// Whether `version` (e.g. `"4.5.5"`, `"4.2.0-dev"`) is at least `major.minor`.
pub fn meets_minimum(version: &str, min_major: u32, min_minor: u32) -> bool {
    parse_major_minor(version) >= (min_major, min_minor)
}

/// Extract `(major, minor)` from a version string, tolerating pre-release
/// suffixes like `-dev` or `-beta1`. Missing parts read as 0.
fn parse_major_minor(version: &str) -> (u32, u32) {
    let mut parts = version.split('.').map(parse_leading_number);
    let major = parts.next().flatten().unwrap_or(0);
    let minor = parts.next().flatten().unwrap_or(0);
    (major, minor)
}

fn parse_leading_number(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_floor_comparisons() {
        assert!(meets_minimum("4.5.5", MIN_MAJOR, MIN_MINOR));
        assert!(meets_minimum("4.2.0", MIN_MAJOR, MIN_MINOR));
        assert!(meets_minimum("5.0.0", MIN_MAJOR, MIN_MINOR));
        assert!(!meets_minimum("4.1.9", MIN_MAJOR, MIN_MINOR));
        assert!(!meets_minimum("3.7.8", MIN_MAJOR, MIN_MINOR));
    }

    #[test]
    fn tolerates_prerelease_suffixes() {
        assert!(meets_minimum("4.2.0-dev", MIN_MAJOR, MIN_MINOR));
        assert!(meets_minimum("4.3.0-beta1", MIN_MAJOR, MIN_MINOR));
        assert!(!meets_minimum("4.1.0-rc1", MIN_MAJOR, MIN_MINOR));
    }

    #[test]
    fn status_parses_optional_versions() {
        let s: Status = serde_json::from_value(serde_json::json!({
            "netbox-version": "4.5.5",
            "django-version": "5.0.9",
            "python-version": "3.12.3"
        }))
        .unwrap();
        assert_eq!(s.netbox_version, "4.5.5");
        assert_eq!(s.django_version.as_deref(), Some("5.0.9"));
        assert_eq!(s.python_version.as_deref(), Some("3.12.3"));

        // Minimal payload still works.
        let bare: Status =
            serde_json::from_value(serde_json::json!({"netbox-version": "4.2.0"})).unwrap();
        assert!(bare.django_version.is_none());
    }

    #[test]
    fn missing_or_garbage_parts_read_as_zero() {
        assert_eq!(parse_major_minor("4"), (4, 0));
        assert_eq!(parse_major_minor(""), (0, 0));
        assert_eq!(parse_major_minor("x.y.z"), (0, 0));
    }

    // --- credential preflight (`/api/authentication-check/`) ---------------

    use crate::config::ProfileConfig;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A client pointed at a wiremock server (no token — the mock ignores auth).
    fn check_client(server: &MockServer) -> NetBoxClient {
        NetBoxClient::new(
            &ProfileConfig {
                url: server.uri(),
                ..Default::default()
            },
            None,
        )
        .unwrap()
    }

    /// The full flat `UserSerializer` body NetBox 4.5+ returns — nbox must parse
    /// only `username`/`display` and ignore the rest (`id`/`url`/`email`/`groups`).
    fn user_body() -> serde_json::Value {
        json!({
            "id": 1,
            "url": "http://nb/api/users/users/1/",
            "display_url": "http://nb/users/users/1/",
            "display": "admin",
            "username": "admin",
            "first_name": "",
            "last_name": "",
            "email": "admin@example.com",
            "is_active": true,
            "date_joined": "2025-01-01T00:00:00Z",
            "last_login": null,
            "groups": [],
            "permissions": []
        })
    }

    #[tokio::test]
    async fn authentication_check_valid_parses_the_flat_user_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/authentication-check/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(user_body()))
            .mount(&server)
            .await;

        match check_client(&server).authentication_check().await {
            AuthCheck::Valid { username, display } => {
                assert_eq!(username, "admin");
                assert_eq!(display.as_deref(), Some("admin"));
            }
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn authentication_check_valid_when_display_is_absent() {
        // A minimal body with no `display` falls back to the bare username; the
        // verdict is still `valid` and `display` is `None` (omitted from JSON).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/authentication-check/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 7, "username": "svc-account"
            })))
            .mount(&server)
            .await;

        match check_client(&server).authentication_check().await {
            AuthCheck::Valid { username, display } => {
                assert_eq!(username, "svc-account");
                assert!(display.is_none(), "got: {display:?}");
            }
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn authentication_check_invalid_on_401_carries_the_reason() {
        // NetBox returns `{"detail":"Invalid v2 token"}` on a rejected token;
        // the preflight surfaces that cause instead of a generic "rejected".
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/authentication-check/"))
            .respond_with(
                ResponseTemplate::new(401).set_body_string("{\"detail\":\"Invalid v2 token\"}"),
            )
            .mount(&server)
            .await;

        match check_client(&server).authentication_check().await {
            AuthCheck::Invalid { reason } => assert_eq!(reason, "Invalid v2 token"),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn authentication_check_invalid_on_403() {
        // 403 (permission denied) is also a token rejection for an IsAuthenticated
        // endpoint — treat it as `invalid`, not `unverified`.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/authentication-check/"))
            .respond_with(ResponseTemplate::new(403).set_body_string(
                "{\"detail\":\"You do not have permission to perform this action.\"}",
            ))
            .mount(&server)
            .await;

        match check_client(&server).authentication_check().await {
            AuthCheck::Invalid { reason } => {
                assert!(reason.contains("permission"), "reason: {reason}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn authentication_check_unverified_when_endpoint_is_absent() {
        // NetBox < 4.5 has no such endpoint → 404 → `unverified`, never an error.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/authentication-check/"))
            .respond_with(ResponseTemplate::new(404).set_body_string("{\"detail\":\"Not found.\"}"))
            .mount(&server)
            .await;

        match check_client(&server).authentication_check().await {
            AuthCheck::Unverified { reason } => {
                assert!(reason.contains("absent"), "reason: {reason}");
            }
            other => panic!("expected Unverified, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn authentication_check_unverified_on_a_non_auth_failure() {
        // A 5xx (or any non-401/403/404) isn't a token verdict — don't guess
        // `invalid`; report `unverified` with the cause.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/authentication-check/"))
            .respond_with(ResponseTemplate::new(502).set_body_string("upstream error"))
            .mount(&server)
            .await;

        match check_client(&server).authentication_check().await {
            AuthCheck::Unverified { reason } => {
                assert!(
                    reason.contains("502"),
                    "reason should name the status: {reason}"
                );
            }
            other => panic!("expected Unverified, got {other:?}"),
        }
    }

    #[test]
    fn auth_check_plain_renders_each_variant() {
        assert_eq!(
            AuthCheck::Valid {
                username: "admin".into(),
                display: Some("admin (Alice)".into()),
            }
            .plain(),
            "valid (admin (Alice))"
        );
        // A missing/empty display falls back to the username.
        assert_eq!(
            AuthCheck::Valid {
                username: "svc".into(),
                display: None,
            }
            .plain(),
            "valid (svc)"
        );
        assert_eq!(
            AuthCheck::Invalid {
                reason: "Invalid token".into(),
            }
            .plain(),
            "invalid/rejected (Invalid token)"
        );
        assert_eq!(
            AuthCheck::Unverified {
                reason: "endpoint absent (NetBox < 4.5)".into(),
            }
            .plain(),
            "unverified (endpoint absent (NetBox < 4.5))"
        );
    }
}
