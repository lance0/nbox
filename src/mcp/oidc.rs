//! OIDC resource-server auth for the HTTP transport (rung 3 of the MCP transport
//! ladder; see `DESIGN.md` §24).
//!
//! When `--oidc-issuer` + `--audience` are configured, nbox validates inbound IdP
//! JWTs on `/mcp` and advertises RFC 9728 Protected Resource Metadata. nbox is an
//! OAuth 2.1 **resource server only**: it validates bearer tokens, it does not
//! mint them or run login. The token-validation obligations (RFC 9068 / OAuth 2.1
//! §5.2) are:
//!
//! - bearer from the `Authorization` header (query-string tokens rejected);
//! - signature via the IdP's JWKS selected by `kid`, with an explicit **alg
//!   allowlist** (RS256/ES256 — never trust the token's `alg`, reject `none`);
//! - `iss` exact-match the configured issuer;
//! - `aud` *contains* the configured audience (RFC 8707 — reject otherwise);
//! - `exp` in the future with ≤120 s clock-skew leeway.
//!
//! Provider-agnostic: works for any conformant IdP (Okta, Entra, Keycloak,
//! Authentik, …). No vendor is named or special-cased.
//!
//! **Fail closed**, but validation is *offline* against cached JWKS: a transient
//! JWKS-fetch failure with the key already cached still validates (serve-stale);
//! an unknown-`kid` cache miss during an outage fails closed. JWKS is cached by
//! `kid`; an unknown `kid` triggers a single rate-limited, single-flight refresh
//! (defeats DoS-by-unknown-kid) and keeps all currently-published keys (rotation
//! overlap).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tokio::sync::Mutex;

/// The two scopes nbox understands (DESIGN §24). Reads need `nbox:read`; writes
/// (none yet) will need `nbox:write`.
pub const SCOPE_READ: &str = "nbox:read";
pub const SCOPE_WRITE: &str = "nbox:write";

/// The algorithms nbox will verify with — the explicit allowlist. The token's
/// own `alg` is never trusted: `decode` is told exactly these, so `none` and any
/// alg-confusion variant is rejected before a key is even selected.
const ALLOWED_ALGS: [Algorithm; 2] = [Algorithm::RS256, Algorithm::ES256];

/// Clock-skew leeway for `exp` (and `nbf`), in seconds (DESIGN §24: ≤120 s).
const CLOCK_SKEW_LEEWAY_SECS: u64 = 120;

/// Minimum gap between JWKS refreshes triggered by an unknown `kid`. Rate-limits
/// the refresh so a stream of unknown-`kid` tokens can't hammer the IdP.
const JWKS_REFRESH_MIN_INTERVAL: Duration = Duration::from_secs(30);

/// Resolved OIDC resource-server configuration. Cheap to clone (`Arc` internals);
/// rides as axum router state.
#[derive(Clone)]
pub struct OidcConfig {
    /// The IdP issuer — `iss` must match this exactly.
    pub issuer: Arc<str>,
    /// The expected audience (nbox's canonical resource URI). A token's `aud`
    /// must contain this (RFC 8707).
    pub audience: Arc<str>,
    /// The JWKS endpoint URL, resolved at startup (override or discovered).
    pub jwks_uri: Arc<str>,
    /// The shared, by-`kid` JWKS cache.
    pub jwks: Arc<JwksCache>,
}

/// The validated identity, plumbed into axum request extensions so downstream
/// (Unit 3's audit log + NetBox bridge) can read the caller without re-parsing
/// the token. Never carries the raw token.
#[derive(Clone, Debug)]
pub struct Identity {
    /// `sub` — the stable subject identifier.
    pub sub: Option<String>,
    /// `client_id`, falling back to `azp` — the calling OAuth client.
    pub client_id: Option<String>,
    /// The granted scopes (parsed from `scope` and/or `scp`).
    pub scopes: Vec<String>,
    /// `jti` — the token id, for audit reference (never the token itself).
    pub jti: Option<String>,
    /// `iss` — the issuer the token was minted by.
    pub iss: Option<String>,
}

impl Identity {
    /// Whether the caller was granted `scope`.
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }
}

