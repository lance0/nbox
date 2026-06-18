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
//! - DNS-rebinding defense via an allowed-host set. In loopback mode it is the
//!   strict loopback set (`localhost`, `127.0.0.1`, `::1`); in OIDC/routable mode
//!   it is the `--audience` host (nbox's own identity) plus loopback plus any
//!   `--allowed-host` the operator adds. rmcp validates the `Host` header against
//!   this set, and our gate validates the `Origin` header (when present) against
//!   the same set — in both modes.
//! - The IdP issuer / JWKS / discovered endpoints must be HTTPS unless their host
//!   is loopback (local dev); a plain-`http://` non-loopback IdP URL is a startup
//!   usage error (no fetching signing keys over plaintext).
//! - In OIDC mode, JWT validation on `/mcp`: alg allowlist, `iss`/`aud`/`exp`,
//!   `nbox:read` scope; 401/403 with the right `WWW-Authenticate` challenge.
//! - Advertise `MCP-Protocol-Version: 2025-11-25` on *every* response — including
//!   the 401/403 challenge and 429 rate-limit paths.
//! - stdout stays clean: the protocol travels over the HTTP body, and all logs
//!   go to stderr/file exactly as the stdio path does. Nothing here writes to
//!   stdout. The token is never logged; the raw `Mcp-Session-Id` is hashed.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
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
use super::audit::{self, AuditEvent, AuthMode, Outcome, RateDecision, RateLimiter};
use super::oidc::{
    self, AuthError, Identity, JwksCache, OidcConfig, SCOPE_READ, SCOPE_WRITE,
    require_https_or_loopback,
};
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

impl Guard {
    /// The [`AuthMode`] to record for a request that is rejected *before* auth
    /// resolves (the pre-auth peer-IP 429). For a loopback guard that's the mode
    /// the request would have taken — `StaticBearer` if a token is configured,
    /// else `Loopback`; for OIDC it's `Oidc`.
    fn unauth_mode(&self) -> AuthMode {
        match self {
            Guard::Loopback { token: Some(_) } => AuthMode::StaticBearer,
            Guard::Loopback { token: None } => AuthMode::Loopback,
            Guard::Oidc(_) => AuthMode::Oidc,
        }
    }
}

/// The allowed-host set for the DNS-rebinding defense, shared by rmcp's `Host`
/// check and our `Origin` check so the two never diverge.
///
/// A request whose `Host`/`Origin` host is loopback always passes (loopback is
/// trusted regardless of mode); otherwise its authority must match an `extra`
/// entry. In loopback mode `extra` is empty, so only loopback passes (the strict
/// default). In OIDC/routable mode `extra` carries the `--audience` authority plus
/// any operator `--allowed-host` entries.
///
/// Port handling (matching rmcp's own `Host` matcher): an entry with an explicit
/// port matches only that `host:port`; an entry with no port matches the host on
/// any port. So an operator who pins `nbox.example.com:8443` rejects the same host
/// on a different port, while a bare `nbox.example.com` keeps any-port matching.
#[derive(Clone, Debug, Default)]
struct AllowedHosts {
    /// Non-loopback authorities that are allowed (lowercased; the port is kept
    /// only when the operator/audience specified one explicitly).
    extra: Vec<Authority>,
}

impl AllowedHosts {
    /// Whether the request authority `auth` (host, plus a port when the request
    /// carried one) is allowed. A loopback host is always allowed on any port;
    /// anything else must match an `extra` entry by the port rule above.
    fn allows(&self, auth: &Authority) -> bool {
        if host_is_loopback(&auth.host) {
            return true;
        }
        self.extra.iter().any(|allowed| allowed.matches(auth))
    }

    /// The list rmcp's `with_allowed_hosts` wants: the loopback names plus the
    /// `extra` entries rendered back to `host` / `host:port` strings. rmcp's
    /// matcher applies the same port rule we do, so the two checks agree. Empty
    /// `extra` ⇒ exactly rmcp's strict loopback default.
    fn for_rmcp(&self) -> Vec<String> {
        let mut hosts: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
        hosts.extend(self.extra.iter().map(Authority::to_rmcp_string));
        hosts
    }
}

/// A normalized host authority: a lowercased host plus an optional explicit port.
/// `port: None` means "any port" when used as an allow-list entry, and "the
/// request named no port" when it describes an incoming request.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Authority {
    /// The lowercased host (a hostname or an IP literal, brackets stripped).
    host: String,
    /// The explicit port, if one was present.
    port: Option<u16>,
}

impl Authority {
    /// Whether this allow-list entry matches an incoming request authority. A
    /// ported entry matches only the same `host:port`; a port-less entry matches
    /// the host on any port (mirrors rmcp's `host_is_allowed`).
    fn matches(&self, req: &Authority) -> bool {
        self.host == req.host && (self.port.is_none() || self.port == req.port)
    }

    /// Render back to the `host` / `host:port` form rmcp's `with_allowed_hosts`
    /// expects. An IPv6 host is bracketed when a port is attached so the authority
    /// stays parseable (`[::1]:8443`).
    fn to_rmcp_string(&self) -> String {
        match self.port {
            Some(port) if self.host.contains(':') => format!("[{}]:{port}", self.host),
            Some(port) => format!("{}:{port}", self.host),
            None => self.host.clone(),
        }
    }
}

/// The full state the `/mcp` gate middleware rides on: the auth [`Guard`], the
/// [`AllowedHosts`] set for the `Origin` check, plus the optional per-caller
/// [`RateLimiter`] (`None` ⇒ rate limiting disabled). Cheap to clone.
#[derive(Clone)]
struct GateState {
    guard: Guard,
    allowed_hosts: Arc<AllowedHosts>,
    rate_limiter: Option<Arc<RateLimiter>>,
}

/// Operational inputs for [`serve_http`], grouped so the call site stays one
/// argument rather than a widening parameter list. The auth `token`/`oidc` and
/// the `rate_limit` are all resolved (flag-over-config) before this is built.
pub struct ServeOptions {
    /// Optional static bearer for loopback mode (ignored in OIDC mode).
    pub token: Option<String>,
    /// OIDC resource-server inputs; `Some` ⇒ rung 3 (validate IdP JWTs).
    pub oidc: Option<OidcArgs>,
    /// Extra hostnames to add to the DNS-rebinding allow-list, on top of the
    /// `--audience` host and loopback. Only honored in OIDC/routable mode (a
    /// loopback bind keeps the strict loopback-only default). Repeatable
    /// (`--allowed-host` / `[serve].allowed_hosts`).
    pub allowed_hosts: Vec<String>,
    /// Per-caller requests-per-minute cap; `0` ⇒ disabled (default).
    pub rate_limit: u32,
}

