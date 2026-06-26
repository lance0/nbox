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

use crate::config::{ApiConfig, ApiSurface, BackendPreference, ProfileConfig};
use crate::error::NboxError;
use crate::netbox::auth::AuthScheme;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::graphql::GraphqlCapabilities;
use crate::netbox::pagination::Page;

/// NetBox's default list page size when no `limit` is sent.
const DEFAULT_PAGE_SIZE: usize = 100;

/// NetBox caps `limit` at `MAX_PAGE_SIZE` server-side; sending more is silently
/// reduced to this, so we clamp at construction. `list_all` follows the server's
/// `next` link from the second page on, so the clamp just bounds the first
/// page's request size — pagination alignment comes from `next`, not from our
/// `limit`/`offset` matching (see `list_all`).
pub(crate) const MAX_PAGE_SIZE: usize = 1000;

pub(crate) type GraphqlProbeResult = std::result::Result<GraphqlCapabilities, String>;

/// An HTTP client bound to a single NetBox instance/profile.
#[derive(Clone)]
pub struct NetBoxClient {
    base_url: Url,
    token: Option<String>,
    auth_scheme: AuthScheme,
    http: reqwest::Client,
    page_size: usize,
    exclude_config_context: bool,
    api: ApiConfig,
    graphql_capabilities: Arc<tokio::sync::OnceCell<GraphqlProbeResult>>,
}