/// The access-token claims nbox reads. Only the fields the RS needs; unknown
/// claims are ignored. `iss`/`aud`/`exp` are validated by `jsonwebtoken` itself
/// (via [`Validation`]); these are pulled out afterward for the [`Identity`].
#[derive(Debug, Deserialize)]
struct Claims {
    sub: Option<String>,
    iss: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    jti: Option<String>,
    /// Space-delimited scope string (OAuth 2.0 / RFC 8693).
    #[serde(default)]
    scope: Option<String>,
    /// Scope array (`scp`) — some IdPs use this instead of/alongside `scope`.
    #[serde(default)]
    scp: Option<ScopeField>,
}

/// `scp` is a string or an array of strings depending on the IdP. Accept both.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ScopeField {
    One(String),
    Many(Vec<String>),
}

/// Why a token was rejected. Maps to the 401/403 challenge the middleware emits.
/// The `Display` text is the safe `error_description` — it never echoes the token
/// or any claim value.
#[derive(Debug)]
pub enum AuthError {
    /// No bearer in the `Authorization` header (or a non-`Bearer` scheme).
    MissingToken,
    /// The token failed validation (signature, `iss`, `aud`, `exp`, malformed,
    /// unknown `kid`, disallowed `alg`, …). → 401 `invalid_token`.
    InvalidToken(&'static str),
    /// Valid token, but it lacks the scope the tool requires. → 403
    /// `insufficient_scope`.
    InsufficientScope { required: &'static str },
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::MissingToken => f.write_str("a bearer token is required"),
            AuthError::InvalidToken(why) => write!(f, "the token is not valid ({why})"),
            AuthError::InsufficientScope { required } => {
                write!(f, "the token is missing the {required} scope")
            }
        }
    }
}

/// The by-`kid` JWKS cache with single-flight, rate-limited refresh.
///
/// Keys are cached by their `kid`; a lookup miss triggers at most one refresh
/// (guarded by a mutex so concurrent misses coalesce — single-flight) and only
/// if the rate limit has elapsed. On a refresh, *all* currently-published keys
/// are kept (rotation overlap), and a transient fetch failure leaves the existing
/// cache intact so already-cached keys keep validating (serve-stale).
pub struct JwksCache {
    /// The JWKS endpoint to fetch.
    jwks_uri: Arc<str>,
    /// The shared reqwest client (same style as the NetBox client).
    http: reqwest::Client,
    /// The cached state, behind a mutex. The mutex also serializes refreshes,
    /// giving single-flight for free.
    state: Mutex<CacheState>,
}

/// The mutable cache state: keys by `kid` plus the last-refresh instant.
struct CacheState {
    /// `kid` → JWK. A JWK with no `kid` is stored under the empty string so a
    /// single-key IdP still works.
    keys: HashMap<String, Jwk>,
    /// When the last refresh completed, for rate-limiting unknown-`kid` refreshes.
    last_refresh: Option<Instant>,
}

impl JwksCache {
    /// Build an empty cache for `jwks_uri` over `http`. The first validation
    /// triggers the initial fetch; nothing is fetched at construction.
    pub fn new(jwks_uri: Arc<str>, http: reqwest::Client) -> Self {
        Self {
            jwks_uri,
            http,
            state: Mutex::new(CacheState {
                keys: HashMap::new(),
                last_refresh: None,
            }),
        }
    }

    /// Resolve the decoding key for `kid`, fetching/refreshing if needed.
    ///
    /// Cache hit → use it. Miss → if the rate limit allows, refresh once
    /// (single-flight via the held lock) and retry the lookup; still missing →
    /// `None` (fail closed). A `None` `kid` matches a single unnamed key.
    async fn decoding_key_for(&self, kid: Option<&str>) -> Option<Jwk> {
        let key = kid.unwrap_or("");
        let mut state = self.state.lock().await;

        // Fast path: already cached, or empty cache on first use.
        if let Some(jwk) = state.keys.get(key) {
            return Some(jwk.clone());
        }
        // A single-key IdP with no `kid` in the token: if the cache holds exactly
        // one key, use it regardless of the lookup key.
        if kid.is_none() && state.keys.len() == 1 {
            return state.keys.values().next().cloned();
        }

        // Miss. Refresh at most once per rate-limit window (single-flight: we
        // hold the lock). On the very first use there's no prior refresh, so the
        // initial fetch always runs.
        let allow_refresh = state
            .last_refresh
            .is_none_or(|t| t.elapsed() >= JWKS_REFRESH_MIN_INTERVAL);
        if allow_refresh {
            self.refresh(&mut state).await;
            if let Some(jwk) = state.keys.get(key) {
                return Some(jwk.clone());
            }
            if kid.is_none() && state.keys.len() == 1 {
                return state.keys.values().next().cloned();
            }
        }
        None
    }

