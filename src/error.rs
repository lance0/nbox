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
    /// HTTP 401 — missing, wrong, or expired token. The `String` is the server's
    /// reason as a ready-to-print suffix (`": Invalid token"`, or empty).
    #[error("authentication failed (HTTP 401){0} — check the token for this profile")]
    Authentication(String),

    /// HTTP 403 — the token is rejected or lacks permission. The `String` is the
    /// server's reason as a ready-to-print suffix (`": Invalid v2 token"`, or empty).
    #[error("permission denied (HTTP 403){0} — check the token or permissions for this profile")]
    PermissionDenied(String),

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

    /// The user asked for an unsupported flag/data combination (e.g. `-o csv`
    /// on a single object, or `--confirm` without `--allow-writes` on a write).
    /// Carries a friendly, actionable message.
    #[error("{0}")]
    Usage(String),

    /// A write was refused because the object changed in NetBox between plan and
    /// apply (an `If-Match` 412 on 4.6+, or a `last_updated`/before-hash mismatch
    /// on the pre-4.6 fallback). The caller should re-run dry-run and re-confirm.
    /// Not a usage error and not a server failure — a deliberate, recoverable
    /// refusal — so it sits in the generic exit-1 bucket with a precise message
    /// (ADR-0001 §8: "not applied: object changed in NetBox; re-run dry-run").
    #[error("not applied: object changed in NetBox; re-run dry-run{0}")]
    StalePrecondition(String),

    /// NetBox rejected a write's patch body (HTTP 400). Carries the field-level
    /// detail NetBox returns, surfaced with field context and no stdout pollution
    /// (ADR-0001 §8: "not applied: NetBox rejected the patch"). Exit 1, like other
    /// API failures — distinct from a stale precondition above so the message can
    /// tell the operator exactly what to fix rather than "re-run dry-run".
    #[error("not applied: NetBox rejected the patch: {0}")]
    WriteValidation(String),

    /// Any other non-success API response.
    #[error("NetBox API request failed: HTTP {status}: {body}")]
    Api { status: u16, body: String },
}

impl NboxError {
    /// The process exit code for this error. Stable contract:
    /// `2` usage, `3` auth/permission, `4` not found, `5` ambiguous, `1` other.
    pub fn exit_code(&self) -> i32 {
        match self {
            NboxError::Usage(_) => 2,
            NboxError::Authentication(_) | NboxError::PermissionDenied(_) => 3,
            NboxError::NotFound(_) => 4,
            NboxError::Ambiguous { .. } => 5,
            NboxError::StalePrecondition(_)
            | NboxError::WriteValidation(_)
            | NboxError::Api { .. } => 1,
        }
    }

    /// The exit code for any error: the most specific [`NboxError`] in the chain,
    /// else `1`.
    pub fn exit_code_for(err: &anyhow::Error) -> i32 {
        err.chain()
            .find_map(|e| e.downcast_ref::<NboxError>())
            .map_or(1, NboxError::exit_code)
    }
}

/// True if `err`'s chain contains an [`io::ErrorKind::BrokenPipe`].
///
/// On Unix the SIGPIPE reset in `main` makes a closed stdout pipe terminate the
/// process before this is reached, so this is the portable belt-and-suspenders
/// path: any platform where a `BrokenPipe` surfaces as an `anyhow::Error`
/// instead (e.g. an explicit `writeln!` whose error was propagated) gets a quiet
/// exit rather than a printed error. It matches *only* `BrokenPipe`, so
/// unrelated IO errors keep their normal noisy path.
pub fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|e| {
        e.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

/// True if `err`'s chain contains a [`NboxError::NotFound`] — i.e. the request
/// hit an HTTP 404. Used by version-gated search fan-out branches (kinds added
/// in a later NetBox release, like rack-groups/vm-types in 4.6) to treat a
/// missing endpoint as an empty result on older releases rather than fail the
/// whole search. A 404 on a *list* endpoint means "this kind doesn't exist on
/// this version" — never a per-object miss — so swallowing it is safe there.
pub fn is_not_found(err: &anyhow::Error) -> bool {
    err.chain()
        .find_map(|e| e.downcast_ref::<NboxError>())
        .is_some_and(|ne| matches!(ne, NboxError::NotFound(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(NboxError::Usage("x".into()).exit_code(), 2);
        assert_eq!(NboxError::Authentication(String::new()).exit_code(), 3);
        assert_eq!(NboxError::PermissionDenied(String::new()).exit_code(), 3);
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
        let cases: [(NboxError, i32); 8] = [
            (NboxError::Usage("bad combo".into()), 2),
            (NboxError::Authentication(String::new()), 3),
            (NboxError::PermissionDenied(String::new()), 3),
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
            (NboxError::StalePrecondition(String::new()), 1),
            (
                NboxError::WriteValidation("description: required".into()),
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
        let err = anyhow::Error::from(NboxError::Authentication(String::new()))
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
        let err = anyhow::Error::from(NboxError::PermissionDenied(String::new()))
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

    #[test]
    fn broken_pipe_is_detected_directly_and_through_context() {
        let bare = anyhow::Error::from(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "broken pipe",
        ));
        assert!(is_broken_pipe(&bare));

        // The realistic shape: a write error wrapped with handler context.
        let wrapped = anyhow::Error::from(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "broken pipe",
        ))
        .context("writing output")
        .context("running `nbox completions bash`");
        assert!(is_broken_pipe(&wrapped));
    }

    #[test]
    fn non_broken_pipe_io_errors_are_not_classified_as_broken_pipe() {
        // Must not swallow unrelated IO errors: only BrokenPipe gets the quiet
        // exit; everything else keeps its normal, noisy path.
        let other = anyhow::Error::from(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ))
        .context("writing output");
        assert!(!is_broken_pipe(&other));

        let untyped = anyhow::anyhow!("some non-IO failure");
        assert!(!is_broken_pipe(&untyped));
    }
}
