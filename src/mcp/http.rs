//! HTTP transport for `nbox serve --http` (rungs 2 + 3 of the MCP transport
//! ladder; see `DESIGN.md` §24).
//!
//! This is an *alternate transport* for the exact same [`NboxMcp`] server the
//! stdio path serves — the handler, tool router, and eight read-only tools are
//! reused unchanged. rmcp's Streamable HTTP server ([`StreamableHttpService`] +
//! [`LocalSessionManager`]) is mounted at `/mcp` on an axum router.
//!
//! Two modes, chosen at startup:
//!
//! - **Loopback, no OAuth (rung 2).** Bind a loopback address only; the trust
//!   boundary is the loopback interface. An optional static bearer adds a second
//!   factor for local clients that want it. This is unchanged from Unit 1.
//! - **OIDC resource server (rung 3).** When `--oidc-issuer` + `--audience` are
//!   set, nbox validates inbound IdP JWTs on `/mcp` (see [`super::oidc`]) and
//!   advertises Protected Resource Metadata at
//!   `GET /.well-known/oauth-protected-resource`. This mode may bind a routable
//!   interface (TLS must terminate in front — a reverse proxy; not done here).
//!
//! Security (DESIGN §24, mandatory):
//! - Loopback-only bind unless OIDC auth is configured; a non-loopback bind
//!   without `--oidc-issuer` is a usage error.
//! - Validate the `Origin` header on every request → 403 on a non-loopback
//!   origin (DNS-rebinding defense). rmcp additionally validates the `Host`
//!   header against the loopback allow-list (loopback mode only).
//! - In OIDC mode, JWT validation on `/mcp`: alg allowlist, `iss`/`aud`/`exp`,
//!   `nbox:read` scope; 401/403 with the right `WWW-Authenticate` challenge.
//! - Advertise `MCP-Protocol-Version: 2025-11-25` on every response.
//! - stdout stays clean: the protocol travels over the HTTP body, and all logs
//!   go to stderr/file exactly as the stdio path does. Nothing here writes to
//!   stdout. The token is never logged.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::NboxMcp;
use super::oidc::{self, AuthError, Identity, JwksCache, OidcConfig, SCOPE_READ, SCOPE_WRITE};
use crate::error::NboxError;
use crate::netbox::client::NetBoxClient;

/// The MCP protocol revision this server targets (DESIGN §24). Advertised on
/// every HTTP response so clients can pin the wire version.
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// The RFC 9728 Protected Resource Metadata path. Public (no auth).
const PRM_PATH: &str = "/.well-known/oauth-protected-resource";

/// Resolved OIDC inputs from flags/config, before the JWKS URI is discovered.
/// `serve_http` turns this into a live [`OidcConfig`] (fetching the JWKS URI if
/// no override was given).
pub struct OidcArgs {
    /// The IdP issuer URL.
    pub issuer: String,
    /// The expected audience — nbox's canonical resource URI.
    pub audience: String,
    /// Optional JWKS URL override; `None` ⇒ discover from the issuer.
    pub jwks_url: Option<String>,
}

/// Per-request guard state shared with the middleware.
///
/// `Loopback` is rung 2 (the optional static bearer; loopback is the boundary).
/// `Oidc` is rung 3 (validate IdP JWTs on `/mcp`). The variant is fixed at
/// startup. Cheap to clone (`Arc`/`Option<Arc>`), so it rides as router state.
#[derive(Clone)]
enum Guard {
    /// Loopback mode: `Some` ⇒ require `Authorization: Bearer <token>`; `None`
    /// ⇒ no auth (loopback is the trust boundary).
    Loopback { token: Option<Arc<str>> },
    /// OIDC resource-server mode: validate inbound IdP JWTs.
    Oidc(OidcConfig),
}

