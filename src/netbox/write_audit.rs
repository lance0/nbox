//! Local write audit (ADR-0001 §8).
//!
//! Every write emits exactly one structured `tracing` event under
//! [`AUDIT_TARGET`] after the outcome is known. Like the MCP audit path, this
//! goes to `tracing` (stderr/file), NEVER stdout, and the fields are an explicit
//! allow-list. The redaction discipline is the contract:
//!
//! - logs the changed field **names**, never the before/after values;
//! - never the token, the `Authorization` header, or the raw `PATCH` body;
//! - never the full object;
//! - never the free-form `changelog_message` body — only a `message_present`
//!   flag and its character length.
//!
//! Field set (allow-list): `surface` (`cli`/`tui`/`mcp`), `profile`, NetBox
//! `host`, `operation`, target `kind`/`id`/`display`, `fields` (names),
//! `outcome`, `http_method`, `http_path` (no query string), `status`,
//! `latency_ms`, `request_id` (NetBox object-change id, when a receipt lookup
//! finds one — deferred in v1), `message_present`, `message_len`.
//!
//! Off by default under the `warn` filter; opt in with
//! `NBOX_LOG=…,nbox::write_audit=info` (or `--log-level`).

use std::time::Instant;

use crate::netbox::mutation::Operation;

/// The `tracing` target every write audit event is emitted under. Filterable
/// via `--log-level` / `NBOX_LOG` (e.g. `nbox::write_audit=info` to isolate,
/// `nbox::write_audit=off` to silence).
pub const AUDIT_TARGET: &str = "nbox::write_audit";

/// Which nbox surface issued the write (recorded so a CLI/TUI/MCP write is
/// distinguishable in the log). `cli` and `mcp` ship; TUI writes are deferred.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Cli,
    /// A write driven through `nbox serve` (an MCP `nbox_apply_write` call),
    /// attributed to the calling user's OIDC `sub` via the per-user vault.
    Mcp,
}

impl Surface {
    fn as_str(self) -> &'static str {
        match self {
            Surface::Cli => "cli",
            Surface::Mcp => "mcp",
        }
    }
}

/// The coarse outcome of a write, recorded so the log is greppable by category.
/// Mirrors the user-visible wording in ADR-0001 §8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `--dry-run`: planned, no changes sent.
    DryRun,
    /// No-op: the current value already matches; no `PATCH` sent.
    NoOp,
    /// The `PATCH` was sent and NetBox accepted it.
    Applied,
    /// A TTY prompt was declined (or non-interactive apply lacked `--confirm`).
    /// Recorded when the operator explicitly said no; a usage refusal before
    /// any plan is a different path (no audit event).
    NotApplied,
    /// The object changed in NetBox between plan and apply (412 or before-hash
    /// mismatch). Recoverable: re-run dry-run.
    Stale,
    /// NetBox rejected the `PATCH` body (HTTP 400).
    Validation,
    /// Any other failure (network error, auth/permission, server 5xx).
    Error,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::DryRun => "dry_run",
            Outcome::NoOp => "no_op",
            Outcome::Applied => "applied",
            Outcome::NotApplied => "not_applied",
            Outcome::Stale => "stale",
            Outcome::Validation => "validation",
            Outcome::Error => "error",
        }
    }
}

/// One write audit event. Holds only safe fields — never a token, a patch
/// value, a full object, or the changelog message body. Built by the write
/// command; [`Self::emit`] writes the single structured `tracing` event.
pub struct WriteAuditEvent<'a> {
    pub surface: Surface,
    pub profile: &'a str,
    /// NetBox host (scheme + host [+ port]) — the base URL minus any path/query.
    pub host: &'a str,
    pub operation: Operation,
    pub target_kind: &'a str,
    pub target_id: u64,
    pub target_display: &'a str,
    /// Changed field NAMES only (ADR-0001 §8) — never the values.
    pub fields: &'a [&'a str],
    pub outcome: Outcome,
    /// HTTP method on the apply (`PATCH`, or `GET` for a dry-run read). Empty
    /// for an outcome that made no request (no-op, declined prompt).
    pub http_method: &'a str,
    /// Request path (no query string — a token could ride one). Empty when no
    /// request was made.
    pub http_path: &'a str,
    /// HTTP status of the apply request (`0` when no request was made).
    pub status: u16,
    pub latency_ms: u128,
    /// NetBox object-change request id, when a receipt lookup finds one
    /// (deferred in v1 — `None` is valid per the allow-list).
    pub request_id: Option<&'a str>,
    /// True when the caller passed a `--message` (never the message body).
    pub message_present: bool,
    /// Character length of the `--message` (never the body).
    pub message_len: usize,
}

