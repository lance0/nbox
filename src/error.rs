//! Typed errors with stable process exit codes.
//!
//! Most of nbox uses `anyhow` for ergonomic propagation. A handful of conditions
//! are worth a *stable* exit code so scripts and agents can branch on them; those
//! become an [`NboxError`] somewhere in the error chain, and `main` maps the chain
//! to a code via [`NboxError::exit_code_for`].

use thiserror::Error;

/// An error condition with a stable, documented exit code.
#[derive(Debug, Error)]
pub enum NboxError {
    /// HTTP 401 — missing, wrong, or expired token.
    #[error("authentication failed (HTTP 401) — check the token for this profile")]
    Authentication,

    /// HTTP 403 — the token is valid but lacks permission.
    #[error("permission denied (HTTP 403) — the token cannot access this resource")]
    PermissionDenied,

    /// A lookup matched nothing. Carries a friendly, actionable message.
    #[error("{0}")]
    NotFound(String),

    /// A reference matched more than one object.
    #[error(
        "{noun} \"{value}\" is ambiguous — matches: {matches}\n\nBe more specific, or use an ID."
    )]
    Ambiguous {
        noun: String,
        value: String,
        matches: String,
    },

    /// Any other non-success API response.
    #[error("NetBox API request failed: HTTP {status}: {body}")]
    Api { status: u16, body: String },
}

impl NboxError {
    /// The process exit code for this error. Stable contract:
    /// `3` auth/permission, `4` not found, `5` ambiguous, `1` other.
    pub fn exit_code(&self) -> i32 {
        match self {
            NboxError::Authentication | NboxError::PermissionDenied => 3,
            NboxError::NotFound(_) => 4,
            NboxError::Ambiguous { .. } => 5,
            NboxError::Api { .. } => 1,
        }
    }

    /// The exit code for any error: the most specific [`NboxError`] in the chain,
    /// else `1`.
    pub fn exit_code_for(err: &anyhow::Error) -> i32 {
        err.chain()
            .find_map(|e| e.downcast_ref::<NboxError>())
            .map(NboxError::exit_code)
            .unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(NboxError::Authentication.exit_code(), 3);
        assert_eq!(NboxError::PermissionDenied.exit_code(), 3);
        assert_eq!(NboxError::NotFound("x".into()).exit_code(), 4);
        assert_eq!(
            NboxError::Ambiguous {
                noun: "device".into(),
                value: "edge".into(),
                matches: "edge01, edge02".into()
            }
            .exit_code(),
            5
        );
        assert_eq!(
            NboxError::Api {
                status: 500,
                body: "boom".into()
            }
            .exit_code(),
            1
        );
    }

    #[test]
    fn exit_code_is_found_through_a_wrapped_chain() {
        let err = anyhow::Error::from(NboxError::NotFound("no device".into()))
            .context("while looking up device");
        assert_eq!(NboxError::exit_code_for(&err), 4);
    }

    #[test]
    fn every_variant_keeps_its_code_when_wrapped_in_context() {
        // The chain-walking path (`exit_code_for`) must recover the stable code
        // for each variant even when buried under a `.context(...)` layer — this
        // is the real path `main` takes, since handlers wrap errors with context.
        let cases: [(NboxError, i32); 5] = [
            (NboxError::Authentication, 3),
            (NboxError::PermissionDenied, 3),
            (NboxError::NotFound("nope".into()), 4),
            (
                NboxError::Ambiguous {
                    noun: "device".into(),
                    value: "edge".into(),
                    matches: "edge01, edge02".into(),
                },
                5,
            ),
            (
                NboxError::Api {
                    status: 502,
                    body: "bad gateway".into(),
                },
                1,
            ),
        ];
        for (variant, code) in cases {
            let err = anyhow::Error::from(variant).context("while looking up the object");
            assert_eq!(
                NboxError::exit_code_for(&err),
                code,
                "wrapped variant lost its code"
            );
        }
    }

    #[test]
    fn exit_code_survives_multiple_context_layers() {
        // Several nested context layers — the deepest typed error still wins.
        let err = anyhow::Error::from(NboxError::Authentication)
            .context("authenticating to NetBox")
            .context("running `nbox device edge01`")
            .context("dispatching command");
        assert_eq!(NboxError::exit_code_for(&err), 3);
    }

    #[test]
    fn typed_error_under_untyped_outer_context_is_still_found() {
        // A typed error buried beneath an untyped (string) context layer — the
        // realistic shape when a handler adds a `.with_context(|| "...")` note —
        // is still recovered by walking the chain.
        let err = anyhow::Error::from(NboxError::PermissionDenied)
            .context("fetching /api/dcim/sites/")
            .context("running `nbox site iad1`");
        assert_eq!(NboxError::exit_code_for(&err), 3);
    }

    #[test]
    fn untyped_error_defaults_to_one() {
        let err = anyhow::anyhow!("some generic failure");
        assert_eq!(NboxError::exit_code_for(&err), 1);
    }

    #[test]
    fn untyped_error_with_context_still_defaults_to_one() {
        let err = anyhow::anyhow!("disk full")
            .context("writing cache")
            .context("running command");
        assert_eq!(NboxError::exit_code_for(&err), 1);
    }
}