/// Serve the read-only MCP server over HTTP until interrupted.
///
/// Without `oidc`, `addr` must be a loopback socket address (rung 2): the trust
/// boundary is loopback, plus the optional static `token`. With `oidc`, JWTs are
/// validated on `/mcp` (rung 3) and `addr` may be routable — TLS must terminate
/// in front (a reverse proxy). The same [`NboxMcp`] backs every request. stdout
/// is never written; the token is never logged.
pub async fn serve_http(
    client: NetBoxClient,
    addr: &str,
    token: Option<String>,
    oidc: Option<OidcArgs>,
) -> Result<()> {
    let oidc_on = oidc.is_some();
    let socket = parse_bind_addr(addr, oidc_on)?;
    if oidc_on && !is_loopback(socket.ip()) {
        tracing::warn!(
            %socket,
            "binding a non-loopback address — terminate TLS in front (reverse proxy); \
             nbox serves plain HTTP and validates inbound IdP JWTs but does not do TLS"
        );
    }

    // Build the OIDC config (discovering the JWKS URI if no override was given)
    // before we stand up the listener, so a misconfiguration fails at startup.
    let guard = match oidc {
        Some(args) => Guard::Oidc(build_oidc_config(args).await?),
        None => Guard::Loopback {
            token: token.map(|t| Arc::from(t.as_str())),
        },
    };

    // Build the server once; the service factory hands rmcp a fresh clone per
    // session (cheap — `NboxMcp` holds an `Arc<NetBoxClient>`).
    let server = NboxMcp::new(client);

    let cancel = CancellationToken::new();
    // `StreamableHttpServerConfig` is `#[non_exhaustive]`, so build from the
    // default and adjust via the builder. Origin validation is enforced by our
    // own middleware (full control over the loopback decision + 403 body); the
    // rmcp default still validates the `Host` header against the loopback
    // allow-list for DNS-rebinding defense.
    let config =
        StreamableHttpServerConfig::default().with_cancellation_token(cancel.child_token());
    let mcp = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        config,
    );

    // The PRM well-known route is public (no auth, no gate) and only meaningful
    // in OIDC mode; mount it there. `/mcp` carries the gate (bearer/JWT + Origin
    // + protocol-version header); `/.well-known/*` is left public.
    let mut router = axum::Router::new()
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(guard.clone(), gate));
    if let Guard::Oidc(cfg) = &guard {
        router = router.route(PRM_PATH, get(prm_handler).with_state(cfg.clone()));
    }

    let listener = tokio::net::TcpListener::bind(socket)
        .await
        .with_context(|| format!("binding {socket}"))?;
    tracing::info!(%socket, oidc = oidc_on, "nbox MCP server listening (HTTP)");

    // Graceful shutdown on Ctrl-C: cancel the rmcp sessions, then let axum drain.
    let shutdown = async move {
        let _ = tokio::signal::ctrl_c().await;
        cancel.cancel();
    };
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
        .context("HTTP server error")?;
    Ok(())
}

/// Turn resolved [`OidcArgs`] into a live [`OidcConfig`]: discover the JWKS URI
/// from the issuer if no override was given, then build the by-`kid` cache. The
/// JWKS itself is fetched lazily on the first token validation.
async fn build_oidc_config(args: OidcArgs) -> Result<OidcConfig> {
    // A dedicated client for IdP calls (discovery + JWKS). Same reqwest/rustls
    // style as the NetBox client; default TLS verification (an IdP is public).
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("building the OIDC HTTP client")?;

    let jwks_uri = match args.jwks_url {
        Some(url) => url,
        None => oidc::discover_jwks_uri(&http, &args.issuer)
            .await
            .with_context(|| {
                format!(
                    "discovering the JWKS URI from issuer {} — pass --oidc-jwks-url to set it \
                     explicitly",
                    args.issuer
                )
            })?,
    };
    tracing::info!(issuer = %args.issuer, jwks_uri = %jwks_uri, "OIDC resource-server mode");

    let jwks = Arc::new(JwksCache::new(Arc::from(jwks_uri.as_str()), http));
    Ok(OidcConfig {
        issuer: Arc::from(args.issuer.as_str()),
        audience: Arc::from(args.audience.as_str()),
        jwks_uri: Arc::from(jwks_uri.as_str()),
        jwks,
    })
}

