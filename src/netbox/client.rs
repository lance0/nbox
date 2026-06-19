//! The NetBox REST client.
//!
//! REST is the primary integration path. The client carries auth, paging, and
//! the per-profile config-context optimization. Tokens are never logged — debug
//! logging emits a redacted scheme marker only (see [`NetBoxClient::masked_authorization`]).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Url;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::{BackendKind, ProfileConfig};
use crate::error::NboxError;
use crate::netbox::auth::AuthScheme;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::graphql::GraphqlCapabilities;
use crate::netbox::pagination::Page;

/// NetBox's default list page size when no `limit` is sent.
const DEFAULT_PAGE_SIZE: usize = 100;

/// NetBox caps `limit` at `MAX_PAGE_SIZE` server-side; sending more is silently
/// reduced to this, so we clamp at construction to keep `limit`/`offset` windows
/// aligned (see `list_all`).
pub(crate) const MAX_PAGE_SIZE: usize = 1000;

/// An HTTP client bound to a single NetBox instance/profile.
#[derive(Clone)]
pub struct NetBoxClient {
    base_url: Url,
    token: Option<String>,
    auth_scheme: AuthScheme,
    http: reqwest::Client,
    page_size: usize,
    exclude_config_context: bool,
    backend: BackendKind,
    graphql_capabilities: Arc<tokio::sync::OnceCell<GraphqlCapabilities>>,
}

impl NetBoxClient {
    /// Build a client from a profile and an optional resolved token.
    pub fn new(profile: &ProfileConfig, token: Option<String>) -> Result<Self> {
        let timeout = Duration::from_secs(profile.timeout_secs.unwrap_or(15));
        let verify_tls = profile.verify_tls.unwrap_or(true);

        let http = reqwest::Client::builder()
            .timeout(timeout)
            .danger_accept_invalid_certs(!verify_tls)
            .build()
            .context("building the HTTP client")?;

        // Ensure a trailing slash so `Url::join` preserves any base subpath
        // (e.g. NetBox installed under `https://host/netbox/`).
        let mut url = profile.url.clone();
        if !url.ends_with('/') {
            url.push('/');
        }
        let base_url =
            Url::parse(&url).with_context(|| format!("invalid NetBox URL: {}", profile.url))?;

        // Clamp the page size into NetBox's valid window. A profile value of `0`
        // means "return ALL" to NetBox (no `limit` cap), which breaks our
        // offset-windowed paging, so it falls back to the default. Anything else
        // is clamped to `1..=MAX_PAGE_SIZE`: values above the server cap would be
        // silently reduced server-side, desyncing our `offset` from the page size.
        let page_size = match profile.page_size {
            None | Some(0) => DEFAULT_PAGE_SIZE,
            Some(n) => n.clamp(1, MAX_PAGE_SIZE),
        };

        Ok(Self {
            base_url,
            token,
            auth_scheme: profile.auth_scheme.unwrap_or_default(),
            http,
            page_size,
            exclude_config_context: profile.exclude_config_context.unwrap_or(true),
            backend: profile.backend(),
            graphql_capabilities: Arc::new(tokio::sync::OnceCell::new()),
        })
    }

    /// The preferred read backend for this client/profile.
    pub fn backend(&self) -> BackendKind {
        self.backend
    }

    pub(crate) fn graphql_capability_cache(&self) -> &tokio::sync::OnceCell<GraphqlCapabilities> {
        &self.graphql_capabilities
    }

    /// The effective page size sent as `limit` (clamped into `1..=MAX_PAGE_SIZE`).
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    /// Whether list calls for devices/VMs omit config context.
    pub fn exclude_config_context(&self) -> bool {
        self.exclude_config_context
    }

    /// The configured NetBox base URL.
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Resolve an API path against the base URL.
    fn url_for(&self, path: &str) -> Result<Url> {
        self.base_url
            .join(path.trim_start_matches('/'))
            .with_context(|| format!("building URL for {path}"))
    }

    /// A loggable, redacted form of the `Authorization` header. Returns the
    /// scheme word with the token masked — never the token itself.
    fn masked_authorization(&self) -> Option<String> {
        let token = self.token.as_ref()?;
        let header = self.auth_scheme.header_value(token);
        let scheme = header.split_whitespace().next().unwrap_or("Token");
        Some(format!("{scheme} ****"))
    }

