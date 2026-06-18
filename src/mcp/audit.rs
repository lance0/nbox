//! Operational layer for the HTTP transport: a structured audit log and a
//! per-caller rate limiter (DESIGN §24, v1 ops baseline).
//!
//! Both key on the *caller* — `sub` if the request carried a validated
//! [`Identity`](super::oidc::Identity), else `client_id`, else the peer IP (which
//! covers loopback / static-bearer mode, where there is no token identity). The
//! audit event records WHO / WHAT / WHEN / OUTCOME for every authenticated
//! request to `/mcp`; the limiter caps requests per caller per minute.
//!
//! This is **read-only Pattern 3** framing: the audit log attributes a call to
//! the verified caller, but the last hop to NetBox still uses the one local
//! service token, so NetBox sees a single identity. That is accountability, not
//! per-user RBAC — see `docs/MCP.md`. Per-user identity→NetBox-token bridging
//! (Pattern 2) is v2.
//!
//! Nothing here ever logs the token, the `Authorization` header, or any secret —
//! the [`Identity`](super::oidc::Identity) it reads never carries the raw token,
//! and the audit fields are an explicit allow-list (`sub`, `client_id`, `scope`,
//! `jti`, `iss`, method, path, status, outcome, latency).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::oidc::Identity;

/// The `tracing` target every audit event is emitted under. Filterable via
/// `--log-level` / `NBOX_LOG` (e.g. `nbox::audit=info` to isolate the audit log,
/// or `nbox::audit=off` to silence it). Documented in `docs/MCP.md`.
pub const AUDIT_TARGET: &str = "nbox::audit";

/// How the request authenticated, recorded in the audit event so a loopback /
/// static-bearer call (which has no token [`Identity`]) is still attributable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    /// Loopback mode, no static bearer configured — loopback is the boundary.
    Loopback,
    /// Loopback mode with the optional static bearer presented + accepted.
    StaticBearer,
    /// OIDC resource-server mode — a validated IdP JWT.
    Oidc,
}

impl AuthMode {
    /// The stable string recorded as the `auth` field.
    fn as_str(self) -> &'static str {
        match self {
            AuthMode::Loopback => "loopback",
            AuthMode::StaticBearer => "static-bearer",
            AuthMode::Oidc => "oidc",
        }
    }
}

/// The coarse outcome of a request, recorded as the `outcome` field. Distinct
/// from the raw status so the log is greppable by category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 2xx/3xx — the request was served.
    Ok,
    /// 401/403 — authentication or scope check failed.
    AuthFailed,
    /// 429 — the per-caller rate limit was hit.
    RateLimited,
    /// Anything else (4xx/5xx) — a request- or server-side error.
    Error,
}

impl Outcome {
    /// Classify an HTTP status code into a coarse outcome.
    pub fn from_status(status: u16) -> Self {
        match status {
            200..=399 => Outcome::Ok,
            401 | 403 => Outcome::AuthFailed,
            429 => Outcome::RateLimited,
            _ => Outcome::Error,
        }
    }

    /// The stable string recorded as the `outcome` field.
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::AuthFailed => "auth-failed",
            Outcome::RateLimited => "rate-limited",
            Outcome::Error => "error",
        }
    }
}

/// The stable key a request is attributed to: `sub`, else `client_id`, else the
/// peer IP. Used both as the rate-limit bucket key and as a coarse audit field.
///
/// The IP fallback is prefixed (`ip:`) so an IP-keyed caller can never collide
/// with a `sub`/`client_id` that happens to look like an address.
pub fn caller_key(identity: Option<&Identity>, peer: Option<IpAddr>) -> String {
    if let Some(id) = identity {
        if let Some(sub) = id.sub.as_deref().filter(|s| !s.is_empty()) {
            return format!("sub:{sub}");
        }
        if let Some(client) = id.client_id.as_deref().filter(|s| !s.is_empty()) {
            return format!("client:{client}");
        }
    }
    match peer {
        Some(ip) => format!("ip:{ip}"),
        None => "ip:unknown".to_string(),
    }
}

