//! Loopback HTTP transport for `nbox serve --http` (rung 2 of the MCP transport
//! ladder; see `DESIGN.md` §24).
//!
//! This is an *alternate transport* for the exact same [`NboxMcp`] server the
//! stdio path serves — the handler, tool router, and eight read-only tools are
//! reused unchanged. rmcp's Streamable HTTP server ([`StreamableHttpService`] +
//! [`LocalSessionManager`]) is mounted at `/mcp` on an axum router.
//!
//! Unit 1 is **loopback only, no OAuth**. The trust boundary is the loopback
//! interface; an optional static bearer adds a second factor for local clients
//! that want it. Network-reachable binding + OIDC is a later unit.
//!
//! Security (DESIGN §24, mandatory):
//! - Bind only the loopback address given; reject non-loopback at the CLI.
//! - Validate the `Origin` header on every request → 403 on a non-loopback
//!   origin (DNS-rebinding defense). rmcp additionally validates the `Host`
//!   header against the loopback allow-list.
//! - Advertise `MCP-Protocol-Version: 2025-11-25` on every response.
//! - stdout stays clean: the protocol travels over the HTTP body, and all logs
//!   go to stderr/file exactly as the stdio path does. Nothing here writes to
//!   stdout.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio_util::sync::CancellationToken;

use super::NboxMcp;
use crate::error::NboxError;
use crate::netbox::client::NetBoxClient;

/// The MCP protocol revision this server targets (DESIGN §24). Advertised on
/// every HTTP response so clients can pin the wire version.
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// Per-request guard state shared with the middleware: the optional static
/// bearer token. Cheap to clone (an `Arc`), so it rides as axum router state.
#[derive(Clone)]
struct Guard {
    /// `Some` ⇒ require `Authorization: Bearer <token>` on `/mcp`. `None` ⇒ no
    /// auth (loopback is the trust boundary).
    token: Option<Arc<str>>,
}

/// Serve the read-only MCP server over loopback HTTP until interrupted.
///
/// `addr` must be a loopback socket address (validated by the caller / here);
/// `token`, when `Some`, is the static bearer required on `/mcp`. The same
/// [`NboxMcp`] used by the stdio path backs every request. stdout is never
/// written — the protocol is the HTTP body and logs go to stderr.
pub async fn serve_http(client: NetBoxClient, addr: &str, token: Option<String>) -> Result<()> {
    let socket = parse_loopback_addr(addr)?;

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

    let guard = Guard {
        token: token.map(|t| Arc::from(t.as_str())),
    };
    // The gate runs on every response (including 401/403/404), so the
    // protocol-version header and the bearer/Origin checks cover the whole path.
    let router = axum::Router::new()
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(guard, gate));

    let listener = tokio::net::TcpListener::bind(socket)
        .await
        .with_context(|| format!("binding {socket}"))?;
    tracing::info!(%socket, "nbox MCP server listening (HTTP)");

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

/// Parse `addr` to a [`SocketAddr`] and require it to be loopback.
///
/// Binding a non-loopback interface is rejected as a [`NboxError::Usage`] (exit
/// `2`): it requires the OIDC auth mode coming in a later unit. There is no
/// bypass flag by design.
fn parse_loopback_addr(addr: &str) -> Result<SocketAddr> {
    let socket: SocketAddr = addr.parse().map_err(|_| {
        NboxError::Usage(format!(
            "--http expects an IP:PORT address, e.g. 127.0.0.1:8080 (got \"{addr}\")"
        ))
    })?;
    if !is_loopback(socket.ip()) {
        return Err(NboxError::Usage(format!(
            "--http {addr} is not a loopback address. Unit 1 binds loopback only \
             (127.0.0.0/8 or ::1); binding a routable interface requires the OIDC \
             auth mode coming in a later release."
        ))
        .into());
    }
    Ok(socket)
}

/// Whether `ip` is a loopback address (127.0.0.0/8 or ::1).
fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

/// Axum middleware on `/mcp`: enforce the optional static bearer, validate the
/// `Origin` header, and advertise the MCP protocol version on the response.
///
/// Order: bearer (401) → origin (403) → inner service. Both checks fail closed.
async fn gate(State(guard): State<Guard>, request: Request<Body>, next: Next) -> Response {
    // 1) Static bearer, if configured. Missing/wrong → 401. Constant-time
    //    compare so a failure can't be timed. NEVER log the token.
    if let Some(expected) = guard.token.as_deref() {
        let presented = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        let ok = presented.is_some_and(|got| ct_eq(got.as_bytes(), expected.as_bytes()));
        if !ok {
            return unauthorized();
        }
    }

    // 2) Origin validation (DNS-rebinding defense). A request *with* an Origin
    //    must carry a loopback origin; requests without one (non-browser MCP
    //    clients) pass. A malformed or non-loopback Origin → 403.
    if let Some(origin) = request.headers().get(header::ORIGIN) {
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

/// 401 with a `WWW-Authenticate: Bearer` challenge. The body is generic — never
/// echoes the token or what was presented.
fn unauthorized() -> Response {
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
    fn loopback_addrs_are_accepted() {
        assert!(parse_loopback_addr("127.0.0.1:8080").is_ok());
        assert!(parse_loopback_addr("127.0.0.5:1234").is_ok());
        assert!(parse_loopback_addr("[::1]:8080").is_ok());
    }

    #[test]
    fn non_loopback_addrs_are_usage_errors() {
        for addr in ["0.0.0.0:8080", "192.168.1.10:8080", "[2001:db8::1]:8080"] {
            let err = parse_loopback_addr(addr).unwrap_err();
            let nbox = err
                .chain()
                .find_map(|e| e.downcast_ref::<NboxError>())
                .expect("a typed NboxError");
            assert!(
                matches!(nbox, NboxError::Usage(_)),
                "{addr} should be Usage"
            );
        }
    }

    #[test]
    fn garbage_addr_is_a_usage_error() {
        let err = parse_loopback_addr("not-an-addr").unwrap_err();
        assert!(
            err.chain()
                .any(|e| matches!(e.downcast_ref::<NboxError>(), Some(NboxError::Usage(_))))
        );
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
}