impl WriteAuditEvent<'_> {
    /// Emit the single structured event under [`AUDIT_TARGET`] at `info` (so a
    /// default `warn` filter excludes it until the operator opts in). The fields
    /// are JSON-friendly so a JSON `tracing` layer renders a clean record.
    pub fn emit(&self) {
        tracing::info!(
            target: AUDIT_TARGET,
            surface = self.surface.as_str(),
            profile = self.profile,
            host = self.host,
            operation = self.operation.as_str(),
            target_kind = self.target_kind,
            target_id = self.target_id,
            target_display = self.target_display,
            fields = self.fields.join(","),
            outcome = self.outcome.as_str(),
            http_method = self.http_method,
            http_path = self.http_path,
            status = self.status,
            latency_ms = self.latency_ms,
            request_id = self.request_id,
            message_present = self.message_present,
            message_len = self.message_len,
            "write"
        );
    }
}

/// A small stopwatch for the apply latency field. `started` at the top of apply,
/// [`elapsed_ms`](Started::elapsed_ms) when the response (or refusal) is known.
/// Kept simple and explicit so the audit test can reason about non-zero latency
/// without a fake clock.
pub struct Started(Instant);

impl Started {
    #[must_use]
    pub fn now() -> Self {
        Self(Instant::now())
    }

    #[must_use]
    pub fn elapsed_ms(&self) -> u128 {
        // Monotonic elapsed since start — `Instant` never goes backwards, so no
        // clock-skew guard is needed.
        self.0.elapsed().as_millis()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev<'a>(fields: &'a [&'a str], message: Option<&'a str>) -> WriteAuditEvent<'a> {
        WriteAuditEvent {
            surface: Surface::Cli,
            profile: "default",
            host: "http://netbox.example",
            operation: Operation::Update,
            target_kind: "interface",
            target_id: 42,
            target_display: "xe-0/0/1",
            fields,
            outcome: Outcome::Applied,
            http_method: "PATCH",
            http_path: "/api/dcim/interfaces/42/",
            status: 200,
            latency_ms: 7,
            request_id: None,
            message_present: message.is_some(),
            message_len: message.map_or(0, str::len),
        }
    }

    #[test]
    fn outcome_strings_are_stable() {
        assert_eq!(Outcome::DryRun.as_str(), "dry_run");
        assert_eq!(Outcome::NoOp.as_str(), "no_op");
        assert_eq!(Outcome::Applied.as_str(), "applied");
        assert_eq!(Outcome::NotApplied.as_str(), "not_applied");
        assert_eq!(Outcome::Stale.as_str(), "stale");
        assert_eq!(Outcome::Validation.as_str(), "validation");
        assert_eq!(Outcome::Error.as_str(), "error");
    }

    #[test]
    fn surface_string_is_cli_for_v1() {
        assert_eq!(Surface::Cli.as_str(), "cli");
    }

    #[test]
    fn allocate_event_audits_operation_and_post_method() {
        // An allocate (e.g. `ip reserve`) audits with operation="allocate" and
        // http_method="POST"; still only field NAMES are carried, never values.
        let fields = ["description", "dns_name"];
        let mut e = ev(&fields, None);
        e.operation = Operation::Allocate;
        e.target_kind = "ip";
        e.http_method = "POST";
        e.http_path = "/api/ipam/prefixes/1/available-ips/";
        e.status = 201;
        assert_eq!(e.operation.as_str(), "allocate");
        assert_eq!(e.http_method, "POST");
        assert_eq!(e.fields, &["description", "dns_name"]);
    }