/// Hash a raw `Mcp-Session-Id` into a short, stable hex digest for the audit log.
///
/// The raw session id is a bearer-ish correlation token — logging it verbatim
/// would put a reusable handle in the log. A SHA-256 prefix (16 hex chars / the
/// first 8 bytes) stays *correlatable* — the same session id always hashes to the
/// same value, so a session's requests group together — without exposing the
/// token itself. Truncation only weakens preimage resistance, which doesn't
/// matter here: we want a stable opaque label, not a commitment.
pub fn session_hash(session_id: &str) -> String {
    use std::fmt::Write as _;

    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(session_id.as_bytes());
    // First 8 bytes → 16 hex chars: enough to disambiguate sessions in a log
    // without being a full 64-char wall of hex.
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// One audit event, emitted once per request to `/mcp` after the response is
/// known. Built by the middleware; [`Self::emit`] writes the single structured
/// `tracing` event. Holds only safe fields — never a token or secret.
pub struct AuditEvent<'a> {
    /// Correlation id for this request (also returned in the response, see the
    /// middleware). Hex, generated per request.
    pub request_id: &'a str,
    /// How the request authenticated.
    pub auth: AuthMode,
    /// The attributed caller (`sub:`/`client:`/`ip:` key).
    pub caller: &'a str,
    /// `sub` from the validated identity, if any.
    pub sub: Option<&'a str>,
    /// `client_id` (or `azp`) from the validated identity, if any.
    pub client_id: Option<&'a str>,
    /// The granted scopes, space-joined, if any.
    pub scope: Option<&'a str>,
    /// `jti` — the token id, for reference (never the token).
    pub jti: Option<&'a str>,
    /// `iss` — the token issuer, if any.
    pub iss: Option<&'a str>,
    /// HTTP method.
    pub method: &'a str,
    /// Request path (no query string — a token could ride a query string, and
    /// the spec rejects query tokens anyway, so the path is logged bare).
    pub path: &'a str,
    /// A short SHA-256 prefix of the `Mcp-Session-Id` if the client sent one
    /// (see [`session_hash`]). Correlatable across a session's requests, but the
    /// raw session token never lands in the log.
    pub session: Option<&'a str>,
    /// Response status code.
    pub status: u16,
    /// Coarse outcome.
    pub outcome: Outcome,
    /// Wall-clock latency in milliseconds.
    pub latency_ms: u128,
}

impl AuditEvent<'_> {
    /// Emit the single structured event under [`AUDIT_TARGET`]. One event per
    /// request; the fields are JSON-friendly so a JSON `tracing` layer renders a
    /// clean record. Logged at `info` so a default `warn` filter excludes it
    /// until the operator opts in (`nbox::audit=info`).
    pub fn emit(&self) {
        tracing::info!(
            target: AUDIT_TARGET,
            request_id = self.request_id,
            auth = self.auth.as_str(),
            caller = self.caller,
            sub = self.sub,
            client_id = self.client_id,
            scope = self.scope,
            jti = self.jti,
            iss = self.iss,
            method = self.method,
            path = self.path,
            session = self.session,
            status = self.status,
            outcome = self.outcome.as_str(),
            latency_ms = self.latency_ms,
            "mcp request"
        );
    }
}

/// A per-caller fixed-window rate limiter.
///
/// Keyed on the [`caller_key`]; each caller gets its own window, so one caller
/// hitting the limit never affects another. Fixed window (one minute): the first
/// request in a window starts it, the (`limit`+1)th within the same window is
/// rejected, and the window resets `window`-seconds after it opened.
///
/// Construction is gated on a non-zero limit ([`RateLimiter::new`] returns `None`
/// for `0`), so "disabled" is simply the absence of a limiter — the hot path
/// pays nothing.
pub struct RateLimiter {
    /// Requests allowed per window, per caller.
    limit: u32,
    /// The window length (one minute).
    window: Duration,
    /// Per-caller window state.
    state: Mutex<HashMap<String, Window>>,
}

/// One caller's current window: when it opened and how many requests it has seen.
struct Window {
    /// When this window started.
    started: Instant,
    /// Requests counted in this window so far.
    count: u32,
}

/// The limiter's verdict for one request.
#[derive(Debug, PartialEq, Eq)]
pub enum RateDecision {
    /// Under the limit — proceed.
    Allow,
    /// Over the limit — reject with `429` and this `Retry-After` (seconds).
    Limited { retry_after_secs: u64 },
}

impl RateLimiter {
    /// Build a limiter allowing `per_minute` requests per caller per minute, or
    /// `None` when `per_minute` is `0` (disabled — the default).
    pub fn new(per_minute: u32) -> Option<Self> {
        if per_minute == 0 {
            return None;
        }
        Some(Self {
            limit: per_minute,
            window: Duration::from_mins(1),
            state: Mutex::new(HashMap::new()),
        })
    }