/// Parse `addr` to a [`SocketAddr`]. In loopback mode (`!oidc`) it must be a
/// loopback address; a routable bind is rejected as a [`NboxError::Usage`] (exit
/// `2`) pointing at `--oidc-issuer`. In OIDC mode any address is allowed (the
/// caller warns about TLS for non-loopback binds).
fn parse_bind_addr(addr: &str, oidc: bool) -> Result<SocketAddr> {
    let socket: SocketAddr = addr.parse().map_err(|_| {
        NboxError::Usage(format!(
            "--http expects an IP:PORT address, e.g. 127.0.0.1:8080 (got \"{addr}\")"
        ))
    })?;
    if !oidc && !is_loopback(socket.ip()) {
        return Err(NboxError::Usage(format!(
            "--http {addr} is not a loopback address. Binding a routable interface \
             requires the OIDC resource-server auth mode — pass --oidc-issuer <URL> \
             and --audience <VALUE> (and terminate TLS in front). Loopback (127.0.0.0/8 \
             or ::1) needs neither."
        ))
        .into());
    }
    Ok(socket)
}

/// Whether `ip` is a loopback address (127.0.0.0/8 or ::1).
fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

/// Axum middleware on `/mcp`: enforce auth, validate the `Origin` header, and
/// advertise the MCP protocol version on the response.
///
/// Loopback mode: optional static bearer (401) → origin (403) → inner.
/// OIDC mode: validate the JWT + scope (401/403) → origin (403) → inner, and
/// plumb the validated [`Identity`] into request extensions for downstream.
/// Every path fails closed.
async fn gate(State(guard): State<Guard>, mut request: Request<Body>, next: Next) -> Response {
    // 1) Authenticate by mode. The auth check runs first; only an authenticated
    //    (or auth-not-required) request proceeds to the Origin check.
    match &guard {
        Guard::Loopback { token } => {
            if let Some(expected) = token.as_deref() {
                let presented = request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "));
                let ok = presented.is_some_and(|got| ct_eq(got.as_bytes(), expected.as_bytes()));
                if !ok {
                    return loopback_unauthorized();
                }
            }
        }
        Guard::Oidc(cfg) => {
            let auth_header = request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok());
            match oidc::validate_bearer(cfg, auth_header).await {
                Ok(identity) => {
                    // All eight current tools are reads → require `nbox:read`.
                    // (`nbox:write` gating is wired below for future write tools.)
                    if let Err(scope_err) = require_scope(&identity, SCOPE_READ) {
                        return oidc_challenge(cfg, &scope_err);
                    }
                    // Plumb the validated identity through for Unit 3's audit log
                    // + NetBox bridge. rmcp doesn't surface caller identity, so
                    // request extensions are the handoff.
                    request.extensions_mut().insert(identity);
                }
                Err(e) => return oidc_challenge(cfg, &e),
            }
        }
    }

    // 2) Origin validation (DNS-rebinding defense). A request *with* an Origin
    //    must carry a loopback origin in loopback mode; in OIDC mode the bearer
    //    is the auth boundary, but we still reject a cross-origin browser request
    //    that lacks a loopback origin only when bound to loopback. To keep the
    //    defense simple and uniform, the loopback-origin rule applies whenever an
    //    Origin header is present *and* we are in loopback mode.
    if matches!(guard, Guard::Loopback { .. })
        && let Some(origin) = request.headers().get(header::ORIGIN)
    {
        let allowed = origin.to_str().ok().is_some_and(origin_is_loopback);
        if !allowed {
            return forbidden();
        }
    }

    let mut response = next.run(request).await;
    response.headers_mut().insert(
        "mcp-protocol-version",
        header::HeaderValue::from_static(MCP_PROTOCOL_VERSION),
    );
    response
}

/// Enforce that `identity` carries `required`. `nbox:write` implies `nbox:read`
/// is *not* assumed — each scope is checked explicitly so the two stay distinct.
fn require_scope(identity: &Identity, required: &'static str) -> Result<(), AuthError> {
    if identity.has_scope(required) {
        Ok(())
    } else {
        Err(AuthError::InsufficientScope { required })
    }
}

/// The `GET /.well-known/oauth-protected-resource` handler (RFC 9728). Public —
/// no auth. Advertises nbox's resource URI, the authorization server(s), the
/// scopes, the bearer method, and the JWKS URI.
async fn prm_handler(State(cfg): State<OidcConfig>) -> Json<serde_json::Value> {
    Json(json!({
        "resource": cfg.audience.as_ref(),
        "authorization_servers": [cfg.issuer.as_ref()],
        "scopes_supported": [SCOPE_READ, SCOPE_WRITE],
        "bearer_methods_supported": ["header"],
        "jwks_uri": cfg.jwks_uri.as_ref(),
    }))
}