    #[test]
    fn message_is_recorded_as_present_flag_and_length_only() {
        // The event carries a present-flag + length, never the body. This test
        // pins the fields exist and the length is what was passed; the body
        // never enters the struct.
        let e = ev(&["description"], Some("rotating uplink xe-0/0/1"));
        assert!(e.message_present);
        assert_eq!(e.message_len, "rotating uplink xe-0/0/1".len());
        let e = ev(&["description"], None);
        assert!(!e.message_present);
        assert_eq!(e.message_len, 0);
    }

    #[test]
    fn fields_are_names_not_values() {
        // The audit allow-list is field NAMES. The struct holds a slice of str
        // names; values never appear here (they live on the MutationPlan, which
        // is shown to the operator, not logged).
        let e = ev(&["description"], None);
        assert_eq!(e.fields, &["description"]);
    }

    #[test]
    fn started_stopwatch_is_non_negative() {
        let s = Started::now();
        // Just exercises the path; a freshly started stopwatch reads ~0ms.
        assert!(s.elapsed_ms() < 5_000);
    }

    /// The tracing event `emit()` produces carries exactly the documented
    /// field allow-list — and no `token`/`authorization`/secret — regardless
    /// of what an operator filters. Pins the field names an agent/log scraper
    /// can rely on; a removed, renamed, or added field fails here.
    #[test]
    fn write_audit_field_allowlist_is_stable() {
        use std::sync::{Arc, Mutex as StdMutex};

        use tracing::field::{Field, Visit};
        use tracing::subscriber::with_default;
        use tracing_subscriber::layer::{Context, SubscriberExt};
        use tracing_subscriber::{Layer, Registry};

        // A tracing layer that records the field NAMES of every event under
        // the write-audit target. We use it to prove the emitted record carries
        // exactly the documented allow-list — and, crucially, no
        // `token`/`authorization` field.
        #[derive(Default)]
        struct Capture {
            names: Arc<StdMutex<Vec<String>>>,
        }
        struct NameVisitor<'a> {
            names: &'a mut Vec<String>,
        }
        impl Visit for NameVisitor<'_> {
            fn record_debug(&mut self, field: &Field, _value: &dyn std::fmt::Debug) {
                self.names.push(field.name().to_string());
            }
            fn record_str(&mut self, field: &Field, _value: &str) {
                self.names.push(field.name().to_string());
            }
        }
        impl<S: tracing::Subscriber> Layer<S> for Capture {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                if event.metadata().target() != AUDIT_TARGET {
                    return;
                }
                let mut names = self.names.lock().unwrap();
                let mut v = NameVisitor { names: &mut names };
                event.record(&mut v);
            }
        }

        let names = Arc::new(StdMutex::new(Vec::new()));
        let layer = Capture {
            names: names.clone(),
        };
        let subscriber = Registry::default().with(layer);

        with_default(subscriber, || {
            let mut event = ev(&["description", "status"], None);
            event.request_id = Some("req-1");
            event.emit();
        });

        let mut names = names.lock().unwrap().clone();
        names.sort_unstable();
        names.dedup();

        // The exact documented allow-list (plus the `message` event message).
        let mut allowed: Vec<String> = [
            "message",
            "surface",
            "profile",
            "host",
            "operation",
            "target_kind",
            "target_id",
            "target_display",
            "fields",
            "outcome",
            "http_method",
            "http_path",
            "status",
            "latency_ms",
            "request_id",
            "message_present",
            "message_len",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        allowed.sort_unstable();
        assert_eq!(names, allowed, "write-audit event field set drifted");
        // The forbidden fields must never be present, under any name.
        for forbidden in ["token", "authorization", "secret", "bearer"] {
            assert!(
                !names.iter().any(|n| n == forbidden),
                "write-audit event must never emit a `{forbidden}` field"
            );
        }
        // Sanity: a representative allow-list field did make it through.
        assert!(names.iter().any(|n| n == "outcome"));
        assert!(names.iter().any(|n| n == "surface"));
    }
}