impl NetBoxClient {
    /// Build a client from a profile and an optional resolved token.
    pub fn new(profile: &ProfileConfig, token: Option<String>) -> Result<Self> {
        let timeout = Duration::from_secs(profile.timeout_secs.unwrap_or(15));
        let verify_tls = profile.verify_tls.unwrap_or(true);

        // Keep the connect timeout under the overall timeout (a 10s connect
        // budget with a 5s total is confusing); clamp to [1s, 10s].
        let connect_timeout = Duration::from_secs(10)
            .min(timeout.saturating_sub(Duration::from_secs(1)))
            .max(Duration::from_secs(1));

        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(connect_timeout)
            // NetBox is commonly served by gunicorn *sync* workers, which close
            // the connection after each response rather than honoring HTTP/1.1
            // keep-alive. nbox's search fan-out fires ~17 requests at once; a
            // pooled connection the server has already half-closed can hang a
            // reused request to the full timeout. Keep *zero* idle connections in
            // the pool, so a completed request's connection is dropped rather than
            // reused (like curl). This only changes the post-response reuse path:
            // concurrency is unaffected — the fan-out still opens up to ~17
            // connections at once; they just aren't pooled for the next request.
            .pool_max_idle_per_host(0)
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
        // means "return ALL" to NetBox (no `limit` cap), which defeats paging, so
        // it falls back to the default. Anything else is clamped to
        // `1..=MAX_PAGE_SIZE`: values above the server cap would be silently
        // reduced server-side. (`list_all` follows the server's `next` link, so a
        // server-side clamp no longer desyncs our pagination — but an unclamped
        // `0`/huge value still requests an unbounded first page, hence the
        // floor/ceiling here.)
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
            api: profile.api.clone().unwrap_or_default(),
            graphql_capabilities: Arc::new(tokio::sync::OnceCell::new()),
        })
    }

    /// The configured backend preference for `surface` (REST when unset). This is
    /// the *preference*; [`effective_backend`](Self::effective_backend) resolves it
    /// against the capability probe.
    pub fn api_preference(&self, surface: ApiSurface) -> BackendPreference {
        match surface {
            ApiSurface::Search => self.api.search,
            ApiSurface::Vrf => self.api.vrf,
            ApiSurface::RouteTarget => self.api.route_target,
        }
        .unwrap_or_default()
    }

    /// True when a surface that can actually run over GraphQL prefers it — the
    /// gate for probing the schema so a pure-REST profile keeps `nbox status`
    /// cheap. Search is REST-canonical (see [`crate::netbox::capabilities`]), so
    /// a `search = "graphql"` preference never triggers a probe.
    pub(crate) fn any_graphql_preferred(&self) -> bool {
        self.api.vrf == Some(BackendPreference::Graphql)
            || self.api.route_target == Some(BackendPreference::Graphql)
    }

    pub(crate) fn graphql_capability_cache(&self) -> &tokio::sync::OnceCell<GraphqlProbeResult> {
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
            if retry_on_rate_limit(&res, attempt, "NetBox").await {
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

    /// Like [`get`](Self::get), but also captures the `ETag` response header when
    /// NetBox sends one (4.6+ detail responses). The write foundation uses this
    /// to record the server precondition on a read-before-write so the apply step
    /// can send `If-Match` (ADR-0001 §3). On older releases the `ETag` header is
    /// absent and the second element is `None`, in which case the planner falls
    /// back to `last_updated` + a before-hash.
    pub(crate) async fn get_with_etag<T>(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<(T, Option<String>)>
    where
        T: DeserializeOwned,
    {
        let res = self.send(path, params).await?;
        let etag = res
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = Self::decode(res).await?;
        Ok((body, etag))
    }

    /// Issue an authenticated `OPTIONS` (read-only metadata; DRF field schemas
    /// and choice enumerations). Reuses the read path's auth, timeout, URL
    /// scoping, and token redaction. Used by the write foundation to enumerate a
    /// choice field's allowed values *before* sending a `PATCH` (ADR-0001 §6),
    /// e.g. a device's `status`. `OPTIONS` is read-only (no body is sent and none
    /// is expected beyond DRF's metadata), so it shares the `GET`-style status
    /// mapping — a non-2xx is the standard read [`status_error`].
    pub(crate) async fn options(&self, path: &str) -> Result<serde_json::Value> {
        let url = self.url_for(path)?;
        match self.masked_authorization() {
            Some(auth) => tracing::debug!(url = %url, auth = %auth, "OPTIONS"),
            None => tracing::debug!(url = %url, "OPTIONS (unauthenticated)"),
        }
        let res = self
            .http
            .request(reqwest::Method::OPTIONS, url)
            .send()
            .await?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(status_error(status, &text).into());
        }
        // DRF's OPTIONS body is JSON metadata; tolerate an empty body (some
        // proxies strip it) by falling back to an empty object, which the choice
        // extractor treats as "couldn't enumerate" (a usage error, never an
        // unvalidated PATCH).
        if text.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&text).context("decoding NetBox OPTIONS metadata")
    }

    /// Perform an authenticated `PATCH` (partial update) and deserialize the
    /// response. The single v1 write transport (ADR-0001 §2): reuses the read
    /// path's auth, timeout, 429 retry, URL scoping, status mapping, and token
    /// redaction. `if_match` sends `If-Match: <etag>` for optimistic concurrency
    /// on NetBox 4.6+; a stale object produces `412`, mapped to
    /// [`NboxError::StalePrecondition`]. A `400` carrying NetBox's field-level
    /// validation is mapped to [`NboxError::WriteValidation`] so the message can
    /// name the rejected field without polluting stdout. Returns the updated
    /// object and the new `ETag` (when present) for the receipt.
    pub(crate) async fn patch<T>(
        &self,
        path: &str,
        body: &serde_json::Value,
        if_match: Option<&str>,
    ) -> Result<(T, Option<String>, u16)>
    where
        T: DeserializeOwned,
    {
        let url = self.url_for(path)?;

        match self.masked_authorization() {
            Some(auth) => tracing::debug!(url = %url, auth = %auth, "PATCH"),
            None => tracing::debug!(url = %url, "PATCH (unauthenticated)"),
        }

        let mut attempt = 0;
        loop {
            let mut req = self
                .http
                .patch(url.clone())
                .header(ACCEPT, "application/json")
                .header(CONTENT_TYPE, "application/json")
                .json(body);
            if let Some(token) = &self.token {
                req = req.header(AUTHORIZATION, self.auth_scheme.header_value(token));
            }
            if let Some(etag) = if_match {
                req = req.header(reqwest::header::IF_MATCH, etag);
            }

            let res = req.send().await.context("sending PATCH to NetBox")?;
            if retry_on_rate_limit(&res, attempt, "NetBox PATCH").await {
                attempt += 1;
                continue;
            }

            let status = res.status();
            let etag = res
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            if !status.is_success() {
                let text = res.text().await.unwrap_or_default();
                return Err(write_status_error(status, &text).into());
            }
            let body: T = res.json().await.context("decoding PATCH response")?;
            return Ok((body, etag, status.as_u16()));
        }
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

    /// Like [`list`](Self::list) but caps the `limit` param at `max` (clamped to
    /// `min(page_size, max)`). Used by the search fan-out so a `--limit 25` search
    /// fetches at most 25 rows per endpoint instead of the full `page_size` (100
    /// by default) — the merge truncates to `req.limit` anyway, so the extra rows
    /// are deserialized and ranked only to be thrown away. A floor at the call
    /// site (`max(req.limit, floor)`) keeps each branch wide enough to contribute
    /// its share of the merged result.
    pub async fn list_limited<T>(
        &self,
        endpoint: Endpoint,
        mut params: Vec<(&str, String)>,
        max: usize,
    ) -> Result<Page<T>>
    where
        T: DeserializeOwned,
    {
        let limit = self.page_size.min(max);
        params.push(("limit", limit.to_string()));
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
            if retry_on_rate_limit(&res, attempt, "NetBox GraphQL").await {
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
        // Size the FIRST page to the larger of the configured `page_size` and
        // `max` (the latter capped at the server's MAX_PAGE_SIZE). A large fetch
        // — a 1000-row browse, a 1000-port panel, a 200-row VRF section — then
        // lands in one round trip instead of `ceil(max / page_size)` sequential
        // ones. The configured `page_size` (kept in 1..=MAX_PAGE_SIZE at
        // construction) is a throughput knob, so it's a floor here: a small `max`
        // keeps today's single-page behavior; a large `max` grows the page to
        // match, never past MAX_PAGE_SIZE. No call pulls more rows per request
        // than it did before unless `max` itself is larger (and those extra rows
        // are kept, capped at `max` — not wasted).
        let page_size = self.page_size.max(max.min(MAX_PAGE_SIZE));
        let mut out: Vec<T> = Vec::new();

        // First page: build the request ourselves (limit/offset + filters +
        // exclude). Subsequent pages follow the server's `next` link, which
        // echoes every original query param — so filters and `exclude` carry
        // forward without re-appending.
        let mut first_params = base_params.clone();
        first_params.push(("limit", page_size.to_string()));
        first_params.push(("offset", "0".to_string()));
        if self.exclude_config_context && endpoint.has_config_context() {
            first_params.push(("exclude", "config_context".to_string()));
        }
        let mut page: Page<T> = self.get(endpoint.path(), &first_params).await?;
        let count = page.count;
        out.extend(page.results);

        // Follow the server's `next` link for further pages. NetBox (Django REST
        // Framework `LimitOffsetPagination`) builds `next` with offset = current
        // + the *capped* limit (min(requested, MAX_PAGE_SIZE)), so following it
        // lands exactly where the next row lives — even when the server caps our
        // requested `limit` below `page_size`. Computing `offset += page_size`
        // ourselves would overshoot the rows the server actually returned and
        // silently skip them (see KNOWN_ISSUES: browse lists skip rows when
        // MAX_PAGE_SIZE < the requested limit).
        while out.len() < count && out.len() < max {
            let Some(next) = page.next.as_deref() else {
                break;
            };
            let (path, owned) = next_request(next)?;
            let params: Vec<(&str, String)> =
                owned.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
            let res = self.send(&path, &params).await?;
            page = Self::decode(res).await?;
            if page.results.is_empty() {
                break;
            }
            out.extend(page.results);
        }

        out.truncate(max);
        Ok(out)
    }
}

/// Split a server-supplied `next` page link — an absolute URL carrying the next
/// page's `limit`/`offset` plus every original filter param — back into a
/// `(path, params)` pair for [`NetBoxClient::send`]. Only the path and query are
/// used; the host is re-resolved against `base_url` in `send`, so a `next` link
/// naming a different host (it shouldn't, on a single NetBox) still targets the
/// configured box. DRF sizes `next`'s offset with the *capped* limit, which is
/// exactly what keeps [`NetBoxClient::list_all`] aligned with the rows the
/// server actually returned.
fn next_request(next: &str) -> Result<(String, Vec<(String, String)>)> {
    let url = reqwest::Url::parse(next).with_context(|| format!("parsing next link {next}"))?;
    let path = url.path().to_string();
    let params: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    Ok((path, params))
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

/// The shared HTTP-429 retry policy for REST and GraphQL. When `res` is a 429 and
/// `attempt` is below the cap, sleep for the server's `Retry-After` (or
/// exponential `backoff`) and return `true` so the caller retries; otherwise
/// return `false`. `what` tags the warn line so REST and GraphQL stay
/// distinguishable in logs. Centralizing this keeps the two request loops from
/// drifting apart on the rate-limit policy.
async fn retry_on_rate_limit(res: &reqwest::Response, attempt: u32, what: &str) -> bool {
    const MAX_RETRIES: u32 = 3;
    if res.status() != reqwest::StatusCode::TOO_MANY_REQUESTS || attempt >= MAX_RETRIES {
        return false;
    }
    let wait = parse_retry_after(
        res.headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
    )
    .unwrap_or_else(|| backoff(attempt));
    tracing::warn!("{what} rate limited (429); retrying in {wait:?}");
    tokio::time::sleep(wait).await;
    true
}

/// Extract a printable reason suffix from a NetBox auth-error body. NetBox returns
/// `{"detail":"Invalid v2 token"}` on a rejected token (401/403), so surface that
/// real cause instead of a generic "permission denied" — the difference between a
/// 5-minute fix (regenerate the token) and an hour of chasing config. Returns
/// `": <reason>"`, or empty when the body has no usable detail.
fn auth_detail(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return String::new();
    }
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_string))
        .unwrap_or_else(|| truncate(body, 200));
    let detail = detail.trim();
    if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    }
}