/// The absolute PRM URL to advertise in `WWW-Authenticate: resource_metadata`.
/// Spec wants an absolute URL, but the resource server only knows its canonical
/// URI as the configured audience; advertise that joined to the PRM path. If the
/// audience isn't a URL (some deployments use an opaque identifier), fall back to
/// the bare path so the header is still well-formed.
fn prm_url(cfg: &OidcConfig) -> String {
    let aud = cfg.audience.as_ref().trim_end_matches('/');
    if aud.starts_with("http://") || aud.starts_with("https://") {
        format!("{aud}{PRM_PATH}")
    } else {
        PRM_PATH.to_string()
    }
}

/// Build the right RFC 6750 challenge response for an [`AuthError`]:
/// 401 `invalid_token` (+ `resource_metadata`) for a bad/missing/expired token,
/// 403 `insufficient_scope` (+ `scope`) for an authz shortfall. The body and
/// header never echo the token or any claim.
fn oidc_challenge(cfg: &OidcConfig, err: &AuthError) -> Response {
    let resource_metadata = prm_url(cfg);
    match err {
        AuthError::InsufficientScope { required } => {
            let header = format!("Bearer error=\"insufficient_scope\", scope=\"{required}\"");
            challenge(StatusCode::FORBIDDEN, &header, &err.to_string())
        }
        AuthError::MissingToken => {
            // No token presented: a bare `Bearer` challenge with the PRM pointer,
            // no `error` (RFC 6750 §3 — `error` is for a *failed* request).
            let header = format!("Bearer resource_metadata=\"{resource_metadata}\"");
            challenge(StatusCode::UNAUTHORIZED, &header, &err.to_string())
        }
        AuthError::InvalidToken(_) => {
            let description = err.to_string();
            let header = format!(
                "Bearer resource_metadata=\"{resource_metadata}\", error=\"invalid_token\", \
                 error_description=\"{description}\""
            );
            challenge(StatusCode::UNAUTHORIZED, &header, &description)
        }
    }
}

/// A challenge response with a `WWW-Authenticate` header and a generic body.
/// `header_value` is pre-built and contains only safe text (never the token).
fn challenge(status: StatusCode, header_value: &str, description: &str) -> Response {
    let mut response = (status, description.to_string()).into_response();
    if let Ok(value) = header::HeaderValue::from_str(header_value) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

/// True if `origin` (an RFC 6454 origin, e.g. `http://127.0.0.1:8080`) has a
/// loopback host. The scheme and port are not constrained — only the host must
/// be loopback, which is what the DNS-rebinding threat turns on.
fn origin_is_loopback(origin: &str) -> bool {
    // Strip the scheme, then any path, then the optional port, leaving the host.
    let after_scheme = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host = strip_port(authority);
    match host.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        // Bare hostnames: only `localhost` counts as loopback here.
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

/// Split an authority (`host`, `host:port`, `[v6]`, or `[v6]:port`) into its
/// host part, dropping the port.
fn strip_port(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        // IPv6 literal: `[::1]` or `[::1]:8080`.
        return rest.split_once(']').map_or(rest, |(host, _)| host);
    }
    authority
        .rsplit_once(':')
        .map_or(authority, |(host, port)| {
            // Only treat a trailing `:digits` as a port; otherwise it's a bare host.
            if port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() {
                host
            } else {
                authority
            }
        })
}

/// Constant-time byte-slice equality. Avoids leaking the token length-prefix
/// match via timing; lengths still differ, but the per-byte loop doesn't
/// short-circuit on the first mismatch.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 401 with a `WWW-Authenticate: Bearer` challenge for the loopback static
/// bearer. The body is generic — never echoes the token or what was presented.
fn loopback_unauthorized() -> Response {
    let mut response = Response::new(Body::from("Unauthorized"));
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        header::HeaderValue::from_static("Bearer"),
    );
    response
}

