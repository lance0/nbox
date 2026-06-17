//! Normalized multi-endpoint search.
//!
//! There is no universal NetBox search endpoint, so `nbox search` fans out across
//! several object types in parallel using each endpoint's built-in `q=`
//! quick-search, then merges, ranks, dedups, and truncates.

use std::collections::HashSet;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Serialize;

use crate::netbox::client::NetBoxClient;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::models::circuits::Circuit;
use crate::netbox::models::dcim::{Device, Site};
use crate::netbox::models::ipam::{Aggregate, Asn, IpAddress, IpRange, Prefix, Vlan};
use crate::netbox::pagination::Page;
use crate::util::format::api_to_web_url;

/// The kind of object a [`SearchResult`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Device,
    Site,
    IpAddress,
    Prefix,
    Vlan,
    Circuit,
    Aggregate,
    Asn,
    IpRange,
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
            ObjectKind::Circuit => "circuit",
            ObjectKind::Aggregate => "aggregate",
            ObjectKind::Asn => "asn",
            ObjectKind::IpRange => "ip-range",
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
    pub tag: Option<String>,
}

impl SearchFilters {
    /// Build the filter params for an endpoint that supports `supported` keys.
    /// Returns `None` if any *active* filter is unsupported here — the caller
    /// then skips that endpoint rather than send an ignored param (NetBox
    /// silently ignores unknown filters and would return everything).
    fn params_for(&self, supported: &[&str]) -> Option<Vec<(&'static str, String)>> {
        let active: [(&'static str, &Option<String>); 5] = [
            ("status", &self.status),
            ("site", &self.site),
            ("tenant", &self.tenant),
            ("role", &self.role),
            ("tag", &self.tag),
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

/// The outcome of a search: ranked results plus any per-endpoint failures.
///
/// `errors` is non-empty when some endpoints succeeded and others failed — a
/// *partial* result. Callers decide whether to fail closed or surface it. When
/// every endpoint fails (and there are no results), [`NetBoxClient::search`]
/// returns the underlying `Err` instead, preserving its typed exit code.
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    pub results: Vec<SearchResult>,
    pub errors: Vec<String>,
}

/// A normalized search hit.
#[derive(Debug, Clone, Serialize, JsonSchema)]
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
    /// Search across devices, sites, IPs, prefixes, VLANs, circuits,
    /// aggregates, ASNs, and IP ranges in parallel.
    ///
    /// Returns ranked results plus a list of endpoints that failed. If every
    /// endpoint fails and nothing matched, returns the underlying `Err` (so a
    /// bad token surfaces as an auth error, not an empty result set). A *partial*
    /// failure — some endpoints down, others returning data — is reported via
    /// [`SearchOutcome::errors`] for the caller to act on.
    pub async fn search(&self, req: SearchRequest) -> Result<SearchOutcome> {
        let q = req.query.trim().to_string();
        let f = &req.filters;

        let (devices, sites, ips, prefixes, vlans, circuits, aggregates, asns, ip_ranges) = tokio::join!(
            self.search_devices(&q, f),
            self.search_sites(&q, f),
            self.search_ips(&q, f),
            self.search_prefixes(&q, f),
            self.search_vlans(&q, f),
            self.search_circuits(&q, f),
            self.search_aggregates(&q, f),
            self.search_asns(&q, f),
            self.search_ip_ranges(&q, f),
        );

        let mut merged = Vec::new();
        let mut errors = Vec::new();
        let mut last_err = None;
        let branches = [
            ("devices", devices),
            ("sites", sites),
            ("ips", ips),
            ("prefixes", prefixes),
            ("vlans", vlans),
            ("circuits", circuits),
            ("aggregates", aggregates),
            ("asns", asns),
            ("ip-ranges", ip_ranges),
        ];
        for (name, branch) in branches {
            match branch {
                Ok(mut items) => merged.append(&mut items),
                Err(e) => {
                    tracing::warn!("search branch '{name}' failed: {e:#}");
                    errors.push(format!("{name}: {e:#}"));
                    last_err = Some(e);
                }
            }
        }

        // Nothing came back and something failed → surface the typed error
        // rather than a misleading "no results".
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
        Ok(SearchOutcome {
            results: merged,
            errors,
        })
    }

    async fn search_devices(&self, q: &str, f: &SearchFilters) -> Result<Vec<SearchResult>> {
        let Some(params) = endpoint_params(q, f, &["status", "site", "tenant", "role", "tag"])
        else {
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
                subtitle: d
                    .site
                    .as_ref()
                    .map(super::models::common::BriefObject::label),
                url: api_to_web_url(&d.url),
                display: d.name,
            })
            .collect())
    }

    async fn search_sites(&self, q: &str, f: &SearchFilters) -> Result<Vec<SearchResult>> {
        let Some(params) = endpoint_params(q, f, &["status", "tenant", "tag"]) else {
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
        let Some(params) = endpoint_params(q, f, &["status", "tenant", "role", "tag"]) else {
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
        let Some(params) = endpoint_params(q, f, &["status", "tenant", "role", "tag"]) else {
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
                subtitle: p
                    .scope
                    .as_ref()
                    .map(super::models::common::BriefObject::label),
                url: api_to_web_url(&p.url),
                display: p.prefix,
            })
            .collect())
    }