    /// Issue an authenticated GET, returning the raw response. Retries on HTTP
    /// 429 (rate limited), honoring `Retry-After` when present, up to a small cap.
    async fn send(&self, path: &str, params: &[(&str, String)]) -> Result<reqwest::Response> {
        const MAX_RETRIES: u32 = 3;
        let url = self.url_for(path)?;

        match self.masked_authorization() {
            Some(auth) => tracing::debug!(url = %url, auth = %auth, "GET"),
            None => tracing::debug!(url = %url, "GET (unauthenticated)"),
        }

        let mut attempt = 0;
        loop {
            let mut req = self
                .http
                .get(url.clone())
                .query(params)
                .header(ACCEPT, "application/json");
            if let Some(token) = &self.token {
                req = req.header(AUTHORIZATION, self.auth_scheme.header_value(token));
            }

            let res = req.send().await.context("sending request to NetBox")?;
            if res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < MAX_RETRIES {
                let wait = parse_retry_after(
                    res.headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok()),
                )
                .unwrap_or_else(|| backoff(attempt));
                tracing::warn!("NetBox rate limited (429); retrying in {wait:?}");
                tokio::time::sleep(wait).await;
                attempt += 1;
                continue;
            }
            return Ok(res);
        }
    }

    /// Decode a successful response, or turn a non-2xx status into a typed error.
    async fn decode<T: DeserializeOwned>(res: reqwest::Response) -> Result<T> {
        let status = res.status();
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            return Err(status_error(status, &body).into());
        }
        res.json::<T>().await.context("decoding NetBox response")
    }

    /// Perform a GET request and deserialize the JSON response.
    pub async fn get<T>(&self, path: &str, params: &[(&str, String)]) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let res = self.send(path, params).await?;
        Self::decode(res).await
    }

    /// Like [`get`](Self::get), but maps HTTP 404 to `Ok(None)` (so a missing
    /// object by ID is "not found", not an error).
    pub async fn get_optional<T>(&self, path: &str, params: &[(&str, String)]) -> Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        let res = self.send(path, params).await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(Self::decode(res).await?))
    }

    /// Fetch a single page from a list endpoint, applying the page size and the
    /// config-context exclusion for devices/VMs.
    pub async fn list<T>(
        &self,
        endpoint: Endpoint,
        mut params: Vec<(&str, String)>,
    ) -> Result<Page<T>>
    where
        T: DeserializeOwned,
    {
        params.push(("limit", self.page_size.to_string()));
        if self.exclude_config_context && endpoint.has_config_context() {
            params.push(("exclude", "config_context".to_string()));
        }
        self.get(endpoint.path(), &params).await
    }

    /// Perform an authenticated GraphQL POST and deserialize the `data` object.
    ///
    /// GraphQL rides the same NetBox base URL, auth header, TLS settings, timeout,
    /// and 429 retry policy as REST. GraphQL-level errors are mapped to the
    /// generic API exit-code bucket; HTTP 401/403/404 keep the stable status
    /// mapping from REST.
    pub(crate) async fn graphql<T, V>(&self, query: &str, variables: V) -> Result<T>
    where
        T: DeserializeOwned,
        V: Serialize,
    {
        const MAX_RETRIES: u32 = 3;
        let url = self.url_for("/graphql/")?;
        let body = json!({
            "query": query,
            "variables": variables,
        });

        match self.masked_authorization() {
            Some(auth) => tracing::debug!(url = %url, auth = %auth, "GraphQL POST"),
            None => tracing::debug!(url = %url, "GraphQL POST (unauthenticated)"),
        }

        let mut attempt = 0;
        loop {
            let mut req = self
                .http
                .post(url.clone())
                .header(ACCEPT, "application/json")
                .header(CONTENT_TYPE, "application/json")
                .json(&body);
            if let Some(token) = &self.token {
                req = req.header(AUTHORIZATION, self.auth_scheme.header_value(token));
            }

            let res = req
                .send()
                .await
                .context("sending GraphQL request to NetBox")?;
            if res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < MAX_RETRIES {
                let wait = parse_retry_after(
                    res.headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok()),
                )
                .unwrap_or_else(|| backoff(attempt));
                tracing::warn!("NetBox GraphQL rate limited (429); retrying in {wait:?}");
                tokio::time::sleep(wait).await;
                attempt += 1;
                continue;
            }

            return Self::decode_graphql(res).await;
        }
    }

    async fn decode_graphql<T: DeserializeOwned>(res: reqwest::Response) -> Result<T> {
        let status = res.status();
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            return Err(status_error(status, &body).into());
        }

        let envelope: GraphqlEnvelope<T> = res.json().await.context("decoding GraphQL response")?;
        if !envelope.errors.is_empty() {
            let body = envelope
                .errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(NboxError::Api { status: 200, body }.into());
        }
        envelope
            .data
            .context("GraphQL response did not include data")
    }

    /// Page through a list endpoint, collecting up to `max` objects.
    pub async fn list_all<T>(
        &self,
        endpoint: Endpoint,
        base_params: Vec<(&str, String)>,
        max: usize,
    ) -> Result<Vec<T>>
    where
        T: DeserializeOwned,
    {
        let mut out: Vec<T> = Vec::new();
        let mut offset = 0usize;

        loop {
            let mut params = base_params.clone();
            params.push(("limit", self.page_size.to_string()));
            params.push(("offset", offset.to_string()));
            if self.exclude_config_context && endpoint.has_config_context() {
                params.push(("exclude", "config_context".to_string()));
            }

            let page: Page<T> = self.get(endpoint.path(), &params).await?;
            let got = page.results.len();
            out.extend(page.results);

            if got == 0 || out.len() >= page.count || out.len() >= max {
                break;
            }
            // NetBox `limit`/`offset` are absolute row windows: page N starts at
            // `offset = N * limit`. Advance by the requested page size, not by the
            // rows actually returned — a page can come back short (the server caps
            // `limit` at MAX_PAGE_SIZE, or a serializer drops rows post-count), and
            // `offset += got` would then land mid-window and skip rows.
            offset += self.page_size;
        }

        out.truncate(max);
        Ok(out)
    }
}