    /// Fetch the JWKS and replace the cache, keeping all published keys. A fetch
    /// or parse failure is logged and leaves the existing cache untouched
    /// (serve-stale); `last_refresh` is still bumped so the rate limit holds even
    /// on repeated failures. Never logs token/key material.
    async fn refresh(&self, state: &mut CacheState) {
        state.last_refresh = Some(Instant::now());
        let fetched = match self.fetch_jwks().await {
            Ok(set) => set,
            Err(e) => {
                tracing::warn!(jwks_uri = %self.jwks_uri, error = %e, "JWKS refresh failed; serving from cache");
                return;
            }
        };
        let mut keys = HashMap::with_capacity(fetched.keys.len());
        for jwk in fetched.keys {
            let kid = jwk.common.key_id.clone().unwrap_or_default();
            keys.insert(kid, jwk);
        }
        tracing::debug!(jwks_uri = %self.jwks_uri, count = keys.len(), "JWKS refreshed");
        state.keys = keys;
    }

    /// GET the JWKS document and parse it into a [`JwkSet`].
    async fn fetch_jwks(&self) -> anyhow::Result<JwkSet> {
        let resp = self
            .http
            .get(&*self.jwks_uri)
            .send()
            .await?
            .error_for_status()?;
        let set: JwkSet = resp.json().await?;
        Ok(set)
    }
}

/// Validate a bearer token from the `Authorization` header against `cfg`.
///
/// On success returns the caller's [`Identity`]. On failure returns an
/// [`AuthError`] that the middleware turns into the right 401/403 challenge.
/// Scope is **not** checked here (that's per-tool / per-route); the returned
/// identity carries the granted scopes for the caller to enforce.
pub async fn validate_bearer(
    cfg: &OidcConfig,
    auth_header: Option<&str>,
) -> Result<Identity, AuthError> {
    // 1) Extract the bearer. Tokens in the query string are never accepted; only
    //    the `Authorization: Bearer <token>` header.
    let token = bearer_token(auth_header).ok_or(AuthError::MissingToken)?;

    // 2) Decode just the header to pick the key by `kid`. A malformed header or a
    //    disallowed `alg` is rejected before any signature work.
    let header = decode_header(token).map_err(|_| AuthError::InvalidToken("malformed header"))?;
    if !ALLOWED_ALGS.contains(&header.alg) {
        return Err(AuthError::InvalidToken("unsupported algorithm"));
    }

    // 3) Select the JWK (cached by `kid`, refreshing once on a miss). A cache
    //    miss during an outage fails closed.
    let jwk = cfg
        .jwks
        .decoding_key_for(header.kid.as_deref())
        .await
        .ok_or(AuthError::InvalidToken("no matching key"))?;
    let decoding_key =
        DecodingKey::from_jwk(&jwk).map_err(|_| AuthError::InvalidToken("unusable key"))?;

    // 4) Validate signature + claims. The alg allowlist was enforced in step 2
    //    (`header.alg` is provably one of `ALLOWED_ALGS`), so `algorithms` here is
    //    that single verified alg — jsonwebtoken 10 requires the validation algs
    //    to match the key's family, and an RSA key can't carry both RS256+ES256.
    //    `none`/alg-confusion never reaches this point. Plus `iss` exact-match,
    //    `aud` contains the configured audience, and `exp` with the skew leeway.
    let mut validation = Validation::new(header.alg);
    validation.algorithms = vec![header.alg];
    validation.leeway = CLOCK_SKEW_LEEWAY_SECS;
    validation.set_issuer(&[cfg.issuer.as_ref()]);
    validation.set_audience(&[cfg.audience.as_ref()]);
    validation.set_required_spec_claims(&["exp", "iss", "aud"]);

    let data = decode::<Claims>(token, &decoding_key, &validation)
        .map_err(|e| AuthError::InvalidToken(classify_jwt_error(&e)))?;

    Ok(identity_from_claims(data.claims))
}

