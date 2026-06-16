//! Normalized multi-endpoint search.
//!
//! There is no universal NetBox search endpoint, so `nbx search` fans out across
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

/// A search request.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub limit: usize,
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

impl NetBoxClient {
    /// Search across devices, sites, IPs, prefixes, and VLANs in parallel.
    pub async fn search(&self, req: SearchRequest) -> Result<Vec<SearchResult>> {
        let q = req.query.trim().to_string();

        let (devices, sites, ips, prefixes, vlans) = tokio::join!(
            self.search_devices(&q),
            self.search_sites(&q),
            self.search_ips(&q),
            self.search_prefixes(&q),
            self.search_vlans(&q),
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

    async fn search_devices(&self, q: &str) -> Result<Vec<SearchResult>> {
        let page: Page<Device> = self
            .list(Endpoint::Devices, vec![("q", q.to_string())])
            .await?;
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

    async fn search_sites(&self, q: &str) -> Result<Vec<SearchResult>> {
        let page: Page<Site> = self
            .list(Endpoint::Sites, vec![("q", q.to_string())])
            .await?;
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

    async fn search_ips(&self, q: &str) -> Result<Vec<SearchResult>> {
        let page: Page<IpAddress> = self
            .list(Endpoint::IpAddresses, vec![("q", q.to_string())])
            .await?;
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

    async fn search_prefixes(&self, q: &str) -> Result<Vec<SearchResult>> {
        let page: Page<Prefix> = self
            .list(Endpoint::Prefixes, vec![("q", q.to_string())])
            .await?;
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

    async fn search_vlans(&self, q: &str) -> Result<Vec<SearchResult>> {
        let page: Page<Vlan> = self
            .list(Endpoint::Vlans, vec![("q", q.to_string())])
            .await?;
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
}