/// Serve the read-only MCP server over HTTP until interrupted.
///
/// Without `opts.oidc`, `addr` must be a loopback socket address (rung 2): the
/// trust boundary is loopback, plus the optional static `opts.token`. With
/// `opts.oidc`, JWTs are validated on `/mcp` (rung 3) and `addr` may be routable —
/// TLS must terminate in front (a reverse proxy). `opts.rate_limit` (>0) caps
/// requests per caller per minute. The same [`NboxMcp`] backs every request.
/// stdout is never written; the token is never logged.
pub async fn serve_http(client: NetBoxClient, addr: &str, opts: ServeOptions) -> Result<()> {
    let ServeOptions {
        token,
        oidc,
        allowed_hosts: extra_hosts,
        rate_limit,
    } = opts;
    let oidc_on = oidc.is_some();
    let socket = parse_bind_addr(addr, oidc_on)?;
    if oidc_on && !is_loopback(socket.ip()) {
        tracing::warn!(
            %socket,
            "binding a non-loopback address — terminate TLS in front (reverse proxy); \
             nbox serves plain HTTP and validates inbound IdP JWTs but does not do TLS"
        );
    }

    // Build the allowed-host set for the DNS-rebinding defense. Loopback mode is
    // strict (loopback-only — operator `--allowed-host` is ignored there, by
    // design). OIDC mode adds the `--audience` host (nbox's own identity) plus any
    // `--allowed-host` entries, so a real proxied request with the deployment's
    // `Host` passes both rmcp's check and our `Origin` check.
    let allowed_hosts = if oidc_on {
        let mut extra: Vec<Authority> = Vec::new();
        if let Some(args) = &oidc
            && let Some(authority) = host_of_url(&args.audience)
        {
            extra.push(authority);
        }
        extra.extend(extra_hosts.iter().filter_map(|h| normalize_host_entry(h)));
        extra.sort();
        extra.dedup();
        if !extra.is_empty() {
            let rendered: Vec<String> = extra.iter().map(Authority::to_rmcp_string).collect();
            tracing::info!(allowed_hosts = ?rendered, "DNS-rebinding allow-list (plus loopback)");
        }
        AllowedHosts { extra }
    } else {
        if !extra_hosts.is_empty() {
            tracing::warn!(
                "--allowed-host is ignored in loopback mode (the allow-list stays loopback-only); \
                 it applies only with --oidc-issuer"
            );
        }
        AllowedHosts::default()
    };

    // Build the OIDC config (discovering the JWKS URI if no override was given)
    // before we stand up the listener, so a misconfiguration fails at startup.
    let guard = match oidc {
        Some(args) => Guard::Oidc(build_oidc_config(args).await?),
        None => Guard::Loopback {
            token: token.map(|t| Arc::from(t.as_str())),
        },
    };
    // `None` ⇒ rate limiting disabled (the hot path then skips the limiter).
    let rate_limiter = RateLimiter::new(rate_limit).map(Arc::new);
    if rate_limiter.is_some() {
        tracing::info!(per_minute = rate_limit, "per-caller rate limit enabled");
    }
    let allowed_hosts = Arc::new(allowed_hosts);
    let state = GateState {
        guard: guard.clone(),
        allowed_hosts: allowed_hosts.clone(),
        rate_limiter,
    };

    // Build the server once; the service factory hands rmcp a fresh clone per
    // session (cheap — `NboxMcp` holds an `Arc<NetBoxClient>`).
    let server = NboxMcp::new(client);

    let cancel = CancellationToken::new();
    // `StreamableHttpServerConfig` is `#[non_exhaustive]`, so build from the
    // default and adjust via the builder. rmcp validates the `Host` header
    // against `allowed_hosts` (DNS-rebinding defense); we hand it the same set
    // our `Origin` check uses. In loopback mode that is exactly rmcp's strict
    // loopback default; in OIDC mode it additionally allows the `--audience` host
    // (+ operator extras) so a legitimate proxied request is not 403'd. Origin
    // validation is enforced by our own middleware (full control over the 403
    // body) consistently with the Host set.
    let config = StreamableHttpServerConfig::default()
        .with_cancellation_token(cancel.child_token())
        .with_allowed_hosts(allowed_hosts.for_rmcp());
    let mcp = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        config,
    );

    // The PRM well-known route is public (no auth, no gate) and only meaningful
    // in OIDC mode; mount it there. `/mcp` carries the gate (bearer/JWT + Origin
    // + audit + rate limit + protocol-version header); `/.well-known/*` is public.
    let mut router = axum::Router::new()
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(state, gate));
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
    // `into_make_service_with_connect_info` surfaces the peer `SocketAddr` to the
    // gate (via `ConnectInfo`) so a loopback / static-bearer caller — which has
    // no token identity — is still attributable by IP in the audit log + limiter.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .context("HTTP server error")?;
    Ok(())
}

/// Turn resolved [`OidcArgs`] into a live [`OidcConfig`]: discover the JWKS URI
/// from the issuer if no override was given, then build the by-`kid` cache. The
/// JWKS itself is fetched lazily on the first token validation.
async fn build_oidc_config(args: OidcArgs) -> Result<OidcConfig> {
    // HTTPS is mandatory for the IdP issuer (and any JWKS URL) unless the host is
    // loopback — a plain-`http://` non-loopback IdP lets a network attacker swap
    // the signing keys. Fail fast at startup (exit 2) before any fetch.
    require_https_or_loopback(&args.issuer, "issuer")?;
    if let Some(url) = &args.jwks_url {
        require_https_or_loopback(url, "JWKS URL")?;
    }

    // A dedicated client for IdP calls (discovery + JWKS). Same reqwest/rustls
    // style as the NetBox client; default TLS verification (an IdP is public).
    // Its redirect policy re-checks https-or-loopback on EVERY hop, so a 30x to a
    // plain-http non-loopback URL can't downgrade the transport (see oidc.rs).
    let http = oidc::build_oidc_http_client()?;

    let jwks_uri = match args.jwks_url {
        Some(url) => url,
        // `discover_jwks_uri` applies the same https-or-loopback rule to the URL
        // the discovery document returns.
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

/// Whether `host` (a bare hostname or IP literal, no port) is loopback: any
/// `127.0.0.0/8` / `::1` literal, or the name `localhost`.
fn host_is_loopback(host: &str) -> bool {
    match host.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

/// Extract the [`Authority`] (lowercased host + explicit port if any) from a URL
/// like the `--audience` canonical URI. `None` if it isn't an absolute `http(s)`
/// URL with a host (e.g. an opaque audience identifier — no host to allow). The
/// explicit port is kept so an audience of `https://nbox.example.com:8443` pins
/// the allow-list to that port.
fn host_of_url(url: &str) -> Option<Authority> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Drop any userinfo.
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
    parse_authority(host_port)
}

/// Normalize an operator-supplied `--allowed-host` entry to an [`Authority`]
/// (tolerating a `scheme://` prefix). An explicit `:port` is preserved so the
/// entry matches only that port; without one the host matches any port. `None`
/// for an entry with no host.
fn normalize_host_entry(entry: &str) -> Option<Authority> {
    let entry = entry.trim();
    let rest = entry.split_once("://").map_or(entry, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
    parse_authority(host_port)
}

/// Parse an authority (`host`, `host:port`, `[v6]`, or `[v6]:port`) into an
/// [`Authority`]: the lowercased host plus the explicit port if a valid one is
/// present. A trailing `:` with a non-numeric / out-of-range port is treated as
/// no port (the whole authority is the host). `None` for an empty host.
fn parse_authority(authority: &str) -> Option<Authority> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        // IPv6 literal: `[::1]` or `[::1]:8080`.
        match rest.split_once(']') {
            Some((host, after)) => {
                let port = after.strip_prefix(':').and_then(|p| p.parse::<u16>().ok());
                (host, port)
            }
            None => (rest, None),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
                (host, port.parse::<u16>().ok())
            }
            _ => (authority, None),
        }
    };
    if host.is_empty() {
        None
    } else {
        Some(Authority {
            host: host.to_ascii_lowercase(),
            port,
        })
    }
}