/// Map a non-success HTTP status to a typed [`NboxError`] so exit codes are
/// consistent no matter which path hit the error: 401→auth, 403→perms, 404→not
/// found, everything else→generic API error. (Note: `get_optional` intercepts
/// 404 earlier as `Ok(None)`; this covers raw 404s on `get`, e.g. `nbox raw`.)
fn status_error(status: reqwest::StatusCode, body: &str) -> NboxError {
    match status {
        reqwest::StatusCode::UNAUTHORIZED => NboxError::Authentication(auth_detail(body)),
        reqwest::StatusCode::FORBIDDEN => NboxError::PermissionDenied(auth_detail(body)),
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

/// Map a non-success status from a write (`PATCH`) to a typed [`NboxError`].
/// Distinct from the read [`status_error`] so the two write-specific outcomes
/// get precise, actionable messages per ADR-0001 §8.
///
/// `412 Precondition Failed` maps to [`NboxError::StalePrecondition`] (the
/// `If-Match` ETag didn't match; the object changed in NetBox; re-run dry-run).
///
/// `400 Bad Request` maps to [`NboxError::WriteValidation`] carrying NetBox's
/// field-level detail, surfaced with field context and no stdout pollution.
///
/// Auth/permission/not-found keep the same stable mapping as reads — a write is
/// still just a REST call, so a 401/403/404 means the same thing it would on a
/// read of the same object.
fn write_status_error(status: reqwest::StatusCode, body: &str) -> NboxError {
    match status {
        reqwest::StatusCode::PRECONDITION_FAILED => NboxError::StalePrecondition(String::new()),
        reqwest::StatusCode::BAD_REQUEST => NboxError::WriteValidation(truncate(body, 500)),
        other => status_error(other, body),
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

    #[tokio::test]
    async fn get_retries_a_429_then_succeeds() {
        use serde_json::json;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // The eventual success (default priority — the fallback).
        Mock::given(method("GET"))
            .and(path("/api/x/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .mount(&server)
            .await;
        // Higher priority, good for exactly one response: a 429 carrying
        // `Retry-After: 0` so the shared retry policy fires immediately (no test
        // sleep). Once exhausted it stops matching and the 200 above serves.
        Mock::given(method("GET"))
            .and(path("/api/x/"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;

        let client = NetBoxClient::new(
            &ProfileConfig {
                url: server.uri(),
                ..Default::default()
            },
            None,
        )
        .unwrap();

        // `send` (via `get`) transparently retries the 429 and returns the 200.
        let body: serde_json::Value = client.get("/api/x/", &[]).await.expect("retry then 200");
        assert_eq!(body["ok"], json!(true));
        // The retry really happened: the server saw two GETs for the same path.
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2,
            "a 429 is retried once, then the request succeeds"
        );
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
    fn auth_detail_extracts_netbox_detail() {
        // NetBox's `{"detail": "..."}` becomes a ": reason" suffix the auth error
        // appends; non-JSON falls back to the body; empty stays empty.
        assert_eq!(
            auth_detail(r#"{"detail":"Invalid v2 token"}"#),
            ": Invalid v2 token"
        );
        assert_eq!(auth_detail("Forbidden"), ": Forbidden");
        assert_eq!(auth_detail("   "), "");
        assert_eq!(auth_detail(""), "");
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

    #[tokio::test]
    async fn list_all_grows_the_page_to_max_so_a_large_fetch_is_one_round_trip() {
        // With the default page_size (100) and a 500-row `max`, list_all must grow
        // the page to 500 (capped at MAX_PAGE_SIZE) and fetch all rows in ONE
        // request — not page 100-at-a-time over five sequential round trips. The
        // mock matches ONLY `limit=500`, so an un-grown request (limit=100) gets
        // no reply and the call fails; it returns the full 500-row set, so a
        // single-page client breaks after one request (received_requests == 1).
        use serde_json::json;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let rows: Vec<_> = (0..500)
            .map(|i| json!({ "id": i, "name": format!("d{i}") }))
            .collect();
        Mock::given(method("GET"))
            .and(path("/api/dcim/devices/"))
            .and(query_param("limit", "500"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 500, "next": null, "previous": null, "results": rows
            })))
            .mount(&server)
            .await;

        let client = NetBoxClient::new(
            &ProfileConfig {
                url: server.uri(),
                ..Default::default()
            },
            None,
        )
        .unwrap();

        let rows: Vec<serde_json::Value> = client
            .list_all(Endpoint::Devices, vec![], 500)
            .await
            .expect("list_all");
        assert_eq!(rows.len(), 500);
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "a 500-row fetch at max=500 is one round trip"
        );
    }

    #[tokio::test]
    async fn list_all_follows_the_server_next_link_across_pages() {
        // When `max` (2500) exceeds MAX_PAGE_SIZE the first page caps at 1000 and
        // the fetch pages 1000/1000/500 — following the server's `next` link each
        // time rather than computing offsets. Each mock matches its page's
        // `offset` and `limit=1000` exactly, so a request that strays from the
        // `next` link gets no reply and the call fails. Three aligned round trips
        // prove the `next` link carried the right offset (1000, then 2000).
        use serde_json::json;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let next_url = |offset: usize| {
            format!(
                "{}/api/dcim/devices/?limit=1000&offset={offset}",
                server.uri()
            )
        };
        let page = |offset: usize, n: usize, next: Option<String>| {
            let rows: Vec<_> = (0..n)
                .map(|i| json!({ "id": offset + i, "name": format!("d{}", offset + i) }))
                .collect();
            Mock::given(method("GET"))
                .and(path("/api/dcim/devices/"))
                .and(query_param("limit", "1000"))
                .and(query_param("offset", offset.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 2500, "next": next, "previous": null, "results": rows
                })))
                .mount(&server)
        };
        page(0, 1000, Some(next_url(1000))).await;
        page(1000, 1000, Some(next_url(2000))).await;
        page(2000, 500, None).await;

        let client = NetBoxClient::new(
            &ProfileConfig {
                url: server.uri(),
                ..Default::default()
            },
            None,
        )
        .unwrap();

        let rows: Vec<serde_json::Value> = client
            .list_all(Endpoint::Devices, vec![], 2500)
            .await
            .expect("list_all");
        assert_eq!(rows.len(), 2500);
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            3,
            "three aligned round trips following the server next link"
        );
    }
}
