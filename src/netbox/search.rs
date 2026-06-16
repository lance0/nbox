//! Normalized multi-endpoint search.
//!
//! There is no universal NetBox search endpoint, so `nbox search` fans out across
//! several object types in parallel using each endpoint's built-in `q=`
//! quick-search, then merges, ranks, dedups, and truncates.

use std::collections::HashSet;

use anyhow::Result;
use serde::Serialize;

use crate::netbox::client::NetBoxClient;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::models::dcim::{Device, Site};
use crate::netbox::models::ipam::{IpAddress, Prefix, Vlan};
use crate::netbox::pagination::Page;
use crate::util::format::api_to_web_url;

/// The kind of object a [`SearchResult`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Device,
    Site,
    IpAddress,
    Prefix,
    Vlan,
}

impl ObjectKind {
    /// Short label for plain output.
    pub fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Device => "device",
            ObjectKind::Site => "site",
            ObjectKind::IpAddress => "ip",
            ObjectKind::Prefix => "prefix",
            ObjectKind::Vlan => "vlan",
        }
    }
}

/// Structured filters for a search, mapped to NetBox query params (by slug/value).
#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub status: Option<String>,
    pub site: Option<String>,
    pub tenant: Option<String>,
    pub role: Option<String>,
}

impl SearchFilters {
    /// Build the filter params for an endpoint that supports `supported` keys.
    /// Returns `None` if any *active* filter is unsupported here — the caller
    /// then skips that endpoint rather than send an ignored param (NetBox
    /// silently ignores unknown filters and would return everything).
    fn params_for(&self, supported: &[&str]) -> Option<Vec<(&'static str, String)>> {
        let active: [(&'static str, &Option<String>); 4] = [
            ("status", &self.status),
            ("site", &self.site),
            ("tenant", &self.tenant),
            ("role", &self.role),
        ];
        let mut params = Vec::new();
        for (key, value) in active {
            if let Some(v) = value {
                if !supported.contains(&key) {
                    return None;
                }
                params.push((key, v.clone()));
            }
        }
        Some(params)
    }
}

/// A search request.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub limit: usize,
    pub filters: SearchFilters,
}

/// A normalized search hit.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub kind: ObjectKind,
    pub id: u64,
    pub display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    pub url: String,
    pub score: i32,
}

/// Rank a candidate label against the query: exact > prefix > contains > other.
fn score_match(query: &str, candidate: &str) -> i32 {
    let q = query.to_lowercase();
    let c = candidate.to_lowercase();
    if c == q {
        100
    } else if c.starts_with(&q) {
        50
    } else if c.contains(&q) {
        25
    } else {
        // The server's `q` matched some other field (serial, description, …).
        10
    }
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|x| !x.is_empty())
}

/// Build the `q=` query plus any applicable filters for an endpoint, or `None`
/// to skip the endpoint (an active filter it can't satisfy).
fn endpoint_params(
    q: &str,
    filters: &SearchFilters,
    supported: &[&str],
) -> Option<Vec<(&'static str, String)>> {
    let extra = filters.params_for(supported)?;
    let mut params = vec![("q", q.to_string())];
    params.extend(extra);
    Some(params)
}