/// Map a `jsonwebtoken` error to a stable, non-leaking reason string for the
/// `error_description`. Never includes the token or any claim value.
fn classify_jwt_error(e: &jsonwebtoken::errors::Error) -> &'static str {
    use jsonwebtoken::errors::ErrorKind;
    match e.kind() {
        ErrorKind::ExpiredSignature => "expired",
        ErrorKind::InvalidIssuer => "wrong issuer",
        ErrorKind::InvalidAudience => "wrong audience",
        ErrorKind::InvalidSignature => "bad signature",
        ErrorKind::ImmatureSignature => "not yet valid",
        ErrorKind::MissingRequiredClaim(_) => "missing a required claim",
        _ => "invalid",
    }
}

/// Build the [`Identity`] from validated claims. `client_id` falls back to `azp`;
/// scopes merge `scope` (space-delimited) and `scp` (string or array).
fn identity_from_claims(claims: Claims) -> Identity {
    let mut scopes: Vec<String> = Vec::new();
    if let Some(scope) = &claims.scope {
        scopes.extend(scope.split_whitespace().map(str::to_string));
    }
    match claims.scp {
        Some(ScopeField::One(s)) => scopes.extend(s.split_whitespace().map(str::to_string)),
        Some(ScopeField::Many(many)) => scopes.extend(many),
        None => {}
    }
    scopes.sort();
    scopes.dedup();

    Identity {
        sub: claims.sub,
        client_id: claims.client_id.or(claims.azp),
        scopes,
        jti: claims.jti,
        iss: claims.iss,
    }
}

/// Extract the bearer token from an `Authorization` header value. Only the
/// `Bearer ` scheme (case-insensitive) is accepted; the token must be non-empty.
fn bearer_token(header: Option<&str>) -> Option<&str> {
    let value = header?;
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("Bearer") && !token.trim().is_empty() {
        Some(token.trim())
    } else {
        None
    }
}

/// Reject a non-HTTPS OIDC URL unless its host is loopback.
///
/// An IdP issuer / JWKS / discovered endpoint MUST be reached over HTTPS in a
/// routable deployment — a plain-`http://` IdP URL lets a network attacker swap
/// the signing keys and mint any token. The single exception is a loopback host
/// (`127.0.0.0/8` or `::1`, or `localhost`), for local development against a
/// throwaway IdP. `label` names the URL in the error (`issuer` / `JWKS URL` / …).
///
/// Returns a [`NboxError::Usage`] (exit 2) so a misconfiguration fails fast at
/// startup with a clear message rather than fetching keys over plaintext.
pub fn require_https_or_loopback(url: &str, label: &str) -> Result<(), crate::error::NboxError> {
    // Parse just enough to read the scheme + host. We don't pull a URL crate in
    // for this — split the scheme, then the authority, then the host.
    let (scheme, host_port) = absolute_url_host_port(url, label)?;
    if scheme == "https" {
        return Ok(());
    }
    if scheme != "http" {
        return Err(crate::error::NboxError::Usage(format!(
            "the OIDC {label} ({url}) must use https (got scheme \"{scheme}\")"
        )));
    }
    // Plain http:// — allowed only for a loopback host (local dev).
    if host_is_loopback(host_port) {
        return Ok(());
    }
    Err(crate::error::NboxError::Usage(format!(
        "the OIDC {label} ({url}) uses plain http:// for a non-loopback host. Use https \
         (TLS is mandatory for an IdP reachable over the network); plain http:// is \
         accepted only for a loopback host (127.0.0.0/8 or ::1) in local development."
    )))
}