/// Axum middleware on `/mcp`: enforce auth, validate the `Origin` header,
/// rate-limit per caller, advertise the MCP protocol version, and emit one
/// structured audit event for the request.
///
/// Loopback mode: optional static bearer (401) → origin (403) → rate (429) → inner.
/// OIDC mode: validate the JWT + scope (401/403) → origin (403) → rate (429) →
/// inner, and plumb the validated [`Identity`] into request extensions for
/// downstream. Every path fails closed.
///
/// This is a thin shell over [`gate_inner`]: it runs the gate, then *uniformly*
/// — on every path, including the 401/403 challenge and 429 rate-limit paths —
/// stamps `MCP-Protocol-Version` on the response and emits exactly one audit
/// event (target [`audit::AUDIT_TARGET`]) recording WHO/WHAT/WHEN/OUTCOME. The
/// token, the `Authorization` header, and the raw `Mcp-Session-Id` are never
/// logged (the session id is hashed).
async fn gate(State(state): State<GateState>, request: Request<Body>, next: Next) -> Response {
    let start = Instant::now();
    // WHAT/WHEN, captured up front (the request body is never read — request-line
    // fields only, so the rmcp stream is untouched). The JSON-RPC method / tool
    // name is *not* extracted: pulling it would mean buffering/cloning the body,
    // which would break the streaming transport — request-level WHAT is honest
    // and cheap (see docs/MCP.md).
    let method = request.method().as_str().to_string();
    let path = request.uri().path().to_string();
    let session = request
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(audit::session_hash);
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());
    let request_id = new_request_id();

    // Run the gate. `auth_mode` / `identity` are the resolved attribution (an auth
    // failure yields `None` identity); `response` is whatever the gate decided —
    // a challenge/forbidden/429, or the inner service's response.
    let GateOutcome {
        auth_mode,
        identity,
        mut response,
    } = gate_inner(&state, request, next).await;

    // Uniform tail, taken by EVERY path (success and every error/challenge):
    // stamp the protocol version, then emit the single audit event.
    response.headers_mut().insert(
        "mcp-protocol-version",
        header::HeaderValue::from_static(MCP_PROTOCOL_VERSION),
    );
    audit(
        &request_id,
        auth_mode,
        identity.as_ref(),
        peer,
        &method,
        &path,
        session.as_deref(),
        &response,
        start,
    );
    response
}

/// What [`gate_inner`] resolved: the attribution for the audit event plus the
/// response to return. Keeping the attribution out of the response lets the
/// [`gate`] tail audit + stamp the protocol header uniformly on every path.
struct GateOutcome {
    auth_mode: AuthMode,
    identity: Option<Identity>,
    response: Response,
}

/// The gate's decision logic: auth → origin → rate limit → inner service. Returns
/// the response *and* the attribution so the caller can apply the protocol header
/// and audit on a single, uniform path (no early return skips either).
async fn gate_inner(state: &GateState, mut request: Request<Body>, next: Next) -> GateOutcome {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());

    // 0) Pre-auth peer-IP rate limit (DNS-rebinding-independent flood control).
    //    Runs BEFORE auth so a flood of missing/invalid-bearer requests from one
    //    peer is throttled too — otherwise the 401/403 paths return before any
    //    limit and an attacker can hammer JWT validation unthrottled. Keyed on the
    //    peer IP (`ip:<addr>`), so it's a coarse per-peer cap; an authenticated
    //    request additionally honors its per-caller (`sub`/`client`) bucket below.
    //    To avoid double-charging the SAME logical bucket, the post-auth check is
    //    skipped when the resolved caller key is this same `ip:` key (loopback /
    //    static-bearer callers have no token identity, so both keys coincide).
    //    Disabled (`--rate-limit 0` / absent) ⇒ no pre-auth limiting either.
    let peer_key = audit::caller_key(None, peer);
    if let Some(rl) = &state.rate_limiter
        && let RateDecision::Limited { retry_after_secs } = rl.check(&peer_key)
    {
        // The request hasn't authenticated yet; attribute the 429 to the mode the
        // guard would use and no identity (no secret to leak).
        return GateOutcome {
            auth_mode: state.guard.unauth_mode(),
            identity: None,
            response: too_many_requests(retry_after_secs),
        };
    }

    // 1) Authenticate by mode. The auth check runs first; only an authenticated
    //    (or auth-not-required) request proceeds. An auth failure short-circuits
    //    with the challenge response and no identity.
    let (auth_mode, identity) = match &state.guard {
        Guard::Loopback { token } => {
            if let Some(expected) = token.as_deref() {
                let presented = request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "));
                let ok = presented.is_some_and(|got| ct_eq(got.as_bytes(), expected.as_bytes()));
                if !ok {
                    return GateOutcome {
                        auth_mode: AuthMode::StaticBearer,
                        identity: None,
                        response: loopback_unauthorized(),
                    };
                }
                (AuthMode::StaticBearer, None)
            } else {
                (AuthMode::Loopback, None)
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
                        return GateOutcome {
                            auth_mode: AuthMode::Oidc,
                            identity: Some(identity),
                            response: oidc_challenge(cfg, &scope_err),
                        };
                    }
                    (AuthMode::Oidc, Some(identity))
                }
                Err(e) => {
                    return GateOutcome {
                        auth_mode: AuthMode::Oidc,
                        identity: None,
                        response: oidc_challenge(cfg, &e),
                    };
                }
            }
        }
    };

    // 2) Origin validation (DNS-rebinding defense), in BOTH modes. A request that
    //    carries an `Origin` header must have an allowed host — the SAME set
    //    rmcp's `Host` check uses. In loopback mode that is loopback-only; in
    //    OIDC mode it is the `--audience` host (+ loopback + operator extras). A
    //    request with no `Origin` (a non-browser API client) is unaffected — the
    //    bearer/Host checks are its boundary.
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let allowed = origin
            .to_str()
            .ok()
            .and_then(origin_host)
            .is_some_and(|host| state.allowed_hosts.allows(&host));
        if !allowed {
            return GateOutcome {
                auth_mode,
                identity,
                response: forbidden(),
            };
        }
    }

    // 3) Per-caller rate limit (keyed sub → client_id → peer IP). Disabled when no
    //    limiter is configured. Over the limit → 429 + Retry-After. When the
    //    resolved caller key is the same `ip:` key the pre-auth check already
    //    charged in step 0 (loopback / static-bearer callers, which have no token
    //    identity), skip it — that single logical request is not charged twice to
    //    the one bucket. An OIDC `sub`/`client` caller has a distinct bucket, so it
    //    honors both the coarse peer-IP cap and its own per-caller cap.
    let caller = audit::caller_key(identity.as_ref(), peer);
    if caller != peer_key
        && let Some(rl) = &state.rate_limiter
        && let RateDecision::Limited { retry_after_secs } = rl.check(&caller)
    {
        return GateOutcome {
            auth_mode,
            identity,
            response: too_many_requests(retry_after_secs),
        };
    }

    // Plumb the validated identity through for the NetBox bridge (v2). rmcp
    // doesn't surface caller identity, so request extensions are the handoff.
    if let Some(id) = &identity {
        request.extensions_mut().insert(id.clone());
    }

    let response = next.run(request).await;
    GateOutcome {
        auth_mode,
        identity,
        response,
    }
}