impl NetBoxClient {
    /// Search across devices, sites, IPs, prefixes, and VLANs in parallel.
    pub async fn search(&self, req: SearchRequest) -> Result<Vec<SearchResult>> {
        let q = req.query.trim().to_string();
        let f = &req.filters;

        let (devices, sites, ips, prefixes, vlans) = tokio::join!(
            self.search_devices(&q, f),
            self.search_sites(&q, f),
            self.search_ips(&q, f),
            self.search_prefixes(&q, f),
            self.search_vlans(&q, f),
        );

        let mut merged = Vec::new();
        let mut last_err = None;
        for branch in [devices, sites, ips, prefixes, vlans] {
            match branch {
                Ok(mut items) => merged.append(&mut items),
                Err(e) => {
                    tracing::warn!("search branch failed: {e:#}");
                    last_err = Some(e);
                }
            }
        }

        // If every branch failed (e.g. auth/connection), surface the error
        // instead of a misleading "no results".
        if merged.is_empty()
            && let Some(e) = last_err
        {
            return Err(e);
        }

        merged.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.display.cmp(&b.display))
        });
        let mut seen = HashSet::new();
        merged.retain(|r| seen.insert((r.kind, r.id)));
        merged.truncate(req.limit);
        Ok(merged)
    }

    async fn search_devices(&self, q: &str, f: &SearchFilters) -> Result<Vec<SearchResult>> {
        let Some(params) = endpoint_params(q, f, &["status", "site", "tenant", "role"]) else {
            return Ok(Vec::new());
        };
        let page: Page<Device> = self.list(Endpoint::Devices, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|d| SearchResult {
                kind: ObjectKind::Device,
                id: d.id,
                score: score_match(q, &d.name),
                subtitle: d.site.as_ref().map(|s| s.label()),
                url: api_to_web_url(&d.url),
                display: d.name,
            })
            .collect())
    }

    async fn search_sites(&self, q: &str, f: &SearchFilters) -> Result<Vec<SearchResult>> {
        let Some(params) = endpoint_params(q, f, &["status", "tenant"]) else {
            return Ok(Vec::new());
        };
        let page: Page<Site> = self.list(Endpoint::Sites, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|s| SearchResult {
                kind: ObjectKind::Site,
                id: s.id,
                score: score_match(q, &s.name),
                subtitle: Some(s.slug),
                url: api_to_web_url(&s.url),
                display: s.name,
            })
            .collect())
    }

    async fn search_ips(&self, q: &str, f: &SearchFilters) -> Result<Vec<SearchResult>> {
        let Some(params) = endpoint_params(q, f, &["status", "tenant", "role"]) else {
            return Ok(Vec::new());
        };
        let page: Page<IpAddress> = self.list(Endpoint::IpAddresses, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|ip| SearchResult {
                kind: ObjectKind::IpAddress,
                id: ip.id,
                score: score_match(q, &ip.address),
                subtitle: non_empty(ip.dns_name),
                url: api_to_web_url(&ip.url),
                display: ip.address,
            })
            .collect())
    }

    async fn search_prefixes(&self, q: &str, f: &SearchFilters) -> Result<Vec<SearchResult>> {
        // `site` is intentionally omitted: prefix site-filtering is ambiguous
        // under the 4.2+ polymorphic scope, so we skip prefixes when `--site`
        // is set rather than risk an ignored filter.
        let Some(params) = endpoint_params(q, f, &["status", "tenant", "role"]) else {
            return Ok(Vec::new());
        };
        let page: Page<Prefix> = self.list(Endpoint::Prefixes, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|p| SearchResult {
                kind: ObjectKind::Prefix,
                id: p.id,
                score: score_match(q, &p.prefix),
                subtitle: p.scope.as_ref().map(|s| s.label()),
                url: api_to_web_url(&p.url),
                display: p.prefix,
            })
            .collect())
    }

    async fn search_vlans(&self, q: &str, f: &SearchFilters) -> Result<Vec<SearchResult>> {
        let Some(params) = endpoint_params(q, f, &["status", "site", "tenant", "role"]) else {
            return Ok(Vec::new());
        };
        let page: Page<Vlan> = self.list(Endpoint::Vlans, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|v| {
                let display = format!("{} {}", v.vid, v.name);
                SearchResult {
                    kind: ObjectKind::Vlan,
                    id: v.id,
                    score: score_match(q, &display),
                    subtitle: v.site.as_ref().or(v.group.as_ref()).map(|b| b.label()),
                    url: api_to_web_url(&v.url),
                    display,
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoring_orders_exact_prefix_contains() {
        assert!(score_match("edge01", "edge01") > score_match("edge", "edge01"));
        assert!(score_match("edge", "edge01") > score_match("dge", "edge01"));
        assert!(score_match("dge", "edge01") > score_match("zzz", "edge01"));
    }

    #[test]
    fn filters_apply_to_supported_endpoints_and_skip_others() {
        let f = SearchFilters {
            site: Some("dc1".into()),
            status: Some("active".into()),
            ..Default::default()
        };
        // Devices support both → params built (q + status + site).
        let dev = endpoint_params("edge", &f, &["status", "site", "tenant", "role"]).unwrap();
        assert!(dev.contains(&("q", "edge".to_string())));
        assert!(dev.contains(&("site", "dc1".to_string())));
        assert!(dev.contains(&("status", "active".to_string())));
        // IP addresses don't support `site` → endpoint skipped entirely.
        assert!(endpoint_params("edge", &f, &["status", "tenant", "role"]).is_none());
    }

    #[test]
    fn no_filters_just_passes_q() {
        let f = SearchFilters::default();
        let p = endpoint_params("edge", &f, &["status"]).unwrap();
        assert_eq!(p, vec![("q", "edge".to_string())]);
    }
}