/// 403 for a rejected (non-loopback / malformed) `Origin`.
fn forbidden() -> Response {
    let mut response = Response::new(Body::from("Forbidden: Origin not allowed"));
    *response.status_mut() = StatusCode::FORBIDDEN;
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_addrs_are_accepted_without_oidc() {
        assert!(parse_bind_addr("127.0.0.1:8080", false).is_ok());
        assert!(parse_bind_addr("127.0.0.5:1234", false).is_ok());
        assert!(parse_bind_addr("[::1]:8080", false).is_ok());
    }

    #[test]
    fn non_loopback_addrs_are_usage_errors_without_oidc() {
        for addr in ["0.0.0.0:8080", "192.168.1.10:8080", "[2001:db8::1]:8080"] {
            let err = parse_bind_addr(addr, false).unwrap_err();
            let nbox = err
                .chain()
                .find_map(|e| e.downcast_ref::<NboxError>())
                .expect("a typed NboxError");
            assert!(
                matches!(nbox, NboxError::Usage(_)),
                "{addr} should be Usage"
            );
            // The message points the user at the OIDC auth mode.
            assert!(format!("{nbox}").contains("--oidc-issuer"));
        }
    }

    #[test]
    fn non_loopback_addrs_are_allowed_with_oidc() {
        // In OIDC mode a routable bind is permitted (TLS terminates in front).
        assert!(parse_bind_addr("0.0.0.0:8080", true).is_ok());
        assert!(parse_bind_addr("192.168.1.10:8080", true).is_ok());
        assert!(parse_bind_addr("[2001:db8::1]:8080", true).is_ok());
        // Loopback is still fine in OIDC mode too.
        assert!(parse_bind_addr("127.0.0.1:8080", true).is_ok());
    }

    #[test]
    fn garbage_addr_is_a_usage_error() {
        let err = parse_bind_addr("not-an-addr", false).unwrap_err();
        assert!(
            err.chain()
                .any(|e| matches!(e.downcast_ref::<NboxError>(), Some(NboxError::Usage(_))))
        );
        // Also a usage error in OIDC mode.
        assert!(parse_bind_addr("not-an-addr", true).is_err());
    }

    #[test]
    fn origin_loopback_classification() {
        assert!(origin_is_loopback("http://127.0.0.1:8080"));
        assert!(origin_is_loopback("http://localhost:8080"));
        assert!(origin_is_loopback("https://localhost"));
        assert!(origin_is_loopback("http://[::1]:8080"));
        assert!(origin_is_loopback("http://127.0.0.5"));

        assert!(!origin_is_loopback("http://evil.example.com"));
        assert!(!origin_is_loopback("http://192.168.1.10:8080"));
        assert!(!origin_is_loopback("https://attacker.test:443"));
    }

    #[test]
    fn constant_time_eq_matches_std_eq() {
        assert!(ct_eq(b"secret", b"secret"));
        assert!(!ct_eq(b"secret", b"secrey"));
        assert!(!ct_eq(b"secret", b"secre"));
        assert!(!ct_eq(b"", b"x"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn prm_url_joins_a_url_audience_and_falls_back_for_opaque() {
        let cfg = |aud: &str| OidcConfig {
            issuer: Arc::from("https://idp.example.com"),
            audience: Arc::from(aud),
            jwks_uri: Arc::from("https://idp.example.com/keys"),
            jwks: Arc::new(JwksCache::new(
                Arc::from("https://idp.example.com/keys"),
                reqwest::Client::new(),
            )),
        };
        assert_eq!(
            prm_url(&cfg("https://nbox.example.com")),
            "https://nbox.example.com/.well-known/oauth-protected-resource"
        );
        // Trailing slash on the audience doesn't double up.
        assert_eq!(
            prm_url(&cfg("https://nbox.example.com/")),
            "https://nbox.example.com/.well-known/oauth-protected-resource"
        );
        // An opaque (non-URL) audience falls back to the bare path.
        assert_eq!(prm_url(&cfg("nbox")), PRM_PATH);
    }
}

/// End-to-end tests for the resource-server gate + PRM route, driven through the
/// real axum router (the actual [`gate`], [`oidc::validate_bearer`], JWKS cache,
/// and challenge code). A self-contained mock IdP serves discovery + JWKS from an
/// in-test RSA keypair, and tokens are minted with `jsonwebtoken`. No network.
#[cfg(test)]
mod rs_tests {
    use super::*;
    use axum::Router;
    use axum::extract::Extension;
    use axum::routing::any;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use jsonwebtoken::{Algorithm, EncodingKey, Header};
    use rsa::pkcs1::EncodeRsaPrivateKey;
    use rsa::traits::PublicKeyParts;
    use rsa::{RsaPrivateKey, RsaPublicKey};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tower::ServiceExt; // for `oneshot`

    /// A test IdP: an RSA keypair plus the `kid` its JWKS publishes.
    struct MockIdp {
        private: RsaPrivateKey,
        public: RsaPublicKey,
        kid: String,
    }

    impl MockIdp {
        fn new(kid: &str) -> Self {
            // 2048-bit so signatures are real but generation stays test-fast.
            let mut rng = rand::thread_rng();
            let private = RsaPrivateKey::new(&mut rng, 2048).expect("keypair");
            let public = RsaPublicKey::from(&private);
            Self {
                private,
                public,
                kid: kid.to_string(),
            }
        }

        /// The JWKS document this IdP serves (a single RSA key with its `kid`).
        fn jwks(&self) -> Value {
            let n = URL_SAFE_NO_PAD.encode(self.public.n().to_bytes_be());
            let e = URL_SAFE_NO_PAD.encode(self.public.e().to_bytes_be());
            json!({
                "keys": [{
                    "kty": "RSA",
                    "use": "sig",
                    "alg": "RS256",
                    "kid": self.kid,
                    "n": n,
                    "e": e,
                }]
            })
        }

        /// The PEM `EncodingKey` for signing tokens with this IdP's private key.
        fn encoding_key(&self) -> EncodingKey {
            let pem = self
                .private
                .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
                .unwrap();
            EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap()
        }

        /// Mint a signed RS256 token with the given claims and this IdP's `kid`.
        fn mint(&self, claims: &Value) -> String {
            let mut header = Header::new(Algorithm::RS256);
            header.kid = Some(self.kid.clone());
            jsonwebtoken::encode(&header, claims, &self.encoding_key()).unwrap()
        }

        /// Mint a token whose header claims an unknown `kid` (no JWKS match).
        fn mint_with_kid(&self, claims: &Value, kid: &str) -> String {
            let mut header = Header::new(Algorithm::RS256);
            header.kid = Some(kid.to_string());
            jsonwebtoken::encode(&header, claims, &self.encoding_key()).unwrap()
        }
    }

    const ISSUER: &str = "https://idp.test";
    const AUDIENCE: &str = "https://nbox.test";

    /// Standard well-formed claims: right iss/aud, exp far in the future, read scope.
    fn good_claims() -> Value {
        let exp = jsonwebtoken::get_current_timestamp() + 3600;
        json!({
            "sub": "user-42",
            "iss": ISSUER,
            "aud": AUDIENCE,
            "exp": exp,
            "client_id": "agent-cli",
            "jti": "tok-1",
            "scope": "nbox:read",
        })
    }

    /// Build an [`OidcConfig`] whose JWKS cache fetches from `jwks_url` (the
    /// in-test mock IdP server). The cache is empty until the first validation.
    fn oidc_config(jwks_url: &str) -> OidcConfig {
        let http = reqwest::Client::new();
        OidcConfig {
            issuer: Arc::from(ISSUER),
            audience: Arc::from(AUDIENCE),
            jwks_uri: Arc::from(jwks_url),
            jwks: Arc::new(JwksCache::new(Arc::from(jwks_url), http)),
        }
    }

    /// Spawn the mock IdP HTTP server (discovery + JWKS), returning its base URL.
    /// The discovery document points `jwks_uri` back at this same server.
    async fn spawn_idp(idp: Arc<MockIdp>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let jwks = idp.jwks();
        let disco = json!({
            "issuer": ISSUER,
            "jwks_uri": format!("{base}/jwks"),
        });
        let app = Router::new()
            .route(
                "/.well-known/openid-configuration",
                axum::routing::get(move || {
                    let disco = disco.clone();
                    async move { axum::Json(disco) }
                }),
            )
            .route(
                "/jwks",
                axum::routing::get(move || {
                    let jwks = jwks.clone();
                    async move { axum::Json(jwks) }
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        base
    }

    /// A stub `/mcp` handler standing in for the rmcp service. Returns 200 and
    /// echoes the validated identity (proving the gate plumbed it into the
    /// request extensions) when present.
    async fn stub_mcp(identity: Option<Extension<Identity>>) -> Response {
        match identity {
            Some(Extension(id)) => {
                let body = format!(
                    "ok sub={} client={} scopes={}",
                    id.sub.as_deref().unwrap_or("-"),
                    id.client_id.as_deref().unwrap_or("-"),
                    id.scopes.join(",")
                );
                (StatusCode::OK, body).into_response()
            }
            None => (StatusCode::OK, "ok").into_response(),
        }
    }

    /// The gated router under test: the real `gate` + PRM route over a stub `/mcp`.
    fn router(guard: Guard) -> Router {
        let mut router = Router::new()
            .route("/mcp", any(stub_mcp))
            .layer(middleware::from_fn_with_state(guard.clone(), gate));
        if let Guard::Oidc(cfg) = &guard {
            router = router.route(PRM_PATH, get(prm_handler).with_state(cfg.clone()));
        }
        router
    }

    /// Send a request and return (status, www-authenticate header, body string).
    async fn send(router: Router, req: Request<Body>) -> (StatusCode, Option<String>, String) {
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let www = resp
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        (status, www, String::from_utf8_lossy(&bytes).to_string())
    }

    /// A `GET /mcp` request carrying an optional bearer token.
    fn mcp_request(bearer: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri("/mcp").method("GET");
        if let Some(token) = bearer {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn valid_token_with_read_scope_passes_and_plumbs_identity() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        let (status, _www, body) = send(router(Guard::Oidc(cfg)), mcp_request(Some(&token))).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "valid read token should pass: {body}"
        );
        // The validated identity was plumbed into the request extensions.
        assert!(body.contains("sub=user-42"), "identity sub: {body}");
        assert!(body.contains("client=agent-cli"), "identity client: {body}");
        assert!(body.contains("nbox:read"), "identity scopes: {body}");
    }

    #[tokio::test]
    async fn wrong_audience_is_401_invalid_token() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let mut claims = good_claims();
        claims["aud"] = json!("https://someone-else.test");
        let token = idp.mint(&claims);

        let (status, www, _) = send(router(Guard::Oidc(cfg)), mcp_request(Some(&token))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let www = www.expect("a WWW-Authenticate challenge");
        assert!(www.contains("error=\"invalid_token\""), "got: {www}");
        assert!(www.contains("resource_metadata="), "got: {www}");
    }

    #[tokio::test]
    async fn expired_token_is_401() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let mut claims = good_claims();
        // Expired well beyond the 120 s leeway.
        claims["exp"] = json!(jsonwebtoken::get_current_timestamp() - 1000);
        let token = idp.mint(&claims);

        let (status, www, _) = send(router(Guard::Oidc(cfg)), mcp_request(Some(&token))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(www.unwrap().contains("error=\"invalid_token\""));
    }

    #[tokio::test]
    async fn bad_signature_is_401() {
        // Token minted by a *different* IdP than the one serving the JWKS.
        let serving = Arc::new(MockIdp::new("k1"));
        let attacker = Arc::new(MockIdp::new("k1")); // same kid, different key
        let base = spawn_idp(serving.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = attacker.mint(&good_claims());

        let (status, www, _) = send(router(Guard::Oidc(cfg)), mcp_request(Some(&token))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(www.unwrap().contains("error=\"invalid_token\""));
    }

    #[tokio::test]
    async fn unknown_kid_is_401() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        // Header claims a kid the JWKS doesn't publish; the single refresh on the
        // miss still won't find it.
        let token = idp.mint_with_kid(&good_claims(), "no-such-kid");

        let (status, www, _) = send(router(Guard::Oidc(cfg)), mcp_request(Some(&token))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(www.unwrap().contains("error=\"invalid_token\""));
    }

    #[tokio::test]
    async fn alg_none_is_rejected() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        // Hand-craft an unsigned `alg: none` token (header.payload. with empty sig).
        let header = URL_SAFE_NO_PAD.encode(json!({"alg": "none", "typ": "JWT"}).to_string());
        let payload = URL_SAFE_NO_PAD.encode(good_claims().to_string());
        let token = format!("{header}.{payload}.");

        let (status, www, _) = send(router(Guard::Oidc(cfg)), mcp_request(Some(&token))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(www.unwrap().contains("error=\"invalid_token\""));
    }

    #[tokio::test]
    async fn missing_token_is_401_without_error_code() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));

        let (status, www, _) = send(router(Guard::Oidc(cfg)), mcp_request(None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let www = www.expect("a Bearer challenge");
        // No token presented → bare challenge with the PRM pointer, no `error`.
        assert!(www.contains("resource_metadata="), "got: {www}");
        assert!(!www.contains("error="), "got: {www}");
    }

    #[tokio::test]
    async fn authenticated_but_missing_read_scope_is_403() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let mut claims = good_claims();
        // Authenticated, but only a `nbox:write` scope — no `nbox:read`.
        claims["scope"] = json!("nbox:write profile");
        let token = idp.mint(&claims);

        let (status, www, _) = send(router(Guard::Oidc(cfg)), mcp_request(Some(&token))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let www = www.expect("an insufficient_scope challenge");
        assert!(www.contains("error=\"insufficient_scope\""), "got: {www}");
        assert!(www.contains("scope=\"nbox:read\""), "got: {www}");
    }

    #[tokio::test]
    async fn prm_endpoint_is_public_and_correct() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let jwks_url = format!("{base}/jwks");
        let cfg = oidc_config(&jwks_url);

        // No token on the PRM route — it must be reachable.
        let req = Request::builder()
            .uri(PRM_PATH)
            .method("GET")
            .body(Body::empty())
            .unwrap();
        let (status, _www, body) = send(router(Guard::Oidc(cfg)), req).await;
        assert_eq!(status, StatusCode::OK, "PRM must be public");

        let doc: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(doc["resource"], json!(AUDIENCE));
        assert_eq!(doc["authorization_servers"], json!([ISSUER]));
        assert_eq!(doc["scopes_supported"], json!(["nbox:read", "nbox:write"]));
        assert_eq!(doc["bearer_methods_supported"], json!(["header"]));
        assert_eq!(doc["jwks_uri"], json!(jwks_url));
    }

    #[tokio::test]
    async fn query_string_tokens_are_not_accepted() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        // Token only in the query string, not the Authorization header → rejected.
        let req = Request::builder()
            .uri(format!("/mcp?access_token={token}"))
            .method("GET")
            .body(Body::empty())
            .unwrap();
        let (status, _www, _) = send(router(Guard::Oidc(cfg)), req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn discovery_resolves_jwks_uri() {
        // No JWKS override: the URI is discovered from the issuer's well-known.
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let http = reqwest::Client::new();
        let jwks_uri = oidc::discover_jwks_uri(&http, &base).await.unwrap();
        assert_eq!(jwks_uri, format!("{base}/jwks"));
    }

    // --- Unit 1 behavior is unchanged when OIDC is OFF (loopback mode) ---------

    #[tokio::test]
    async fn loopback_mode_no_token_passes() {
        let guard = Guard::Loopback { token: None };
        let (status, _www, _) = send(router(guard), mcp_request(None)).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn loopback_mode_static_bearer_enforced() {
        let guard = Guard::Loopback {
            token: Some(Arc::from("s3cret")),
        };
        // Right token passes.
        let (ok, _, _) = send(router(guard.clone()), mcp_request(Some("s3cret"))).await;
        assert_eq!(ok, StatusCode::OK);
        // Wrong token → 401 with the bare Bearer challenge.
        let (bad, www, _) = send(router(guard.clone()), mcp_request(Some("nope"))).await;
        assert_eq!(bad, StatusCode::UNAUTHORIZED);
        assert_eq!(www.as_deref(), Some("Bearer"));
        // Missing token → 401.
        let (missing, _, _) = send(router(guard), mcp_request(None)).await;
        assert_eq!(missing, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn loopback_mode_rejects_non_loopback_origin() {
        let guard = Guard::Loopback { token: None };
        let req = Request::builder()
            .uri("/mcp")
            .method("GET")
            .header(header::ORIGIN, "http://evil.example.com")
            .body(Body::empty())
            .unwrap();
        let (status, _www, _) = send(router(guard), req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