/// A short random request id for correlating the audit event with the response.
/// Not security-sensitive — just enough entropy to disambiguate concurrent
/// requests in a log. Derived from the current time's nanos + a process-local
/// counter so it needs no extra dependency.
fn new_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("{nanos:08x}{n:08x}")
}

/// Build and emit the one audit event for a request, given the final response.
/// Pulls the safe identity fields (never the token), classifies the outcome from
/// the status, and records the latency. Centralized so every return path in
/// [`gate`] logs identically.
#[allow(clippy::too_many_arguments)]
fn audit(
    request_id: &str,
    auth: AuthMode,
    identity: Option<&Identity>,
    peer: Option<IpAddr>,
    method: &str,
    path: &str,
    session: Option<&str>,
    response: &Response,
    start: Instant,
) {
    let status = response.status().as_u16();
    let caller = audit::caller_key(identity, peer);
    // Space-join the scopes for a compact, greppable field; `None` when empty.
    let scope = identity.and_then(|id| {
        if id.scopes.is_empty() {
            None
        } else {
            Some(id.scopes.join(" "))
        }
    });
    AuditEvent {
        request_id,
        auth,
        caller: &caller,
        sub: identity.and_then(|id| id.sub.as_deref()),
        client_id: identity.and_then(|id| id.client_id.as_deref()),
        scope: scope.as_deref(),
        jti: identity.and_then(|id| id.jti.as_deref()),
        iss: identity.and_then(|id| id.iss.as_deref()),
        method,
        path,
        session,
        status,
        outcome: Outcome::from_status(status),
        latency_ms: start.elapsed().as_millis(),
    }
    .emit();
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

/// Extract the [`Authority`] (lowercased host + explicit port if any) from an
/// RFC 6454 origin string (e.g. `http://127.0.0.1:8080` → host `127.0.0.1`, port
/// `8080`). The scheme is not constrained — only the host/port matter for the
/// DNS-rebinding decision the caller makes against the allowed-host set. The port
/// is kept so a port-pinned allow-list entry can reject a mismatched port.
/// Returns `None` for a malformed origin or `Origin: null` (no host ⇒ not
/// allowable).
fn origin_host(origin: &str) -> Option<Authority> {
    let origin = origin.trim();
    if origin.is_empty() || origin.eq_ignore_ascii_case("null") {
        return None;
    }
    let after_scheme = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    parse_authority(authority)
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

/// 429 for a caller over its per-minute rate limit, with a `Retry-After` (in
/// seconds, RFC 9110). The body is generic — it names no caller and no secret.
fn too_many_requests(retry_after_secs: u64) -> Response {
    let mut response = Response::new(Body::from("Too Many Requests"));
    *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
    if let Ok(value) = header::HeaderValue::from_str(&retry_after_secs.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
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

    /// A port-less [`Authority`] (host matches on any port as an allow-list entry).
    fn host(h: &str) -> Authority {
        Authority {
            host: h.to_ascii_lowercase(),
            port: None,
        }
    }

    /// A ported [`Authority`].
    fn host_port(h: &str, port: u16) -> Authority {
        Authority {
            host: h.to_ascii_lowercase(),
            port: Some(port),
        }
    }

    #[test]
    fn origin_host_extracts_host_and_port() {
        assert_eq!(
            origin_host("http://127.0.0.1:8080"),
            Some(host_port("127.0.0.1", 8080))
        );
        assert_eq!(
            origin_host("http://localhost:8080"),
            Some(host_port("localhost", 8080))
        );
        // No explicit port → port-less authority.
        assert_eq!(origin_host("https://localhost"), Some(host("localhost")));
        assert_eq!(
            origin_host("http://[::1]:8080"),
            Some(host_port("::1", 8080))
        );
        assert_eq!(origin_host("http://127.0.0.5"), Some(host("127.0.0.5")));
        // Case-folded host.
        assert_eq!(
            origin_host("https://NBOX.Example.COM"),
            Some(host("nbox.example.com"))
        );
        // `Origin: null` and garbage have no host.
        assert_eq!(origin_host("null"), None);
        assert_eq!(origin_host(""), None);
    }

    #[test]
    fn loopback_allowed_hosts_is_strict_loopback_only() {
        // The default (loopback-mode) set: loopback always passes; nothing else.
        let allowed = AllowedHosts::default();
        assert!(allowed.allows(&host("127.0.0.1")));
        assert!(allowed.allows(&host("127.0.0.5")));
        assert!(allowed.allows(&host("::1")));
        assert!(allowed.allows(&host("localhost")));
        // Loopback passes on any port too.
        assert!(allowed.allows(&host_port("127.0.0.1", 9999)));
        assert!(!allowed.allows(&host("evil.example.com")));
        assert!(!allowed.allows(&host("192.168.1.10")));
        assert!(!allowed.allows(&host("nbox.example.com")));
        // rmcp gets exactly the strict loopback default (no extras).
        assert_eq!(allowed.for_rmcp(), vec!["localhost", "127.0.0.1", "::1"]);
    }

    #[test]
    fn oidc_allowed_hosts_adds_the_audience_host_plus_loopback() {
        // OIDC mode: audience host + an operator extra, with loopback still free.
        let allowed = AllowedHosts {
            extra: vec![host("nbox.example.com"), host("alt.example.com")],
        };
        assert!(allowed.allows(&host("nbox.example.com")));
        assert!(allowed.allows(&host("alt.example.com")));
        // A port-less entry matches the host on ANY port.
        assert!(allowed.allows(&host_port("nbox.example.com", 8443)));
        assert!(allowed.allows(&host_port("nbox.example.com", 443)));
        // Case-insensitive match.
        assert!(allowed.allows(&host("NBOX.example.com")));
        // Loopback is always allowed regardless of mode.
        assert!(allowed.allows(&host("127.0.0.1")));
        assert!(allowed.allows(&host("localhost")));
        // A mismatched host is still rejected.
        assert!(!allowed.allows(&host("attacker.test")));
        // rmcp gets loopback + the extras.
        assert_eq!(
            allowed.for_rmcp(),
            vec![
                "localhost",
                "127.0.0.1",
                "::1",
                "nbox.example.com",
                "alt.example.com"
            ]
        );
    }

    #[test]
    fn explicit_port_entry_matches_only_that_port() {
        // An allow-list entry with an explicit port pins to that `host:port`: the
        // same host on a different port (or with no port) is rejected.
        let allowed = AllowedHosts {
            extra: vec![host_port("nbox.example.com", 8443)],
        };
        // Exact host:port matches.
        assert!(allowed.allows(&host_port("nbox.example.com", 8443)));
        // A different port is rejected.
        assert!(!allowed.allows(&host_port("nbox.example.com", 443)));
        assert!(!allowed.allows(&host_port("nbox.example.com", 8080)));
        // No port on the request is rejected too (the entry demands the port).
        assert!(!allowed.allows(&host("nbox.example.com")));
        // rmcp gets the authority rendered back with its port.
        assert_eq!(
            allowed.for_rmcp(),
            vec!["localhost", "127.0.0.1", "::1", "nbox.example.com:8443"]
        );
    }

    #[test]
    fn no_port_entry_matches_any_port() {
        // A port-less entry keeps host-only (any-port) matching — the prior default.
        let allowed = AllowedHosts {
            extra: vec![host("nbox.example.com")],
        };
        assert!(allowed.allows(&host("nbox.example.com")));
        assert!(allowed.allows(&host_port("nbox.example.com", 8443)));
        assert!(allowed.allows(&host_port("nbox.example.com", 1)));
        // Still rejects a different host.
        assert!(!allowed.allows(&host("other.example.com")));
    }

    #[test]
    fn explicit_port_entry_for_ipv6_round_trips_for_rmcp() {
        // An IPv6 host with an explicit port is bracketed when rendered for rmcp.
        let allowed = AllowedHosts {
            extra: vec![host_port("2001:db8::1", 8443)],
        };
        assert!(allowed.allows(&host_port("2001:db8::1", 8443)));
        assert!(!allowed.allows(&host_port("2001:db8::1", 443)));
        assert_eq!(
            allowed.for_rmcp(),
            vec!["localhost", "127.0.0.1", "::1", "[2001:db8::1]:8443"]
        );
    }

    #[test]
    fn host_of_url_and_normalize_host_entry() {
        // No port in the URL → a port-less authority (matches any port).
        assert_eq!(
            host_of_url("https://nbox.example.com"),
            Some(host("nbox.example.com"))
        );
        // An explicit port in the audience is PRESERVED (pins to that port).
        assert_eq!(
            host_of_url("https://nbox.example.com:8443/mcp"),
            Some(host_port("nbox.example.com", 8443))
        );
        assert_eq!(
            host_of_url("https://[2001:db8::1]:8080"),
            Some(host_port("2001:db8::1", 8080))
        );
        // An opaque (non-URL) audience has no host.
        assert_eq!(host_of_url("nbox"), None);

        // `--allowed-host` entries tolerate a scheme, and preserve an explicit port.
        assert_eq!(
            normalize_host_entry("nbox.example.com"),
            Some(host("nbox.example.com"))
        );
        assert_eq!(
            normalize_host_entry("https://nbox.example.com:8443"),
            Some(host_port("nbox.example.com", 8443))
        );
        assert_eq!(
            normalize_host_entry("  HOST.example.com  "),
            Some(host("host.example.com"))
        );
        // A bare `:port` with no host is rejected.
        assert_eq!(normalize_host_entry(""), None);
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
    /// Rate limiting off (the default); see [`router_rl`] for the limited variant.
    /// The allowed-host set is loopback-only (the strict default), matching the
    /// gate's loopback mode; [`router_hosts`] supplies a custom set.
    fn router(guard: Guard) -> Router {
        router_full(guard, None, AllowedHosts::default())
    }

    /// As [`router`], but with `per_minute` requests-per-caller rate limiting on.
    fn router_rl(guard: Guard, per_minute: u32) -> Router {
        router_full(
            guard,
            RateLimiter::new(per_minute).map(Arc::new),
            AllowedHosts::default(),
        )
    }

    /// As [`router`], but with a custom allowed-host set (for the Origin/Host
    /// DNS-rebinding tests in OIDC/routable mode).
    fn router_hosts(guard: Guard, allowed: AllowedHosts) -> Router {
        router_full(guard, None, allowed)
    }

    /// A port-less allow-list authority (matches the host on any port).
    fn allowed_host(h: &str) -> Authority {
        Authority {
            host: h.to_ascii_lowercase(),
            port: None,
        }
    }

    /// A ported allow-list authority (matches only that `host:port`).
    fn allowed_host_port(h: &str, port: u16) -> Authority {
        Authority {
            host: h.to_ascii_lowercase(),
            port: Some(port),
        }
    }

    /// Build the gated router with an explicit rate limiter and allowed-host set.
    fn router_full(
        guard: Guard,
        rate_limiter: Option<Arc<RateLimiter>>,
        allowed_hosts: AllowedHosts,
    ) -> Router {
        let state = GateState {
            guard: guard.clone(),
            allowed_hosts: Arc::new(allowed_hosts),
            rate_limiter,
        };
        let mut router = Router::new()
            .route("/mcp", any(stub_mcp))
            .layer(middleware::from_fn_with_state(state, gate));
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

    // --- (b): https-or-loopback for the IdP issuer / JWKS at startup ----------

    /// Build the OIDC config and assert it failed with a `NboxError::Usage`
    /// (exit 2). A match, not `unwrap_err`, because `OidcConfig` isn't `Debug`.
    async fn assert_oidc_config_usage_error(args: OidcArgs) {
        match build_oidc_config(args).await {
            Ok(_) => panic!("expected a Usage error, got Ok"),
            Err(e) => assert_eq!(NboxError::exit_code_for(&e), 2, "{e:#}"),
        }
    }

    #[tokio::test]
    async fn http_issuer_non_loopback_is_a_startup_usage_error() {
        // A plain-http:// non-loopback issuer fails fast at startup, exit 2, with
        // no network fetch attempted.
        assert_oidc_config_usage_error(OidcArgs {
            issuer: "http://idp.example.com".to_string(),
            audience: "https://nbox.example.com".to_string(),
            jwks_url: None,
        })
        .await;
    }

    #[tokio::test]
    async fn http_jwks_url_non_loopback_is_a_startup_usage_error() {
        // The issuer is https, but the JWKS override is plain http:// — rejected.
        assert_oidc_config_usage_error(OidcArgs {
            issuer: "https://idp.example.com".to_string(),
            audience: "https://nbox.example.com".to_string(),
            jwks_url: Some("http://idp.example.com/keys".to_string()),
        })
        .await;
    }

    #[tokio::test]
    async fn http_loopback_issuer_and_jwks_are_allowed_for_dev() {
        // A loopback http:// issuer + JWKS override is accepted (local dev). The
        // JWKS itself is fetched lazily, so no server is needed here — the config
        // builds without a network call.
        let args = OidcArgs {
            issuer: "http://127.0.0.1:9000".to_string(),
            audience: "https://nbox.example.com".to_string(),
            jwks_url: Some("http://127.0.0.1:9000/keys".to_string()),
        };
        let cfg = build_oidc_config(args)
            .await
            .expect("loopback http:// is allowed");
        assert_eq!(cfg.jwks_uri.as_ref(), "http://127.0.0.1:9000/keys");
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

    #[tokio::test]
    async fn loopback_mode_accepts_loopback_origin() {
        // A loopback Origin still passes in loopback mode (the documented default).
        let guard = Guard::Loopback { token: None };
        let req = Request::builder()
            .uri("/mcp")
            .method("GET")
            .header(header::ORIGIN, "http://localhost:8080")
            .body(Body::empty())
            .unwrap();
        let (status, _www, _) = send(router(guard), req).await;
        assert_eq!(status, StatusCode::OK);
    }

    // --- (a)/(c): Host/Origin DNS-rebinding defense in OIDC/routable mode ------

    /// A `GET /mcp` request with a bearer and an explicit `Origin` header.
    fn mcp_request_origin(bearer: Option<&str>, origin: &str) -> Request<Body> {
        let mut builder = Request::builder()
            .uri("/mcp")
            .method("GET")
            .header(header::ORIGIN, origin);
        if let Some(token) = bearer {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn oidc_origin_matching_audience_host_passes() {
        // OIDC/routable mode: an Origin whose host is the configured allow-list
        // host (here AUDIENCE = https://nbox.test) is accepted.
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        let allowed = AllowedHosts {
            extra: vec![allowed_host("nbox.test")],
        };
        let app = router_hosts(Guard::Oidc(cfg), allowed);
        let (status, _www, body) =
            send(app, mcp_request_origin(Some(&token), "https://nbox.test")).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "matching Origin should pass: {body}"
        );
    }

    #[tokio::test]
    async fn oidc_origin_mismatched_host_is_403() {
        // An Origin whose host is NOT in the allow-list is rejected even with a
        // valid bearer — the DNS-rebinding defense applies in OIDC mode too.
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        let allowed = AllowedHosts {
            extra: vec![allowed_host("nbox.test")],
        };
        let app = router_hosts(Guard::Oidc(cfg), allowed);
        let (status, _www, _) = send(
            app,
            mcp_request_origin(Some(&token), "https://attacker.test"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn oidc_no_origin_header_passes() {
        // A non-browser API client sends no Origin — the bearer/Host checks are
        // its boundary, so it is not 403'd for the absent Origin.
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        let allowed = AllowedHosts {
            extra: vec![allowed_host("nbox.test")],
        };
        let app = router_hosts(Guard::Oidc(cfg), allowed);
        let (status, _www, _) = send(app, mcp_request(Some(&token))).await;
        assert_eq!(status, StatusCode::OK);
    }

    // --- Unit 3: structured audit log + per-caller rate limit -----------------

    use super::super::audit::AUDIT_TARGET;
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;
    use std::sync::Mutex as StdMutex;
    use tracing::field::{Field, Visit};
    use tracing::subscriber::DefaultGuard;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
    use tracing_subscriber::registry::Registry;

    /// One captured audit event: its fields flattened to `name → value` strings.
    #[derive(Clone, Default)]
    struct Captured {
        fields: std::collections::HashMap<String, String>,
    }

    impl Captured {
        fn get(&self, key: &str) -> Option<&str> {
            self.fields.get(key).map(String::as_str)
        }
    }

    /// A `tracing` layer that records every event on [`AUDIT_TARGET`] into a
    /// shared vec, so a test can assert on the emitted audit fields.
    struct CaptureLayer {
        events: Arc<StdMutex<Vec<Captured>>>,
    }

    /// A visitor that flattens an event's fields into the `Captured` map. Records
    /// the debug form so both string and numeric fields are comparable as text.
    struct FieldVisitor<'a>(&'a mut Captured);

    impl Visit for FieldVisitor<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0
                .fields
                .insert(field.name().to_string(), format!("{value:?}"));
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0
                .fields
                .insert(field.name().to_string(), value.to_string());
        }
        fn record_u64(&mut self, field: &Field, value: u64) {
            self.0
                .fields
                .insert(field.name().to_string(), value.to_string());
        }
        fn record_i64(&mut self, field: &Field, value: i64) {
            self.0
                .fields
                .insert(field.name().to_string(), value.to_string());
        }
    }

    impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: LayerContext<'_, S>) {
            if event.metadata().target() != AUDIT_TARGET {
                return;
            }
            let mut captured = Captured::default();
            event.record(&mut FieldVisitor(&mut captured));
            self.events.lock().unwrap().push(captured);
        }
    }

    /// Install a thread-scoped capturing subscriber. The returned guard restores
    /// the previous subscriber on drop; the vec collects audit events emitted
    /// while it's in scope (the `oneshot` future polls on this thread).
    fn capture() -> (Arc<StdMutex<Vec<Captured>>>, DefaultGuard) {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let layer = CaptureLayer {
            events: events.clone(),
        };
        let subscriber = Registry::default().with(layer);
        let guard = tracing::subscriber::set_default(subscriber);
        (events, guard)
    }

    /// A `GET /mcp` request with a peer `SocketAddr` injected as `ConnectInfo`,
    /// the way `into_make_service_with_connect_info` would at runtime — so the
    /// IP-keyed caller path (loopback / static-bearer) is exercised in tests.
    fn mcp_request_from(bearer: Option<&str>, peer: SocketAddr) -> Request<Body> {
        let mut req = mcp_request(bearer);
        req.extensions_mut().insert(ConnectInfo(peer));
        req
    }

    #[tokio::test]
    async fn audit_event_emitted_for_authenticated_oidc_request() {
        let (events, _guard) = capture();

        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        let (status, _www, _) = send(router(Guard::Oidc(cfg)), mcp_request(Some(&token))).await;
        assert_eq!(status, StatusCode::OK);

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1, "exactly one audit event per request");
        let e = &events[0];
        // WHO: the validated identity.
        assert_eq!(e.get("sub"), Some("user-42"));
        assert_eq!(e.get("client_id"), Some("agent-cli"));
        assert_eq!(e.get("jti"), Some("tok-1"));
        assert_eq!(e.get("iss"), Some(ISSUER));
        assert_eq!(e.get("auth"), Some("oidc"));
        assert_eq!(e.get("caller"), Some("sub:user-42"));
        assert!(e.get("scope").is_some_and(|s| s.contains("nbox:read")));
        // WHAT / WHEN / OUTCOME.
        assert_eq!(e.get("method"), Some("GET"));
        assert_eq!(e.get("path"), Some("/mcp"));
        assert_eq!(e.get("status"), Some("200"));
        assert_eq!(e.get("outcome"), Some("ok"));
        assert!(e.get("request_id").is_some_and(|r| !r.is_empty()));
        assert!(e.get("latency_ms").is_some());
    }

    #[tokio::test]
    async fn audit_event_never_contains_the_token_or_authorization() {
        let (events, _guard) = capture();

        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        let (status, _, _) = send(router(Guard::Oidc(cfg)), mcp_request(Some(&token))).await;
        assert_eq!(status, StatusCode::OK);

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        // No field value may contain the raw token or the word "Bearer".
        for (name, value) in &events[0].fields {
            assert!(
                !value.contains(&token),
                "audit field {name} leaked the token: {value}"
            );
            assert!(
                !value.contains("Bearer"),
                "audit field {name} leaked an Authorization scheme: {value}"
            );
        }
    }

    #[tokio::test]
    async fn audit_event_records_auth_failure() {
        let (events, _guard) = capture();

        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));

        // No token → 401, audited as auth-failed with no identity.
        let (status, _www, _) = send(router(Guard::Oidc(cfg)), mcp_request(None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.get("status"), Some("401"));
        assert_eq!(e.get("outcome"), Some("auth-failed"));
        assert_eq!(e.get("auth"), Some("oidc"));
        assert_eq!(e.get("sub"), None, "no identity on a failed auth");
    }

    #[tokio::test]
    async fn audit_event_for_loopback_records_mode_and_peer() {
        let (events, _guard) = capture();
        let peer: SocketAddr = "127.0.0.1:54321".parse().unwrap();

        let (status, _www, _) = send(
            router(Guard::Loopback { token: None }),
            mcp_request_from(None, peer),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        // No OIDC identity → auth mode is loopback, caller is the peer IP.
        assert_eq!(e.get("auth"), Some("loopback"));
        assert_eq!(e.get("caller"), Some("ip:127.0.0.1"));
        assert_eq!(e.get("sub"), None);
        assert_eq!(e.get("outcome"), Some("ok"));
    }

    #[tokio::test]
    async fn rate_limit_429s_after_n_for_same_caller() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        // Limit of 2 per caller. The router (and its limiter) persists across the
        // calls below; each clone shares the same Arc'd limiter.
        let app = router_rl(Guard::Oidc(cfg), 2);

        for i in 1..=2 {
            let resp = app
                .clone()
                .oneshot(mcp_request(Some(&token)))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "request {i} should pass");
        }
        // The 3rd from the same `sub` → 429 with Retry-After.
        let resp = app.oneshot(mcp_request(Some(&token))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry = resp
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .expect("a numeric Retry-After");
        assert!((1..=60).contains(&retry), "Retry-After: {retry}");
    }

    #[tokio::test]
    async fn rate_limit_isolates_distinct_callers() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));

        // Two distinct subjects → two distinct callers.
        let token_a = idp.mint(&good_claims());
        let mut claims_b = good_claims();
        claims_b["sub"] = json!("user-99");
        let token_b = idp.mint(&claims_b);

        let app = router_rl(Guard::Oidc(cfg), 1);

        // Distinct callers come from distinct peers (the realistic case), so the
        // coarse pre-auth peer-IP cap doesn't conflate them and the per-`sub`
        // isolation is what's exercised.
        let peer_a: SocketAddr = "203.0.113.10:5000".parse().unwrap();
        let peer_b: SocketAddr = "203.0.113.11:5000".parse().unwrap();

        // user-42 spends its one allowance, then is limited.
        let ok = app
            .clone()
            .oneshot(mcp_request_from(Some(&token_a), peer_a))
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let limited = app
            .clone()
            .oneshot(mcp_request_from(Some(&token_a), peer_a))
            .await
            .unwrap();
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        // user-99 (a different sub from a different peer) is unaffected.
        let other = app
            .oneshot(mcp_request_from(Some(&token_b), peer_b))
            .await
            .unwrap();
        assert_eq!(other.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rate_limit_disabled_never_429s() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        // Disabled (default): the router has no limiter.
        let app = router(Guard::Oidc(cfg));
        for _ in 0..10 {
            let resp = app
                .clone()
                .oneshot(mcp_request(Some(&token)))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn rate_limited_request_is_audited() {
        let (events, _guard) = capture();

        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        // Limit 2, but the SAME sub arrives from three distinct peers. Each peer's
        // coarse bucket stays under the limit (1 each), so the per-`sub` bucket is
        // the one that trips on the 3rd request — the 429 is attributed to `sub:`.
        let app = router_rl(Guard::Oidc(cfg), 2);
        for (i, octet) in [10u8, 11, 12].into_iter().enumerate() {
            let peer: SocketAddr = format!("203.0.113.{octet}:5000").parse().unwrap();
            let resp = app
                .clone()
                .oneshot(mcp_request_from(Some(&token), peer))
                .await
                .unwrap();
            if i < 2 {
                assert_eq!(resp.status(), StatusCode::OK, "request {i} should pass");
            } else {
                assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
            }
        }

        let events = events.lock().unwrap();
        // Three requests → three audit events; the third is rate-limited per-sub.
        assert_eq!(events.len(), 3);
        let last = events.last().unwrap();
        assert_eq!(last.get("status"), Some("429"));
        assert_eq!(last.get("outcome"), Some("rate-limited"));
        assert_eq!(last.get("caller"), Some("sub:user-42"));
    }

    // --- (M3): unauthenticated requests are rate-limited (pre-auth peer-IP) -----

    /// An UNAUTHENTICATED flood (no/invalid bearer in OIDC mode) from one peer is
    /// throttled by the pre-auth peer-IP limiter: the auth failures themselves are
    /// capped, not just authenticated traffic. The limiter is reached BEFORE the
    /// 401, so request N+1 from the same peer is a 429.
    #[tokio::test]
    async fn unauthenticated_flood_from_one_peer_is_rate_limited() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));

        let app = router_rl(Guard::Oidc(cfg), 3);
        let peer: SocketAddr = "198.51.100.7:6000".parse().unwrap();

        // The first 3 invalid-bearer requests are 401 (auth fails, but they count).
        for i in 1..=3 {
            let resp = app
                .clone()
                .oneshot(mcp_request_from(Some("not-a-real-token"), peer))
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::UNAUTHORIZED,
                "request {i} should 401 (auth-failed but counted)"
            );
        }
        // The 4th from the same peer is throttled BEFORE auth → 429 + Retry-After.
        let resp = app
            .oneshot(mcp_request_from(Some("not-a-real-token"), peer))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            resp.headers().get(header::RETRY_AFTER).is_some(),
            "the pre-auth 429 carries Retry-After"
        );
    }

    /// A *missing* bearer (not just an invalid one) is throttled too — the pre-auth
    /// limiter runs for every request regardless of whether a token was presented.
    #[tokio::test]
    async fn unauthenticated_flood_with_no_bearer_is_rate_limited() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));

        let app = router_rl(Guard::Oidc(cfg), 2);
        let peer: SocketAddr = "198.51.100.8:6000".parse().unwrap();

        for _ in 0..2 {
            let resp = app
                .clone()
                .oneshot(mcp_request_from(None, peer))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        let resp = app.oneshot(mcp_request_from(None, peer)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    /// The pre-auth limiter is per-peer: one peer flooding never throttles another.
    #[tokio::test]
    async fn unauthenticated_flood_does_not_affect_a_different_peer() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));

        let app = router_rl(Guard::Oidc(cfg), 1);
        let flooder: SocketAddr = "198.51.100.9:6000".parse().unwrap();
        let bystander: SocketAddr = "198.51.100.10:6000".parse().unwrap();

        // The flooder spends its allowance and is then throttled.
        let _ = app
            .clone()
            .oneshot(mcp_request_from(Some("bad"), flooder))
            .await
            .unwrap();
        let limited = app
            .clone()
            .oneshot(mcp_request_from(Some("bad"), flooder))
            .await
            .unwrap();
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        // A different peer's first request is unaffected — it gets the normal 401.
        let other = app
            .oneshot(mcp_request_from(Some("bad"), bystander))
            .await
            .unwrap();
        assert_eq!(other.status(), StatusCode::UNAUTHORIZED);
    }

    /// With the limiter disabled (`--rate-limit 0` / absent), an unauthenticated
    /// flood is NEVER throttled — the pre-auth path also respects "disabled".
    #[tokio::test]
    async fn unauthenticated_flood_disabled_never_429s() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));

        let app = router(Guard::Oidc(cfg)); // no limiter
        let peer: SocketAddr = "198.51.100.11:6000".parse().unwrap();
        for _ in 0..10 {
            let resp = app
                .clone()
                .oneshot(mcp_request_from(Some("bad"), peer))
                .await
                .unwrap();
            // Always the normal 401 — never a 429.
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
    }

    /// The pre-auth 429 carries the `MCP-Protocol-Version` header (round-1 fix),
    /// like every other response — including the unauthenticated throttle path.
    #[tokio::test]
    async fn unauthenticated_429_carries_protocol_header() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));

        let app = router_rl(Guard::Oidc(cfg), 1);
        let peer: SocketAddr = "198.51.100.12:6000".parse().unwrap();
        let _ = app
            .clone()
            .oneshot(mcp_request_from(Some("bad"), peer))
            .await
            .unwrap();
        let (status, ver) = protocol_header(app, mcp_request_from(Some("bad"), peer)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(ver.as_deref(), Some(MCP_PROTOCOL_VERSION));
    }

    /// The unauthenticated 429 is audited too, with no identity leaked: the
    /// outcome is `rate-limited`, the caller is the peer IP, and there's no `sub`.
    #[tokio::test]
    async fn unauthenticated_429_is_audited_without_identity() {
        let (events, _guard) = capture();

        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));

        let app = router_rl(Guard::Oidc(cfg), 1);
        let peer: SocketAddr = "198.51.100.13:6000".parse().unwrap();
        let _ = app
            .clone()
            .oneshot(mcp_request_from(Some("bad"), peer))
            .await
            .unwrap();
        let limited = app
            .oneshot(mcp_request_from(Some("bad"), peer))
            .await
            .unwrap();
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);

        let events = events.lock().unwrap();
        let last = events.last().unwrap();
        assert_eq!(last.get("status"), Some("429"));
        assert_eq!(last.get("outcome"), Some("rate-limited"));
        assert_eq!(last.get("caller"), Some("ip:198.51.100.13"));
        // The auth mode is still recorded (oidc), but no token identity leaked.
        assert_eq!(last.get("auth"), Some("oidc"));
        assert_eq!(last.get("sub"), None);
    }

    // --- (M2): explicit-port allow-list matching, end to end through the gate ---

    /// In OIDC/routable mode, an allow-list entry with an explicit port accepts an
    /// Origin on that exact `host:port` and rejects the same host on another port.
    #[tokio::test]
    async fn oidc_origin_explicit_port_matches_only_that_port() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        let allowed = AllowedHosts {
            extra: vec![allowed_host_port("nbox.test", 8443)],
        };
        let app = router_hosts(Guard::Oidc(cfg), allowed);

        // The exact host:port passes.
        let (ok, _www, _) = send(
            app.clone(),
            mcp_request_origin(Some(&token), "https://nbox.test:8443"),
        )
        .await;
        assert_eq!(ok, StatusCode::OK, "matching host:port should pass");

        // A different port on the same host is rejected.
        let (mismatch, _www, _) = send(
            app.clone(),
            mcp_request_origin(Some(&token), "https://nbox.test:9443"),
        )
        .await;
        assert_eq!(mismatch, StatusCode::FORBIDDEN, "mismatched port is 403");

        // The same host with no port is rejected too (the entry demands the port).
        let (no_port, _www, _) =
            send(app, mcp_request_origin(Some(&token), "https://nbox.test")).await;
        assert_eq!(no_port, StatusCode::FORBIDDEN, "no-port origin is 403");
    }

    /// A port-less allow-list entry keeps any-port matching: an Origin on any port
    /// for that host passes (the documented no-port behavior, unchanged).
    #[tokio::test]
    async fn oidc_origin_no_port_entry_matches_any_port() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        let allowed = AllowedHosts {
            extra: vec![allowed_host("nbox.test")],
        };
        let app = router_hosts(Guard::Oidc(cfg), allowed);

        for origin in [
            "https://nbox.test",
            "https://nbox.test:8443",
            "https://nbox.test:1234",
        ] {
            let (status, _www, _) =
                send(app.clone(), mcp_request_origin(Some(&token), origin)).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "{origin} should pass (any-port entry)"
            );
        }
    }

    // --- (d): MCP-Protocol-Version on EVERY response, including errors ---------

    /// The `MCP-Protocol-Version` header value on the response to `req`, if any.
    async fn protocol_header(router: Router, req: Request<Body>) -> (StatusCode, Option<String>) {
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let ver = resp
            .headers()
            .get("mcp-protocol-version")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        (status, ver)
    }

    #[tokio::test]
    async fn protocol_header_present_on_401_invalid_token() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let mut claims = good_claims();
        claims["aud"] = json!("https://someone-else.test");
        let token = idp.mint(&claims);

        let (status, ver) =
            protocol_header(router(Guard::Oidc(cfg)), mcp_request(Some(&token))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(ver.as_deref(), Some(MCP_PROTOCOL_VERSION));
    }

    #[tokio::test]
    async fn protocol_header_present_on_401_missing_token() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));

        let (status, ver) = protocol_header(router(Guard::Oidc(cfg)), mcp_request(None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(ver.as_deref(), Some(MCP_PROTOCOL_VERSION));
    }

    #[tokio::test]
    async fn protocol_header_present_on_403_insufficient_scope() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let mut claims = good_claims();
        claims["scope"] = json!("nbox:write");
        let token = idp.mint(&claims);

        let (status, ver) =
            protocol_header(router(Guard::Oidc(cfg)), mcp_request(Some(&token))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(ver.as_deref(), Some(MCP_PROTOCOL_VERSION));
    }

    #[tokio::test]
    async fn protocol_header_present_on_403_forbidden_origin() {
        let guard = Guard::Loopback { token: None };
        let req = Request::builder()
            .uri("/mcp")
            .method("GET")
            .header(header::ORIGIN, "http://evil.example.com")
            .body(Body::empty())
            .unwrap();
        let (status, ver) = protocol_header(router(guard), req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(ver.as_deref(), Some(MCP_PROTOCOL_VERSION));
    }

    #[tokio::test]
    async fn protocol_header_present_on_429_rate_limited() {
        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        let app = router_rl(Guard::Oidc(cfg), 1);
        // Spend the one allowance, then the next is 429.
        let _ = app
            .clone()
            .oneshot(mcp_request(Some(&token)))
            .await
            .unwrap();
        let (status, ver) = protocol_header(app, mcp_request(Some(&token))).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(ver.as_deref(), Some(MCP_PROTOCOL_VERSION));
    }

    #[tokio::test]
    async fn protocol_header_present_on_401_loopback_static_bearer() {
        let guard = Guard::Loopback {
            token: Some(Arc::from("s3cret")),
        };
        let (status, ver) = protocol_header(router(guard), mcp_request(Some("wrong"))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(ver.as_deref(), Some(MCP_PROTOCOL_VERSION));
    }

    // --- (e): the audit `session` field is a hash, never the raw id -----------

    #[tokio::test]
    async fn audit_session_field_is_hashed_not_the_raw_id() {
        let (events, _guard) = capture();

        let idp = Arc::new(MockIdp::new("k1"));
        let base = spawn_idp(idp.clone()).await;
        let cfg = oidc_config(&format!("{base}/jwks"));
        let token = idp.mint(&good_claims());

        let raw_session = "mcp-session-deadbeef-secret-handle";
        let mut req = mcp_request(Some(&token));
        req.headers_mut().insert(
            "mcp-session-id",
            header::HeaderValue::from_static("mcp-session-deadbeef-secret-handle"),
        );
        let (status, _www, _) = send(router(Guard::Oidc(cfg)), req).await;
        assert_eq!(status, StatusCode::OK);

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        // The `session` field is the SHA-256 prefix, NOT the raw id.
        let logged = e.get("session").expect("a session field");
        assert_eq!(logged, super::super::audit::session_hash(raw_session));
        assert_ne!(logged, raw_session, "raw session id must not be logged");
        // No field anywhere leaks the raw session token.
        for (name, value) in &e.fields {
            assert!(
                !value.contains(raw_session),
                "audit field {name} leaked the raw session id: {value}"
            );
        }
        // The old field name is gone (it's now `session`).
        assert_eq!(e.get("session_id"), None);
    }
}