/// Return the lowercased scheme and authority host/port from an absolute URL.
///
/// This is intentionally lightweight, matching [`require_https_or_loopback`]'s
/// needs: it rejects missing schemes and host-less authorities up front so
/// malformed `https://` values fail as usage errors instead of surfacing later as
/// discovery/fetch failures.
fn absolute_url_host_port<'a>(
    url: &'a str,
    label: &str,
) -> Result<(String, &'a str), crate::error::NboxError> {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => {
            return Err(crate::error::NboxError::Usage(format!(
                "the OIDC {label} ({url}) is not an absolute http(s) URL"
            )));
        }
    };
    if scheme != "http" && scheme != "https" {
        return Err(crate::error::NboxError::Usage(format!(
            "the OIDC {label} ({url}) must use https (got scheme \"{scheme}\")"
        )));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
    if host_port.is_empty() || host_port.starts_with(':') {
        return Err(crate::error::NboxError::Usage(format!(
            "the OIDC {label} ({url}) is not an absolute http(s) URL with a host"
        )));
    }
    Ok((scheme, host_port))
}

/// The hard cap on redirect hops for the OIDC HTTP client. IdP discovery/JWKS
/// endpoints normally answer `200` directly; a single redirect (e.g. a trailing
/// `/` normalization) is tolerated, but a chain is treated as suspicious.
const OIDC_MAX_REDIRECTS: usize = 3;

/// Build the reqwest client nbox uses for IdP calls (discovery + JWKS), with a
/// **redirect-safe** policy: every redirect hop's target URL is re-checked with
/// [`require_https_or_loopback`], so an `https://` issuer/JWKS endpoint can't
/// `30x`-redirect the fetch down to a plain-`http://` non-loopback URL and
/// silently downgrade the transport the validation was meant to guarantee.
///
/// The original-URL checks in the caller still run (defense in depth); this
/// closes the gap that they only covered the *first* hop. A non-HTTPS,
/// non-loopback redirect target makes the request fail (not follow); the chain
/// is also capped at [`OIDC_MAX_REDIRECTS`] hops to bound redirect loops.
pub fn build_oidc_http_client() -> anyhow::Result<reqwest::Client> {
    let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
        // Re-apply the https-or-loopback rule to THIS hop's target. A plain-http
        // non-loopback target is refused (the request errors), never followed.
        if let Err(e) = require_https_or_loopback(attempt.url().as_str(), "redirected URL") {
            return attempt.error(e);
        }
        if attempt.previous().len() >= OIDC_MAX_REDIRECTS {
            return attempt.error(format!(
                "OIDC request exceeded {OIDC_MAX_REDIRECTS} redirects"
            ));
        }
        attempt.follow()
    });
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(redirect_policy)
        .build()
        .map_err(|e| anyhow::anyhow!("building the OIDC HTTP client: {e}"))
}