    async fn search_vlans(&self, q: &str, f: &SearchFilters) -> Result<Vec<SearchResult>> {
        let Some(params) = endpoint_params(q, f, &["status", "site", "tenant", "role", "tag"])
        else {
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
                    subtitle: v
                        .site
                        .as_ref()
                        .or(v.group.as_ref())
                        .map(super::models::common::BriefObject::label),
                    url: api_to_web_url(&v.url),
                    display,
                }
            })
            .collect())
    }

    async fn search_circuits(&self, q: &str, f: &SearchFilters) -> Result<Vec<SearchResult>> {
        let Some(params) = endpoint_params(q, f, &["status", "tenant", "tag"]) else {
            return Ok(Vec::new());
        };
        let page: Page<Circuit> = self.list(Endpoint::Circuits, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|c| SearchResult {
                kind: ObjectKind::Circuit,
                id: c.id,
                score: score_match(q, &c.cid),
                subtitle: c
                    .provider
                    .as_ref()
                    .map(super::models::common::BriefObject::label),
                url: api_to_web_url(&c.url),
                display: c.cid,
            })
            .collect())
    }

    async fn search_aggregates(&self, q: &str, f: &SearchFilters) -> Result<Vec<SearchResult>> {
        let Some(params) = endpoint_params(q, f, &["tenant", "tag"]) else {
            return Ok(Vec::new());
        };
        let page: Page<Aggregate> = self.list(Endpoint::Aggregates, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|a| SearchResult {
                kind: ObjectKind::Aggregate,
                id: a.id,
                score: score_match(q, &a.prefix),
                subtitle: a
                    .rir
                    .as_ref()
                    .map(super::models::common::BriefObject::label),
                url: api_to_web_url(&a.url),
                display: a.prefix,
            })
            .collect())
    }

    async fn search_asns(&self, q: &str, f: &SearchFilters) -> Result<Vec<SearchResult>> {
        let Some(mut params) = endpoint_params(q, f, &["tenant", "tag"]) else {
            return Ok(Vec::new());
        };
        // A bare AS number won't be matched by the `q` quick-search (it scans
        // text fields, not the numeric `asn`). When the query is purely numeric,
        // match the `asn` field directly instead of `q` (NetBox ANDs filters, so
        // keeping both would over-filter to nothing).
        if let Ok(asn) = q.parse::<u32>() {
            params.retain(|(k, _)| *k != "q");
            params.push(("asn", asn.to_string()));
        }
        let page: Page<Asn> = self.list(Endpoint::Asns, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|a| {
                let display = format!("AS{}", a.asn);
                SearchResult {
                    kind: ObjectKind::Asn,
                    id: a.id,
                    score: score_match(q, &a.asn.to_string()),
                    subtitle: a
                        .rir
                        .as_ref()
                        .map(super::models::common::BriefObject::label),
                    url: api_to_web_url(&a.url),
                    display,
                }
            })
            .collect())
    }

    async fn search_ip_ranges(&self, q: &str, f: &SearchFilters) -> Result<Vec<SearchResult>> {
        let Some(params) = endpoint_params(q, f, &["status", "tenant", "role", "tag"]) else {
            return Ok(Vec::new());
        };
        let page: Page<IpRange> = self.list(Endpoint::IpRanges, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|r| {
                let display = format!("{}-{}", r.start_address, r.end_address);
                SearchResult {
                    kind: ObjectKind::IpRange,
                    id: r.id,
                    score: score_match(q, &display),
                    subtitle: r
                        .vrf
                        .as_ref()
                        .or(r.role.as_ref())
                        .map(super::models::common::BriefObject::label),
                    url: api_to_web_url(&r.url),
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
    fn tag_filter_is_passed_to_supported_endpoints() {
        let f = SearchFilters {
            tag: Some("critical".into()),
            ..Default::default()
        };
        let p = endpoint_params("edge", &f, &["status", "tag"]).unwrap();
        assert!(p.contains(&("tag", "critical".to_string())));
        // An endpoint that doesn't list `tag` is skipped rather than ignoring it.
        assert!(endpoint_params("edge", &f, &["status"]).is_none());
    }

    #[test]
    fn no_filters_just_passes_q() {
        let f = SearchFilters::default();
        let p = endpoint_params("edge", &f, &["status"]).unwrap();
        assert_eq!(p, vec![("q", "edge".to_string())]);
    }

    #[test]
    fn object_kind_labels_cover_new_kinds() {
        assert_eq!(ObjectKind::Circuit.as_str(), "circuit");
        assert_eq!(ObjectKind::Aggregate.as_str(), "aggregate");
        assert_eq!(ObjectKind::Asn.as_str(), "asn");
        assert_eq!(ObjectKind::IpRange.as_str(), "ip-range");
    }
}