    /// Record one request from `caller` and decide whether it is allowed.
    ///
    /// Fixed window: a caller's first request opens a window; subsequent requests
    /// in the same window increment the count; once the window elapses it resets.
    /// A rejected request returns the seconds until the window resets so the
    /// caller can set `Retry-After`.
    pub fn check(&self, caller: &str) -> RateDecision {
        self.check_at(caller, Instant::now())
    }

    /// [`Self::check`] with an injectable clock, for deterministic tests.
    fn check_at(&self, caller: &str, now: Instant) -> RateDecision {
        // Mutex poisoning can only happen if a holder panicked; the critical
        // section is panic-free, so recover the guard rather than propagate.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = state.entry(caller.to_string()).or_insert(Window {
            started: now,
            count: 0,
        });

        // Window elapsed → start a fresh one.
        if now.duration_since(entry.started) >= self.window {
            entry.started = now;
            entry.count = 0;
        }

        if entry.count >= self.limit {
            let elapsed = now.duration_since(entry.started);
            let remaining = self.window.saturating_sub(elapsed);
            // Round up so Retry-After never tells the caller to retry early.
            let secs = remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0);
            return RateDecision::Limited {
                retry_after_secs: secs.max(1),
            };
        }
        entry.count += 1;
        RateDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(sub: Option<&str>, client: Option<&str>) -> Identity {
        Identity {
            sub: sub.map(str::to_string),
            client_id: client.map(str::to_string),
            scopes: vec![],
            jti: None,
            iss: None,
        }
    }

    #[test]
    fn caller_key_prefers_sub_then_client_then_ip() {
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        // sub wins.
        assert_eq!(
            caller_key(Some(&identity(Some("user-1"), Some("c"))), Some(ip)),
            "sub:user-1"
        );
        // client_id when no sub.
        assert_eq!(
            caller_key(Some(&identity(None, Some("agent"))), Some(ip)),
            "client:agent"
        );
        // peer IP when no identity at all (loopback / static-bearer).
        assert_eq!(caller_key(None, Some(ip)), "ip:127.0.0.1");
        // Empty sub/client is treated as absent, falling through to the IP.
        assert_eq!(
            caller_key(Some(&identity(Some(""), Some(""))), Some(ip)),
            "ip:127.0.0.1"
        );
        // No identity and no peer → a stable sentinel.
        assert_eq!(caller_key(None, None), "ip:unknown");
    }

    #[test]
    fn session_hash_is_stable_short_hex_and_not_the_raw_id() {
        let raw = "mcp-session-0123456789abcdef-secret";
        let h = session_hash(raw);
        // 16 lowercase hex chars (first 8 bytes of SHA-256).
        assert_eq!(h.len(), 16, "got {h}");
        assert!(
            h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "non-hex digest: {h}"
        );
        // Stable: the same session id always hashes the same (so a session's
        // requests correlate in the log).
        assert_eq!(session_hash(raw), h);
        // A different session id hashes differently.
        assert_ne!(session_hash("a-different-session"), h);
        // Crucially, the digest is NOT the raw id (no token leak).
        assert_ne!(h, raw);
        assert!(!raw.contains(&h));
    }

    #[test]
    fn caller_key_ip_fallback_cannot_collide_with_a_sub_or_client() {
        // The `ip:` prefix isolates an IP-keyed caller from a `sub`/`client_id`
        // that happens to look like an address. A peer at 10.0.0.1 and a token
        // whose `sub` is literally "10.0.0.1" are different buckets.
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let by_ip = caller_key(None, Some(ip));
        let by_sub = caller_key(Some(&identity(Some("10.0.0.1"), None)), None);
        assert_eq!(by_ip, "ip:10.0.0.1");
        assert_eq!(by_sub, "sub:10.0.0.1");
        assert_ne!(by_ip, by_sub);
    }

    #[test]
    fn caller_key_handles_ipv6_peers() {
        let ip: IpAddr = "::1".parse().unwrap();
        assert_eq!(caller_key(None, Some(ip)), "ip:::1");
    }

    #[test]
    fn auth_mode_as_str_is_stable() {
        // The audit `auth` field strings are a stable contract.
        assert_eq!(AuthMode::Loopback.as_str(), "loopback");
        assert_eq!(AuthMode::StaticBearer.as_str(), "static-bearer");
        assert_eq!(AuthMode::Oidc.as_str(), "oidc");
    }

    #[test]
    fn outcome_as_str_is_stable() {
        // The audit `outcome` field strings are a stable, greppable contract.
        assert_eq!(Outcome::Ok.as_str(), "ok");
        assert_eq!(Outcome::AuthFailed.as_str(), "auth-failed");
        assert_eq!(Outcome::RateLimited.as_str(), "rate-limited");
        assert_eq!(Outcome::Error.as_str(), "error");
    }

    #[test]
    fn outcome_classifies_status_codes() {
        assert_eq!(Outcome::from_status(200), Outcome::Ok);
        assert_eq!(Outcome::from_status(204), Outcome::Ok);
        assert_eq!(Outcome::from_status(401), Outcome::AuthFailed);
        assert_eq!(Outcome::from_status(403), Outcome::AuthFailed);
        assert_eq!(Outcome::from_status(429), Outcome::RateLimited);
        assert_eq!(Outcome::from_status(400), Outcome::Error);
        assert_eq!(Outcome::from_status(500), Outcome::Error);
    }

    #[test]
    fn zero_limit_disables_the_limiter() {
        assert!(RateLimiter::new(0).is_none());
        assert!(RateLimiter::new(1).is_some());
    }

    #[test]
    fn limiter_allows_up_to_limit_then_429s() {
        let rl = RateLimiter::new(3).unwrap();
        let t0 = Instant::now();
        // First 3 allowed.
        for _ in 0..3 {
            assert_eq!(rl.check_at("sub:a", t0), RateDecision::Allow);
        }
        // 4th rejected, with a positive Retry-After.
        match rl.check_at("sub:a", t0) {
            RateDecision::Limited { retry_after_secs } => assert!(retry_after_secs >= 1),
            RateDecision::Allow => panic!("expected Limited, got Allow"),
        }
    }

    #[test]
    fn limiter_isolates_callers() {
        let rl = RateLimiter::new(1).unwrap();
        let t0 = Instant::now();
        // Caller a uses its one allowance.
        assert_eq!(rl.check_at("sub:a", t0), RateDecision::Allow);
        assert!(matches!(
            rl.check_at("sub:a", t0),
            RateDecision::Limited { .. }
        ));
        // Caller b is unaffected — its own fresh window.
        assert_eq!(rl.check_at("sub:b", t0), RateDecision::Allow);
    }

    #[test]
    fn limiter_resets_after_the_window() {
        let rl = RateLimiter::new(1).unwrap();
        let t0 = Instant::now();
        assert_eq!(rl.check_at("sub:a", t0), RateDecision::Allow);
        assert!(matches!(
            rl.check_at("sub:a", t0),
            RateDecision::Limited { .. }
        ));
        // A minute later the window has reset → allowed again.
        let later = t0 + Duration::from_secs(61);
        assert_eq!(rl.check_at("sub:a", later), RateDecision::Allow);
    }

    #[test]
    fn retry_after_is_rounded_up_and_within_the_window() {
        let rl = RateLimiter::new(1).unwrap();
        let t0 = Instant::now();
        assert_eq!(rl.check_at("sub:a", t0), RateDecision::Allow);
        // 0.5 s into the window: ~59.5 s remain → rounds up to 60.
        let half = t0 + Duration::from_millis(500);
        match rl.check_at("sub:a", half) {
            RateDecision::Limited { retry_after_secs } => {
                assert!(
                    (1..=60).contains(&retry_after_secs),
                    "got {retry_after_secs}"
                );
            }
            RateDecision::Allow => panic!("expected Limited, got Allow"),
        }
    }

    #[test]
    fn window_boundary_exactly_n_allowed_then_n_plus_one_limited() {
        // The fixed-window contract at its exact edge: with limit N and all
        // requests inside one window, requests 1..=N are allowed and the (N+1)th
        // is rejected. At the window's start the full minute remains, so the
        // Retry-After is the whole window (60).
        let n = 5;
        let rl = RateLimiter::new(n).unwrap();
        let t0 = Instant::now();
        for i in 1..=n {
            assert_eq!(
                rl.check_at("sub:edge", t0),
                RateDecision::Allow,
                "request {i} of {n} should be allowed"
            );
        }
        assert_eq!(
            rl.check_at("sub:edge", t0),
            RateDecision::Limited {
                retry_after_secs: 60
            },
            "the (N+1)th request in the window is rejected with a full-window Retry-After"
        );
    }

    #[test]
    fn limited_requests_do_not_extend_the_window() {
        // A request rejected mid-window must not reset/extend the window: once the
        // original minute elapses (measured from the FIRST request, not the last
        // rejected one), the caller is allowed again.
        let rl = RateLimiter::new(1).unwrap();
        let t0 = Instant::now();
        assert_eq!(rl.check_at("sub:a", t0), RateDecision::Allow);
        // Several rejections partway through the window.
        for dt in [10, 20, 30] {
            assert!(matches!(
                rl.check_at("sub:a", t0 + Duration::from_secs(dt)),
                RateDecision::Limited { .. }
            ));
        }
        // 61 s after the FIRST request the window has reset despite the rejections.
        assert_eq!(
            rl.check_at("sub:a", t0 + Duration::from_secs(61)),
            RateDecision::Allow
        );
    }

    #[test]
    fn audit_event_emits_only_the_allow_listed_fields_and_no_secret() {
        use std::sync::{Arc, Mutex as StdMutex};

        use tracing::field::{Field, Visit};
        use tracing::subscriber::with_default;
        use tracing_subscriber::layer::{Context, SubscriberExt};
        use tracing_subscriber::{Layer, Registry};

        // A tracing layer that records the field NAMES (and stringified values)
        // of every event under the audit target. We use it to prove the emitted
        // record carries exactly the documented allow-list — and, crucially, no
        // `token`/`authorization` field — regardless of what an operator filters.
        #[derive(Default)]
        struct Capture {
            names: Arc<StdMutex<Vec<String>>>,
            values: Arc<StdMutex<Vec<String>>>,
        }
        struct NameVisitor<'a> {
            names: &'a mut Vec<String>,
            values: &'a mut Vec<String>,
        }
        impl Visit for NameVisitor<'_> {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                self.names.push(field.name().to_string());
                self.values.push(format!("{value:?}"));
            }
            fn record_str(&mut self, field: &Field, value: &str) {
                self.names.push(field.name().to_string());
                self.values.push(value.to_string());
            }
        }
        impl<S: tracing::Subscriber> Layer<S> for Capture {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                if event.metadata().target() != AUDIT_TARGET {
                    return;
                }
                let mut names = self.names.lock().unwrap();
                let mut values = self.values.lock().unwrap();
                let mut v = NameVisitor {
                    names: &mut names,
                    values: &mut values,
                };
                event.record(&mut v);
            }
        }

        let names = Arc::new(StdMutex::new(Vec::new()));
        let values = Arc::new(StdMutex::new(Vec::new()));
        let layer = Capture {
            names: names.clone(),
            values: values.clone(),
        };
        let subscriber = Registry::default().with(layer);

        with_default(subscriber, || {
            AuditEvent {
                request_id: "req-abc",
                auth: AuthMode::Oidc,
                caller: "sub:user-1",
                sub: Some("user-1"),
                client_id: Some("agent-x"),
                scope: Some("netbox:read"),
                jti: Some("jti-123"),
                iss: Some("https://idp.example.com"),
                method: "POST",
                path: "/mcp",
                session: Some("0123456789abcdef"),
                status: 200,
                outcome: Outcome::Ok,
                latency_ms: 12,
            }
            .emit();
        });

        let names = names.lock().unwrap();
        let values = values.lock().unwrap();

        // The exact documented allow-list (plus the `message`).
        let allowed = [
            "message",
            "request_id",
            "auth",
            "caller",
            "sub",
            "client_id",
            "scope",
            "jti",
            "iss",
            "method",
            "path",
            "session",
            "status",
            "outcome",
            "latency_ms",
        ];
        for name in names.iter() {
            assert!(
                allowed.contains(&name.as_str()),
                "audit event emitted an unexpected field: {name}"
            );
        }
        // The forbidden fields must never be present, under any name.
        for forbidden in [
            "token",
            "authorization",
            "Authorization",
            "bearer",
            "secret",
        ] {
            assert!(
                !names.iter().any(|n| n == forbidden),
                "audit event must never emit a `{forbidden}` field"
            );
        }
        // And no recorded value carries an Authorization/bearer-shaped secret.
        for v in values.iter() {
            let lower = v.to_lowercase();
            assert!(
                !lower.contains("bearer ") && !lower.contains("authorization"),
                "audit value leaked a secret-shaped string: {v}"
            );
        }
        // Sanity: the safe identity fields did make it through.
        assert!(names.iter().any(|n| n == "sub"));
        assert!(values.iter().any(|v| v == "user-1"));
    }
}