/// Whether `host_port` (`host`, `host:port`, `[v6]`, or `[v6]:port`) is a
/// loopback host: any `127.0.0.0/8` / `::1` literal, or the name `localhost`.
fn host_is_loopback(host_port: &str) -> bool {
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        // IPv6 literal: `[::1]` or `[::1]:8080`.
        rest.split_once(']').map_or(rest, |(h, _)| h)
    } else {
        host_port
            .rsplit_once(':')
            .filter(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
            .map_or(host_port, |(h, _)| h)
    };
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

/// Discover the JWKS URI for `issuer`.
///
/// Tries the OpenID Connect discovery document first
/// (`<issuer>/.well-known/openid-configuration`), then the OAuth 2.0
/// authorization-server metadata (`<issuer>/.well-known/oauth-authorization-server`).
/// The first to yield a `jwks_uri` wins. A discovered `jwks_uri` is held to the
/// same https-or-loopback rule as a configured one (an attacker controlling the
/// discovery document must not be able to point nbox at a plaintext JWKS).
pub async fn discover_jwks_uri(http: &reqwest::Client, issuer: &str) -> anyhow::Result<String> {
    /// The one field of the discovery document we need.
    #[derive(Deserialize)]
    struct Discovery {
        jwks_uri: Option<String>,
    }

    let base = issuer.trim_end_matches('/');
    let candidates = [
        format!("{base}/.well-known/openid-configuration"),
        format!("{base}/.well-known/oauth-authorization-server"),
    ];

    let mut last_err: Option<anyhow::Error> = None;
    for url in candidates {
        match http
            .get(&url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
        {
            Ok(resp) => match resp.json::<Discovery>().await {
                Ok(doc) => {
                    if let Some(uri) = doc.jwks_uri.filter(|u| !u.is_empty()) {
                        // A discovered JWKS URL is held to the same https-or-loopback
                        // rule as a configured one — don't fetch keys over plaintext
                        // just because the discovery doc said so.
                        require_https_or_loopback(&uri, "discovered JWKS URL")?;
                        return Ok(uri);
                    }
                    last_err = Some(anyhow::anyhow!("{url} has no jwks_uri"));
                }
                Err(e) => last_err = Some(e.into()),
            },
            Err(e) => last_err = Some(e.into()),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no discovery endpoint responded")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_extraction_accepts_only_the_bearer_scheme() {
        assert_eq!(
            bearer_token(Some("Bearer abc.def.ghi")),
            Some("abc.def.ghi")
        );
        // Scheme is case-insensitive.
        assert_eq!(bearer_token(Some("bearer xyz")), Some("xyz"));
        // Other schemes / missing token are rejected.
        assert_eq!(bearer_token(Some("Token abc")), None);
        assert_eq!(bearer_token(Some("Basic abc")), None);
        assert_eq!(bearer_token(Some("Bearer ")), None);
        assert_eq!(bearer_token(Some("Bearer   ")), None);
        assert_eq!(bearer_token(None), None);
    }

    #[test]
    fn scopes_merge_scope_string_and_scp_array() {
        let claims = Claims {
            sub: Some("user-1".into()),
            iss: Some("https://idp".into()),
            client_id: None,
            azp: Some("client-9".into()),
            jti: Some("jti-1".into()),
            scope: Some("nbox:read profile".into()),
            scp: Some(ScopeField::Many(vec!["email".into(), "nbox:read".into()])),
        };
        let id = identity_from_claims(claims);
        // `client_id` falls back to `azp`.
        assert_eq!(id.client_id.as_deref(), Some("client-9"));
        // Scopes from both fields, deduped.
        assert!(id.has_scope("nbox:read"));
        assert!(id.has_scope("profile"));
        assert!(id.has_scope("email"));
        assert!(!id.has_scope("nbox:write"));
        assert_eq!(id.sub.as_deref(), Some("user-1"));
        assert_eq!(id.jti.as_deref(), Some("jti-1"));
    }

    #[test]
    fn https_urls_are_always_accepted() {
        assert!(require_https_or_loopback("https://idp.example.com", "issuer").is_ok());
        assert!(
            require_https_or_loopback("https://idp.example.com/.well-known/jwks", "JWKS URL")
                .is_ok()
        );
        // Scheme is matched case-insensitively.
        assert!(require_https_or_loopback("HTTPS://idp.example.com", "issuer").is_ok());
    }

    #[test]
    fn absolute_https_urls_must_have_a_host() {
        for url in ["https://", "https:///jwks", "https://?x=1", "https://#frag"] {
            let err = require_https_or_loopback(url, "issuer").unwrap_err();
            assert!(
                matches!(err, crate::error::NboxError::Usage(_)),
                "{url} should be a Usage error"
            );
            assert_eq!(err.exit_code(), 2, "{url} should exit 2");
            assert!(
                format!("{err:#}").contains("with a host"),
                "{url} should explain the missing host"
            );
        }
    }

    #[test]
    fn plain_http_non_loopback_is_rejected_as_usage() {
        for url in [
            "http://idp.example.com",
            "http://idp.example.com:8080/jwks",
            "http://192.168.1.10/keys",
            "http://[2001:db8::1]:8080/jwks",
        ] {
            let err = require_https_or_loopback(url, "issuer").unwrap_err();
            assert!(
                matches!(err, crate::error::NboxError::Usage(_)),
                "{url} should be a Usage error"
            );
            assert_eq!(err.exit_code(), 2, "{url} should exit 2");
        }
    }

    #[test]
    fn plain_http_loopback_is_allowed_for_dev() {
        for url in [
            "http://127.0.0.1:8080",
            "http://127.0.0.5/jwks",
            "http://localhost:9000/.well-known/openid-configuration",
            "http://[::1]:8080/keys",
            "http://localhost",
        ] {
            assert!(
                require_https_or_loopback(url, "JWKS URL").is_ok(),
                "{url} (loopback) should be allowed"
            );
        }
    }

    #[test]
    fn non_http_schemes_and_garbage_are_rejected() {
        assert!(require_https_or_loopback("ftp://idp.example.com", "issuer").is_err());
        assert!(require_https_or_loopback("idp.example.com", "issuer").is_err());
        assert!(require_https_or_loopback("", "issuer").is_err());
    }

    #[test]
    fn client_id_preferred_over_azp() {
        let claims = Claims {
            sub: None,
            iss: None,
            client_id: Some("explicit".into()),
            azp: Some("fallback".into()),
            jti: None,
            scope: None,
            scp: None,
        };
        let id = identity_from_claims(claims);
        assert_eq!(id.client_id.as_deref(), Some("explicit"));
        assert!(id.scopes.is_empty());
    }

    // --- (H2): the OIDC HTTP client is redirect-safe (no HTTPS downgrade) -------
    //
    // The client built by `build_oidc_http_client` re-checks https-or-loopback on
    // EVERY redirect hop, so a 30x to a plain-http non-loopback URL is refused
    // (the request errors) rather than followed — the transport the original-URL
    // check guaranteed can't be downgraded by a redirect. A loopback http hop is
    // still allowed (local dev), mirroring the original-URL rule.

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn redirect_to_plain_http_non_loopback_is_refused() {
        // The IdP (a loopback wiremock server) answers discovery/JWKS with a 302 to
        // a plain-http NON-loopback URL. The redirect-safe policy must refuse to
        // follow it — the fetch errors instead of downgrading to http://attacker.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", "http://attacker.example.com/jwks"),
            )
            .mount(&server)
            .await;

        let http = build_oidc_http_client().expect("client builds");
        let res = http.get(format!("{}/jwks", server.uri())).send().await;
        // The request fails (the redirect was not followed to plain http).
        assert!(
            res.is_err(),
            "a redirect to a plain-http non-loopback URL must fail, not be followed"
        );
        let err = res.unwrap_err();
        // It's a redirect-policy refusal, not a transport error reaching the target.
        assert!(
            err.is_redirect() || err.to_string().contains("redirect"),
            "expected a redirect-policy error, got: {err}"
        );
    }

    #[tokio::test]
    async fn redirect_to_loopback_http_is_followed() {
        // A redirect whose target is ANOTHER loopback http URL is allowed (local
        // dev): the policy permits a loopback hop, so the fetch follows it and the
        // final 200 body comes back.
        let server = MockServer::start().await;
        // `/start` redirects to `/end` on the same (loopback) server.
        let end = format!("{}/end", server.uri());
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", end.as_str()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/end"))
            .respond_with(ResponseTemplate::new(200).set_body_string("arrived"))
            .mount(&server)
            .await;

        let http = build_oidc_http_client().expect("client builds");
        let body = http
            .get(format!("{}/start", server.uri()))
            .send()
            .await
            .expect("loopback http redirect should be followed")
            .text()
            .await
            .expect("body");
        assert_eq!(body, "arrived");
    }

    #[tokio::test]
    async fn redirect_chain_is_capped() {
        // A redirect loop is bounded: even all-loopback hops are capped so a chain
        // can't spin forever. The server bounces `/loop` to itself; the client
        // gives up with a redirect error after the cap.
        let server = MockServer::start().await;
        let self_url = format!("{}/loop", server.uri());
        Mock::given(method("GET"))
            .and(path("/loop"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", self_url.as_str()))
            .mount(&server)
            .await;

        let http = build_oidc_http_client().expect("client builds");
        let res = http.get(&self_url).send().await;
        assert!(res.is_err(), "an unbounded redirect loop must error");
    }
}