#[derive(Debug, Deserialize)]
struct GraphqlEnvelope<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

/// Parse a `Retry-After` header value (delay in seconds; HTTP-date form is not
/// honored). Caps the wait so a hostile/large value can't stall us indefinitely.
fn parse_retry_after(value: Option<&str>) -> Option<Duration> {
    let secs: u64 = value?.trim().parse().ok()?;
    Some(Duration::from_secs(secs.min(60)))
}

/// Exponential backoff for retry `attempt` (0-based): 0.5s, 1s, 2s, …
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(500u64 << attempt.min(6))
}

/// Map a non-success HTTP status to a typed [`NboxError`] so exit codes are
/// consistent no matter which path hit the error: 401→auth, 403→perms, 404→not
/// found, everything else→generic API error. (Note: `get_optional` intercepts
/// 404 earlier as `Ok(None)`; this covers raw 404s on `get`, e.g. `nbox raw`.)
fn status_error(status: reqwest::StatusCode, body: &str) -> NboxError {
    match status {
        reqwest::StatusCode::UNAUTHORIZED => NboxError::Authentication,
        reqwest::StatusCode::FORBIDDEN => NboxError::PermissionDenied,
        reqwest::StatusCode::NOT_FOUND => {
            let body = body.trim();
            if body.is_empty() {
                NboxError::NotFound("not found (HTTP 404)".to_string())
            } else {
                NboxError::NotFound(format!("not found (HTTP 404): {}", truncate(body, 200)))
            }
        }
        other => NboxError::Api {
            status: other.as_u16(),
            body: truncate(body, 500),
        },
    }
}

/// Truncate a string to at most `max` characters, appending an ellipsis if cut.
fn truncate(s: &str, max: usize) -> String {
    let t: String = s.chars().take(max).collect();
    if t.len() < s.len() {
        format!("{t}…")
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_with(token: &str, scheme: Option<AuthScheme>) -> NetBoxClient {
        let profile = ProfileConfig {
            url: "http://localhost".into(),
            auth_scheme: scheme,
            ..Default::default()
        };
        NetBoxClient::new(&profile, Some(token.into())).unwrap()
    }

    #[test]
    fn masked_auth_never_contains_token() {
        let c = client_with("nbt_key.secretpart", None);
        let masked = c.masked_authorization().unwrap();
        assert_eq!(masked, "Bearer ****");
        assert!(!masked.contains("secretpart"));

        let c = client_with("legacysecret", None);
        let masked = c.masked_authorization().unwrap();
        assert_eq!(masked, "Token ****");
        assert!(!masked.contains("legacysecret"));
    }

    #[test]
    fn base_url_join_preserves_subpath() {
        let profile = ProfileConfig {
            url: "http://h/netbox".into(),
            ..Default::default()
        };
        let c = NetBoxClient::new(&profile, None).unwrap();
        assert_eq!(
            c.url_for("/api/dcim/sites/").unwrap().as_str(),
            "http://h/netbox/api/dcim/sites/"
        );
    }

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 3), "hel…");
    }

    #[test]
    fn retry_after_parses_seconds_and_caps() {
        assert_eq!(parse_retry_after(Some("5")), Some(Duration::from_secs(5)));
        assert_eq!(
            parse_retry_after(Some("  2 ")),
            Some(Duration::from_secs(2))
        );
        // Capped so a huge value can't stall the client.
        assert_eq!(
            parse_retry_after(Some("9999")),
            Some(Duration::from_secs(60))
        );
        // HTTP-date form and garbage are ignored (fall back to backoff).
        assert_eq!(
            parse_retry_after(Some("Wed, 21 Oct 2099 07:28:00 GMT")),
            None
        );
        assert_eq!(parse_retry_after(None), None);
    }

    #[test]
    fn backoff_grows() {
        assert!(backoff(0) < backoff(1));
        assert!(backoff(1) < backoff(2));
    }

    #[test]
    fn status_error_maps_to_stable_exit_codes() {
        use reqwest::StatusCode;
        assert_eq!(status_error(StatusCode::UNAUTHORIZED, "").exit_code(), 3);
        assert_eq!(status_error(StatusCode::FORBIDDEN, "").exit_code(), 3);
        // 404 is now "not found" (exit 4) regardless of path — consistent with
        // `nbox device 999999`, even for a raw `nbox raw GET /…/999999/`.
        assert_eq!(
            status_error(StatusCode::NOT_FOUND, "{\"detail\":\"Not found.\"}").exit_code(),
            4
        );
        assert_eq!(status_error(StatusCode::NOT_FOUND, "").exit_code(), 4);
        // Other statuses stay generic (exit 1).
        assert_eq!(
            status_error(StatusCode::INTERNAL_SERVER_ERROR, "boom").exit_code(),
            1
        );
    }
}
