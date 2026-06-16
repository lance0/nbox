//! The NetBox REST client.
//!
//! REST is the primary integration path. The client carries auth, paging, and
//! the per-profile config-context optimization. Tokens are never logged — debug
//! logging emits a redacted scheme marker only (see [`NetBoxClient::masked_authorization`]).

use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Url;
use reqwest::header::{ACCEPT, AUTHORIZATION};
use serde::de::DeserializeOwned;

use crate::config::ProfileConfig;
use crate::netbox::auth::AuthScheme;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::pagination::Page;

/// An HTTP client bound to a single NetBox instance/profile.
#[derive(Clone)]
pub struct NetBoxClient {
    base_url: Url,
    token: Option<String>,
    auth_scheme: AuthScheme,
    http: reqwest::Client,
    page_size: usize,
    exclude_config_context: bool,
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

        Ok(Self {
            base_url,
            token,
            auth_scheme: profile.auth_scheme.unwrap_or_default(),
            http,
            page_size: profile.page_size.unwrap_or(100),
            exclude_config_context: profile.exclude_config_context.unwrap_or(true),
        })
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

    /// Issue an authenticated GET, returning the raw response.
    async fn send(&self, path: &str, params: &[(&str, String)]) -> Result<reqwest::Response> {
        let url = self.url_for(path)?;

        match self.masked_authorization() {
            Some(auth) => tracing::debug!(url = %url, auth = %auth, "GET"),
            None => tracing::debug!(url = %url, "GET (unauthenticated)"),
        }

        let mut req = self
            .http
            .get(url)
            .query(params)
            .header(ACCEPT, "application/json");

        if let Some(token) = &self.token {
            req = req.header(AUTHORIZATION, self.auth_scheme.header_value(token));
        }

        req.send().await.context("sending request to NetBox")
    }

    /// Decode a successful response, or turn a non-2xx status into an error.
    async fn decode<T: DeserializeOwned>(res: reqwest::Response) -> Result<T> {
        let status = res.status();
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            bail!(
                "NetBox API request failed: {status}: {}",
                truncate(&body, 500)
            );
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
            offset += got;
        }

        out.truncate(max);
        Ok(out)
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
}
