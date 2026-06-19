//! Normalized multi-endpoint search.
//!
//! There is no universal NetBox search endpoint, so `nbox search` fans out across
//! several object types in parallel using each endpoint's built-in `q=`
//! quick-search, then merges, ranks, dedups, and truncates.

use std::collections::HashSet;

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::config::BackendKind;
use crate::netbox::client::{MAX_PAGE_SIZE, NetBoxClient};
use crate::netbox::endpoints::Endpoint;
use crate::netbox::graphql::GraphqlCapabilities;
use crate::netbox::models::circuits::{Circuit, Provider};
use crate::netbox::models::dcim::{Device, Site};
use crate::netbox::models::ipam::{Aggregate, Asn, IpAddress, IpRange, Prefix, Vlan};
use crate::netbox::models::tenancy::{Contact, Tenant};
use crate::netbox::models::virtualization::{Cluster, VirtualMachine};
use crate::netbox::pagination::Page;
use crate::util::format::api_to_web_url;

/// The kind of object a [`SearchResult`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
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
    Tenant,
    Contact,
    Provider,
    Vm,
    Cluster,
    /// A rack. Not (yet) a search result — it's drill-only: openable in the TUI
    /// and a cross-navigation target (e.g. a device's rack). Promoting it to a
    /// searchable kind is tracked in the roadmap. Kept last so the search-result
    /// ordering of the existing kinds is undisturbed.
    Rack,
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
            ObjectKind::Tenant => "tenant",
            ObjectKind::Contact => "contact",
            ObjectKind::Provider => "provider",
            ObjectKind::Vm => "vm",
            ObjectKind::Cluster => "cluster",
            ObjectKind::Rack => "rack",
        }
    }
}

/// Structured filters for a search, mapped to NetBox query params (by slug/value).
///
/// `site`/`region`/`site_group`/`location` are *scope* filters: NetBox 4.2's
/// prefix `scope` is a single polymorphic type+id, so at most one of them may be
/// set at a time (enforced in [`NetBoxClient::search`]). All four are resolved to
/// a numeric id up front and handled out-of-band per endpoint — as `scope_type`+
/// `scope_id` on the polymorphic endpoints (prefixes, clusters) and as
/// `site_id`/`region_id`/`site_group_id`/`location_id` on the rest — never through
/// the plain-value allowlist below (the plain `?site=` param wants a slug, so an
/// id or display name would silently match nothing).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilters {
    pub status: Option<String>,
    pub site: Option<String>,
    pub region: Option<String>,
    pub site_group: Option<String>,
    pub location: Option<String>,
    pub tenant: Option<String>,
    pub role: Option<String>,
    pub tag: Option<String>,
    /// VRF reference (id | rd | name). Resolved once to a numeric id and applied
    /// as `vrf_id=` on the VRF-capable endpoints (IPs, prefixes) only; endpoints
    /// that carry no VRF are skipped for this filter. Orthogonal to the scope
    /// filters above — both may be set at once.
    pub vrf: Option<String>,
}

/// A scope filter resolved to a NetBox content type + numeric id. Exactly one
/// scope is active at a time (mutual exclusion is enforced in `resolve_scope`).
#[derive(Debug, Clone)]
struct ResolvedScope {
    /// The prefix `scope_type` content type, e.g. `dcim.region`.
    content_type: &'static str,
    /// The resolved object id.
    id: u64,
}

impl SearchFilters {
    /// Build the filter params for an endpoint that supports `supported` keys.
    /// Returns `None` if any *active* filter is unsupported here — the caller
    /// then skips that endpoint rather than send an ignored param (NetBox
    /// silently ignores unknown filters and would return everything).
    ///
    /// Scope filters (`site`/`region`/`site_group`/`location`) are *not* included
    /// here — they're resolved to a numeric id once and applied out-of-band per
    /// endpoint (as `site_id`/`region_id`/… or `scope_type`+`scope_id`). The plain
    /// `?site=` param expects a *slug*, so a `--site` given as an id or display
    /// name would silently match nothing; the resolved id avoids that.
    fn params_for(&self, supported: &[&str]) -> Option<Vec<(&'static str, String)>> {
        let active: [(&'static str, &Option<String>); 4] = [
            ("status", &self.status),
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResult {
    pub kind: ObjectKind,
    pub id: u64,
    pub display: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

/// Whether an endpoint should skip itself because an id-based scope
/// (`region`/`site-group`/`location`) is active and the endpoint has no clean
/// filter for it. `--site` (a `dcim.site` scope) is honored by the endpoints that
/// support it via the resolved `site_id`, so it does NOT trigger a skip here —
/// only the three id-based scopes do. This keeps a region/site-group/location
/// filter from silently returning an unfiltered endpoint's full result set.
fn skip_for_id_scope(scope: Option<&ResolvedScope>) -> bool {
    matches!(
        scope.map(|s| s.content_type),
        Some("dcim.region" | "dcim.sitegroup" | "dcim.location")
    )
}

/// Whether an endpoint should skip itself because *any* scope is active and the
/// endpoint can honor no scope at all — including `--site`. Used by the endpoints
/// that carry no site/region/site-group/location filter (IPs, circuits,
/// aggregates, ASNs, IP ranges, tenants, contacts, providers, and the site search
/// itself), so an active scope skips them rather than returning an unfiltered
/// result set. Endpoints that honor `--site` (devices/VLANs/VMs via `site_id`)
/// use [`skip_for_id_scope`] instead, and the polymorphic endpoints
/// (prefixes/clusters) honor every scope out-of-band.
fn skip_for_any_scope(scope: Option<&ResolvedScope>) -> bool {
    scope.is_some()
}

/// A typed not-found error for an unresolved scope reference, e.g. a `--region`
/// that matches nothing. Exit 4, with an actionable hint — mirrors the original
/// `--site` not-found message.
fn not_found(noun: &str, reference: &str) -> anyhow::Error {
    crate::error::NboxError::NotFound(format!(
        "no {noun} matched \"{reference}\"\n\nTry:\n  nbox search {reference}"
    ))
    .into()
}

/// The location label for a VLAN search result's subtitle, following the same
/// `scope → site → group` precedence as the detail view (`VlanView::build` and
/// `query::vlan_scope_label`). The polymorphic `scope` wins (NetBox 4.2+),
/// falling back to a directly assigned `site`, then the VLAN `group`. Returns
/// just the scope object's label (not the disambiguation form), so the search
/// subtitle and the detail view's location stay consistent.
fn vlan_subtitle(v: &Vlan) -> Option<String> {
    use super::models::common::BriefObject;
    v.scope
        .as_ref()
        .or(v.site.as_ref())
        .or(v.group.as_ref())
        .map(BriefObject::label)
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
    /// aggregates, ASNs, IP ranges, tenants, contacts, providers, virtual
    /// machines, and clusters in parallel.
    ///
    /// Returns ranked results plus a list of endpoints that failed. If every
    /// endpoint fails and nothing matched, returns the underlying `Err` (so a
    /// bad token surfaces as an auth error, not an empty result set). A *partial*
    /// failure — some endpoints down, others returning data — is reported via
    /// [`SearchOutcome::errors`] for the caller to act on.
    pub async fn search(&self, req: SearchRequest) -> Result<SearchOutcome> {
        if self.backend() == BackendKind::Graphql {
            // Keep the large GraphQL fan-out future boxed at this public entry
            // point so spawned call sites can await `search()` normally.
            return Box::pin(self.search_graphql(req)).await;
        }

        let q = req.query.trim().to_string();
        let f = &req.filters;

        // Resolve the (single) scope filter to a content type + numeric id once,
        // up front. NetBox 4.2 replaced the prefix `site` FK with a polymorphic
        // `scope` (a single type+id), so a plain `?site=`/`?region=`/… is a dead
        // filter on prefixes — they need `scope_type=<ct>` + `scope_id=<id>`. An
        // unknown ref is a hard not-found error (exit 4) so search fails loudly
        // rather than quietly returning nothing. Scope is an *exact* match: each
        // flag filters by its own scope only — no hierarchy/descendant semantics.
        let scope = self.resolve_scope(f).await?;

        // Resolve the (optional) `--vrf` reference (id | rd | name) to a numeric
        // id once, up front. An unknown VRF is a hard not-found error (exit 4) so
        // search fails loudly rather than quietly returning nothing — matching the
        // scope-filter behavior. The resolved id is applied as `vrf_id=` on the
        // VRF-capable endpoints (IPs, prefixes); the rest skip the vrf filter.
        // `--vrf` is orthogonal to the scope filters: both may be active at once.
        let vrf_id = self.resolve_vrf(f).await?;

        let (
            devices,
            sites,
            ips,
            prefixes,
            vlans,
            circuits,
            aggregates,
            asns,
            ip_ranges,
            tenants,
            contacts,
            providers,
            vms,
            clusters,
        ) = tokio::join!(
            self.search_devices(&q, f, scope.as_ref()),
            self.search_sites(&q, f, scope.as_ref()),
            self.search_ips(&q, f, scope.as_ref(), vrf_id),
            self.search_prefixes(&q, f, scope.as_ref(), vrf_id),
            self.search_vlans(&q, f, scope.as_ref()),
            self.search_circuits(&q, f, scope.as_ref()),
            self.search_aggregates(&q, f, scope.as_ref()),
            self.search_asns(&q, f, scope.as_ref()),
            self.search_ip_ranges(&q, f, scope.as_ref()),
            self.search_tenants(&q, f, scope.as_ref()),
            self.search_contacts(&q, f, scope.as_ref()),
            self.search_providers(&q, f, scope.as_ref()),
            self.search_vms(&q, f, scope.as_ref()),
            self.search_clusters(&q, f, scope.as_ref()),
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
            ("tenants", tenants),
            ("contacts", contacts),
            ("providers", providers),
            ("vms", vms),
            ("clusters", clusters),
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

    async fn search_graphql(&self, req: SearchRequest) -> Result<SearchOutcome> {
        let q = req.query.trim().to_string();
        let f = &req.filters;
        let scope = self.resolve_scope(f).await?;
        let vrf_id = self.resolve_vrf(f).await?;
        let caps = self.graphql_capabilities().await?;

        let (
            devices,
            sites,
            ips,
            prefixes,
            vlans,
            circuits,
            aggregates,
            asns,
            ip_ranges,
            tenants,
            contacts,
            providers,
            vms,
            clusters,
        ) = tokio::join!(
            self.gql_search_devices(&caps, &q, f, scope.as_ref(), req.limit),
            self.gql_search_sites(&caps, &q, f, scope.as_ref(), req.limit),
            self.gql_search_ips(&caps, &q, f, scope.as_ref(), vrf_id, req.limit),
            self.gql_search_prefixes(&caps, &q, f, scope.as_ref(), vrf_id, req.limit),
            self.gql_search_vlans(&caps, &q, f, scope.as_ref(), req.limit),
            self.gql_search_circuits(&caps, &q, f, scope.as_ref(), req.limit),
            self.gql_search_aggregates(&caps, &q, f, scope.as_ref(), req.limit),
            self.gql_search_asns(&caps, &q, f, scope.as_ref(), req.limit),
            self.gql_search_ip_ranges(&caps, &q, f, scope.as_ref(), req.limit),
            self.gql_search_tenants(&caps, &q, f, scope.as_ref(), req.limit),
            self.gql_search_contacts(&caps, &q, f, scope.as_ref(), req.limit),
            self.gql_search_providers(&caps, &q, f, scope.as_ref(), req.limit),
            self.gql_search_vms(&caps, &q, f, scope.as_ref(), req.limit),
            self.gql_search_clusters(&caps, &q, f, scope.as_ref(), req.limit),
        );

        let mut merged = Vec::new();
        let mut errors = Vec::new();
        let mut last_err = None;
        for (name, branch) in [
            ("devices", devices),
            ("sites", sites),
            ("ips", ips),
            ("prefixes", prefixes),
            ("vlans", vlans),
            ("circuits", circuits),
            ("aggregates", aggregates),
            ("asns", asns),
            ("ip-ranges", ip_ranges),
            ("tenants", tenants),
            ("contacts", contacts),
            ("providers", providers),
            ("vms", vms),
            ("clusters", clusters),
        ] {
            match branch {
                Ok(mut items) => merged.append(&mut items),
                Err(e) => {
                    tracing::warn!("GraphQL search branch '{name}' failed: {e:#}");
                    errors.push(format!("{name}: {e:#}"));
                    last_err = Some(e);
                }
            }
        }

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

    /// Resolve the (at most one) active scope filter to a content type + id.
    ///
    /// `--site`/`--region`/`--site-group`/`--location` are mutually exclusive: the
    /// NetBox prefix `scope` is a single type+id, so combining them is a usage
    /// error (exit 2). The single active flag is resolved via its `*_by_ref`
    /// helper; an unknown ref is a not-found error (exit 4).
    async fn resolve_scope(&self, f: &SearchFilters) -> Result<Option<ResolvedScope>> {
        let active: Vec<&'static str> = [
            ("--site", f.site.is_some()),
            ("--region", f.region.is_some()),
            ("--site-group", f.site_group.is_some()),
            ("--location", f.location.is_some()),
        ]
        .into_iter()
        .filter_map(|(flag, set)| set.then_some(flag))
        .collect();

        if active.len() > 1 {
            return Err(crate::error::NboxError::Usage(format!(
                "scope filters are mutually exclusive — pass only one of {}\n\nNetBox prefix scope is a single type+id; combine them and the result is undefined.",
                active.join(", ")
            ))
            .into());
        }

        if let Some(reference) = &f.site {
            let r = self
                .site_by_ref(reference)
                .await?
                .ok_or_else(|| not_found("site", reference))?;
            return Ok(Some(ResolvedScope {
                content_type: "dcim.site",
                id: r.id,
            }));
        }
        if let Some(reference) = &f.region {
            let r = self
                .region_by_ref(reference)
                .await?
                .ok_or_else(|| not_found("region", reference))?;
            return Ok(Some(ResolvedScope {
                content_type: "dcim.region",
                id: r.id,
            }));
        }
        if let Some(reference) = &f.site_group {
            let r = self
                .site_group_by_ref(reference)
                .await?
                .ok_or_else(|| not_found("site group", reference))?;
            return Ok(Some(ResolvedScope {
                content_type: "dcim.sitegroup",
                id: r.id,
            }));
        }
        if let Some(reference) = &f.location {
            let r = self
                .location_by_ref(reference)
                .await?
                .ok_or_else(|| not_found("location", reference))?;
            return Ok(Some(ResolvedScope {
                content_type: "dcim.location",
                id: r.id,
            }));
        }
        Ok(None)
    }

    /// Resolve the optional `--vrf` reference (id | rd | name) to a numeric id.
    ///
    /// Reuses the same [`vrf_by_ref`](NetBoxClient::vrf_by_ref) resolver the
    /// exact-lookup path uses, so `--vrf` means the same thing across `nbox ip`,
    /// `nbox prefix`, and search. An unknown ref is a not-found error (exit 4) —
    /// search fails loudly rather than silently returning an empty set.
    async fn resolve_vrf(&self, f: &SearchFilters) -> Result<Option<u64>> {
        let Some(reference) = &f.vrf else {
            return Ok(None);
        };
        let v = self
            .vrf_by_ref(reference)
            .await?
            .ok_or_else(|| not_found("VRF", reference))?;
        Ok(Some(v.id))
    }

    async fn graphql_list<T>(
        &self,
        caps: &GraphqlCapabilities,
        list_name: &str,
        filters: Value,
        selection: &str,
        limit: usize,
    ) -> Result<Vec<T>>
    where
        T: DeserializeOwned,
    {
        let Some(field) = caps.list(list_name) else {
            return Ok(Vec::new());
        };
        let Some(filter_type) = field.filter_type() else {
            return Ok(Vec::new());
        };
        let page_limit = limit.clamp(1, MAX_PAGE_SIZE);
        let pagination = if field.has_pagination() {
            format!(", pagination: {{offset: 0, limit: {page_limit}}}")
        } else {
            String::new()
        };
        let query = format!(
            "query($filters: {filter_type}) {{ {list_name}(filters: $filters{pagination}) {{ {selection} }} }}"
        );
        let data: Map<String, Value> = self.graphql(&query, json!({ "filters": filters })).await?;
        let rows = data
            .get(list_name)
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        serde_json::from_value(rows)
            .with_context(|| format!("deserializing GraphQL {list_name} rows"))
    }

    fn gql_filters(
        caps: &GraphqlCapabilities,
        list_name: &str,
        q: &str,
        f: &SearchFilters,
        supported: &[&str],
    ) -> Option<Map<String, Value>> {
        let filter_type = caps.list(list_name)?.filter_type()?;
        let mut filters = Map::new();
        if let Some(v) = caps.filter_value(filter_type, "q", json!(q)) {
            filters.insert("q".into(), v);
        }

        for (key, active) in [
            ("status", &f.status),
            ("tenant", &f.tenant),
            ("role", &f.role),
            ("tag", &f.tag),
        ] {
            let Some(value) = active else {
                continue;
            };
            if !supported.contains(&key) {
                return None;
            }
            let v = caps.filter_value(filter_type, key, json!(value))?;
            filters.insert(key.into(), v);
        }
        Some(filters)
    }

    fn gql_add_filter(
        caps: &GraphqlCapabilities,
        list_name: &str,
        filters: &mut Map<String, Value>,
        key: &str,
        value: Value,
    ) -> bool {
        let Some(filter_type) = caps.list(list_name).and_then(|field| field.filter_type()) else {
            return false;
        };
        let Some(value) = caps.filter_value(filter_type, key, value) else {
            return false;
        };
        filters.insert(key.into(), value);
        true
    }

    fn gql_scope_filter_key(scope: &ResolvedScope) -> &'static str {
        match scope.content_type {
            "dcim.site" => "site_id",
            "dcim.region" => "region_id",
            "dcim.sitegroup" => "site_group_id",
            "dcim.location" => "location_id",
            _ => "scope_id",
        }
    }

    fn gql_web_url(&self, kind: ObjectKind, id: u64) -> String {
        let path = match kind {
            ObjectKind::Device => format!("dcim/devices/{id}/"),
            ObjectKind::Site => format!("dcim/sites/{id}/"),
            ObjectKind::IpAddress => format!("ipam/ip-addresses/{id}/"),
            ObjectKind::Prefix => format!("ipam/prefixes/{id}/"),
            ObjectKind::Vlan => format!("ipam/vlans/{id}/"),
            ObjectKind::Circuit => format!("circuits/circuits/{id}/"),
            ObjectKind::Aggregate => format!("ipam/aggregates/{id}/"),
            ObjectKind::Asn => format!("ipam/asns/{id}/"),
            ObjectKind::IpRange => format!("ipam/ip-ranges/{id}/"),
            ObjectKind::Tenant => format!("tenancy/tenants/{id}/"),
            ObjectKind::Contact => format!("tenancy/contacts/{id}/"),
            ObjectKind::Provider => format!("circuits/providers/{id}/"),
            ObjectKind::Vm => format!("virtualization/virtual-machines/{id}/"),
            ObjectKind::Cluster => format!("virtualization/clusters/{id}/"),
            ObjectKind::Rack => format!("dcim/racks/{id}/"),
        };
        self.base_url()
            .join(&path)
            .map_or_else(|_| path, |url| url.to_string())
    }

    async fn gql_search_devices(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let Some(mut filters) = Self::gql_filters(
            caps,
            "device_list",
            q,
            f,
            &["status", "tenant", "role", "tag"],
        ) else {
            return Ok(Vec::new());
        };
        if let Some(scope) = scope
            && !Self::gql_add_filter(
                caps,
                "device_list",
                &mut filters,
                Self::gql_scope_filter_key(scope),
                json!(scope.id),
            )
        {
            return Ok(Vec::new());
        }
        let rows: Vec<GqlNamedSite> = self
            .graphql_list(
                caps,
                "device_list",
                Value::Object(filters),
                "id name display site { id name display slug }",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|d| {
                let id = d.id()?;
                let name = d.name.or(d.display)?;
                Some(SearchResult {
                    kind: ObjectKind::Device,
                    id,
                    score: score_match(q, &name),
                    subtitle: d.site.and_then(GqlBrief::label),
                    url: self.gql_web_url(ObjectKind::Device, id),
                    display: name,
                })
            })
            .collect())
    }

    async fn gql_search_sites(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(filters) =
            Self::gql_filters(caps, "site_list", q, f, &["status", "tenant", "tag"])
        else {
            return Ok(Vec::new());
        };
        let rows: Vec<GqlSite> = self
            .graphql_list(
                caps,
                "site_list",
                Value::Object(filters),
                "id name display slug",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|s| {
                let id = s.id()?;
                let display = s.name.or(s.display)?;
                Some(SearchResult {
                    kind: ObjectKind::Site,
                    id,
                    score: score_match(q, &display),
                    subtitle: s.slug,
                    url: self.gql_web_url(ObjectKind::Site, id),
                    display,
                })
            })
            .collect())
    }

    async fn gql_search_ips(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        vrf_id: Option<u64>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(mut filters) = Self::gql_filters(
            caps,
            "ip_address_list",
            q,
            f,
            &["status", "tenant", "role", "tag"],
        ) else {
            return Ok(Vec::new());
        };
        if let Some(id) = vrf_id
            && !Self::gql_add_filter(caps, "ip_address_list", &mut filters, "vrf_id", json!(id))
        {
            return Ok(Vec::new());
        }
        let rows: Vec<GqlIpAddress> = self
            .graphql_list(
                caps,
                "ip_address_list",
                Value::Object(filters),
                "id address display dns_name",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|ip| {
                let id = ip.id()?;
                let display = ip.address.or(ip.display)?;
                Some(SearchResult {
                    kind: ObjectKind::IpAddress,
                    id,
                    score: score_match(q, &display),
                    subtitle: non_empty(ip.dns_name),
                    url: self.gql_web_url(ObjectKind::IpAddress, id),
                    display,
                })
            })
            .collect())
    }

    async fn gql_search_prefixes(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        vrf_id: Option<u64>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let without_scope = SearchFilters {
            site: None,
            region: None,
            site_group: None,
            location: None,
            ..f.clone()
        };
        let Some(mut filters) = Self::gql_filters(
            caps,
            "prefix_list",
            q,
            &without_scope,
            &["status", "tenant", "role", "tag"],
        ) else {
            return Ok(Vec::new());
        };
        if let Some(scope) = scope
            && !Self::gql_add_filter(
                caps,
                "prefix_list",
                &mut filters,
                Self::gql_scope_filter_key(scope),
                json!(scope.id),
            )
        {
            // Prefer the friendly per-scope key when NetBox exposes one. The
            // 4.2+ polymorphic shape instead exposes `scope_type`+`scope_id`,
            // which preserves exact scope semantics across site/region/group/location.
            let added_type = Self::gql_add_filter(
                caps,
                "prefix_list",
                &mut filters,
                "scope_type",
                json!(scope.content_type),
            );
            let added_id = Self::gql_add_filter(
                caps,
                "prefix_list",
                &mut filters,
                "scope_id",
                json!(scope.id),
            );
            if !(added_type && added_id) {
                return Ok(Vec::new());
            }
        }
        if let Some(id) = vrf_id
            && !Self::gql_add_filter(caps, "prefix_list", &mut filters, "vrf_id", json!(id))
        {
            return Ok(Vec::new());
        }
        let rows: Vec<GqlPrefix> = self
            .graphql_list(
                caps,
                "prefix_list",
                Value::Object(filters),
                "id prefix display scope { ... on SiteType { id name display slug } ... on RegionType { id name display slug } ... on LocationType { id name display slug } ... on SiteGroupType { id name display slug } }",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|p| {
                let id = p.id()?;
                let display = p.prefix.or(p.display)?;
                Some(SearchResult {
                    kind: ObjectKind::Prefix,
                    id,
                    score: score_match(q, &display),
                    subtitle: p.scope.and_then(GqlBrief::label),
                    url: self.gql_web_url(ObjectKind::Prefix, id),
                    display,
                })
            })
            .collect())
    }

    async fn gql_search_vlans(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if skip_for_id_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(mut filters) = Self::gql_filters(
            caps,
            "vlan_list",
            q,
            f,
            &["status", "tenant", "role", "tag"],
        ) else {
            return Ok(Vec::new());
        };
        if let Some(scope) = scope
            && !Self::gql_add_filter(caps, "vlan_list", &mut filters, "site_id", json!(scope.id))
        {
            return Ok(Vec::new());
        }
        let rows: Vec<GqlVlan> = self
            .graphql_list(
                caps,
                "vlan_list",
                Value::Object(filters),
                "id vid name display site { id name display slug } group { id name display slug }",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|v| {
                let id = v.id()?;
                let name = v.name.or(v.display).unwrap_or_default();
                let display = format!("{} {}", v.vid, name);
                Some(SearchResult {
                    kind: ObjectKind::Vlan,
                    id,
                    score: score_match(q, &display),
                    subtitle: v.site.or(v.group).and_then(GqlBrief::label),
                    url: self.gql_web_url(ObjectKind::Vlan, id),
                    display,
                })
            })
            .collect())
    }

    async fn gql_search_circuits(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(filters) =
            Self::gql_filters(caps, "circuit_list", q, f, &["status", "tenant", "tag"])
        else {
            return Ok(Vec::new());
        };
        let rows: Vec<GqlCircuit> = self
            .graphql_list(
                caps,
                "circuit_list",
                Value::Object(filters),
                "id cid display provider { id name display slug }",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|c| {
                let id = c.id()?;
                let display = c.cid.or(c.display)?;
                Some(SearchResult {
                    kind: ObjectKind::Circuit,
                    id,
                    score: score_match(q, &display),
                    subtitle: c.provider.and_then(GqlBrief::label),
                    url: self.gql_web_url(ObjectKind::Circuit, id),
                    display,
                })
            })
            .collect())
    }

    async fn gql_search_aggregates(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(filters) = Self::gql_filters(caps, "aggregate_list", q, f, &["tenant", "tag"])
        else {
            return Ok(Vec::new());
        };
        let rows: Vec<GqlPrefixLike> = self
            .graphql_list(
                caps,
                "aggregate_list",
                Value::Object(filters),
                "id prefix display rir { id name display slug }",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|a| {
                let id = a.id()?;
                let display = a.prefix.or(a.display)?;
                Some(SearchResult {
                    kind: ObjectKind::Aggregate,
                    id,
                    score: score_match(q, &display),
                    subtitle: a.rir.and_then(GqlBrief::label),
                    url: self.gql_web_url(ObjectKind::Aggregate, id),
                    display,
                })
            })
            .collect())
    }

    async fn gql_search_asns(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(mut filters) = Self::gql_filters(caps, "asn_list", q, f, &["tenant", "tag"])
        else {
            return Ok(Vec::new());
        };
        if let Ok(asn) = q.parse::<u32>() {
            filters.remove("q");
            if !Self::gql_add_filter(caps, "asn_list", &mut filters, "asn", json!(asn)) {
                return Ok(Vec::new());
            }
        }
        let rows: Vec<GqlAsn> = self
            .graphql_list(
                caps,
                "asn_list",
                Value::Object(filters),
                "id asn display rir { id name display slug }",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|a| {
                let id = a.id()?;
                let asn = a.asn?;
                let display = format!("AS{asn}");
                Some(SearchResult {
                    kind: ObjectKind::Asn,
                    id,
                    score: score_match(q, &asn.to_string()),
                    subtitle: a.rir.and_then(GqlBrief::label),
                    url: self.gql_web_url(ObjectKind::Asn, id),
                    display,
                })
            })
            .collect())
    }

    async fn gql_search_ip_ranges(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(filters) = Self::gql_filters(
            caps,
            "ip_range_list",
            q,
            f,
            &["status", "tenant", "role", "tag"],
        ) else {
            return Ok(Vec::new());
        };
        let rows: Vec<GqlIpRange> = self
            .graphql_list(
                caps,
                "ip_range_list",
                Value::Object(filters),
                "id start_address end_address display vrf { id name display rd } role { id name display slug }",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let id = r.id()?;
                let display = r
                    .display
                    .or_else(|| Some(format!("{}-{}", r.start_address?, r.end_address?)))?;
                Some(SearchResult {
                    kind: ObjectKind::IpRange,
                    id,
                    score: score_match(q, &display),
                    subtitle: r.vrf.or(r.role).and_then(GqlBrief::label),
                    url: self.gql_web_url(ObjectKind::IpRange, id),
                    display,
                })
            })
            .collect())
    }

    async fn gql_search_tenants(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(filters) = Self::gql_filters(caps, "tenant_list", q, f, &["tag"]) else {
            return Ok(Vec::new());
        };
        let rows: Vec<GqlTenantLike> = self
            .graphql_list(
                caps,
                "tenant_list",
                Value::Object(filters),
                "id name display slug group { id name display slug }",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|t| {
                let id = t.id()?;
                let display = t.name.or(t.display)?;
                Some(SearchResult {
                    kind: ObjectKind::Tenant,
                    id,
                    score: score_match(q, &display),
                    subtitle: t.group.and_then(GqlBrief::label).or(t.slug),
                    url: self.gql_web_url(ObjectKind::Tenant, id),
                    display,
                })
            })
            .collect())
    }

    async fn gql_search_contacts(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(filters) = Self::gql_filters(caps, "contact_list", q, f, &["tag"]) else {
            return Ok(Vec::new());
        };
        let rows: Vec<GqlContact> = self
            .graphql_list(
                caps,
                "contact_list",
                Value::Object(filters),
                "id name display email",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|c| {
                let id = c.id()?;
                let display = c.name.or(c.display)?;
                Some(SearchResult {
                    kind: ObjectKind::Contact,
                    id,
                    score: score_match(q, &display),
                    subtitle: non_empty(c.email),
                    url: self.gql_web_url(ObjectKind::Contact, id),
                    display,
                })
            })
            .collect())
    }

    async fn gql_search_providers(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(filters) = Self::gql_filters(caps, "provider_list", q, f, &["tag"]) else {
            return Ok(Vec::new());
        };
        let rows: Vec<GqlProvider> = self
            .graphql_list(
                caps,
                "provider_list",
                Value::Object(filters),
                "id name display slug asns { id asn }",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|p| {
                let id = p.id()?;
                let display = p.name.or(p.display)?;
                let subtitle = p
                    .asns
                    .first()
                    .and_then(|asn| asn.asn.map(|n| format!("AS{n}")))
                    .or(p.slug);
                Some(SearchResult {
                    kind: ObjectKind::Provider,
                    id,
                    score: score_match(q, &display),
                    subtitle,
                    url: self.gql_web_url(ObjectKind::Provider, id),
                    display,
                })
            })
            .collect())
    }

    async fn gql_search_vms(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if skip_for_id_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(mut filters) = Self::gql_filters(
            caps,
            "virtual_machine_list",
            q,
            f,
            &["status", "tenant", "role", "tag"],
        ) else {
            return Ok(Vec::new());
        };
        if let Some(scope) = scope
            && !Self::gql_add_filter(
                caps,
                "virtual_machine_list",
                &mut filters,
                "site_id",
                json!(scope.id),
            )
        {
            return Ok(Vec::new());
        }
        let rows: Vec<GqlVm> = self
            .graphql_list(
                caps,
                "virtual_machine_list",
                Value::Object(filters),
                "id name display cluster { id name display } site { id name display slug }",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|vm| {
                let id = vm.id()?;
                let display = vm.name.or(vm.display)?;
                Some(SearchResult {
                    kind: ObjectKind::Vm,
                    id,
                    score: score_match(q, &display),
                    subtitle: vm.cluster.or(vm.site).and_then(GqlBrief::label),
                    url: self.gql_web_url(ObjectKind::Vm, id),
                    display,
                })
            })
            .collect())
    }

    async fn gql_search_clusters(
        &self,
        caps: &GraphqlCapabilities,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let without_scope = SearchFilters {
            site: None,
            region: None,
            site_group: None,
            location: None,
            ..f.clone()
        };
        let Some(mut filters) = Self::gql_filters(
            caps,
            "cluster_list",
            q,
            &without_scope,
            &["status", "tenant", "tag"],
        ) else {
            return Ok(Vec::new());
        };
        if let Some(scope) = scope
            && !Self::gql_add_filter(
                caps,
                "cluster_list",
                &mut filters,
                Self::gql_scope_filter_key(scope),
                json!(scope.id),
            )
        {
            // Prefer the friendly per-scope key when available; otherwise fall
            // back to NetBox's polymorphic `scope_type`+`scope_id` filters.
            let added_type = Self::gql_add_filter(
                caps,
                "cluster_list",
                &mut filters,
                "scope_type",
                json!(scope.content_type),
            );
            let added_id = Self::gql_add_filter(
                caps,
                "cluster_list",
                &mut filters,
                "scope_id",
                json!(scope.id),
            );
            if !(added_type && added_id) {
                return Ok(Vec::new());
            }
        }
        let rows: Vec<GqlCluster> = self
            .graphql_list(
                caps,
                "cluster_list",
                Value::Object(filters),
                "id name display type { id name display slug } scope { ... on SiteType { id name display slug } ... on RegionType { id name display slug } ... on LocationType { id name display slug } ... on SiteGroupType { id name display slug } }",
                limit,
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|c| {
                let id = c.id()?;
                let display = c.name.or(c.display)?;
                Some(SearchResult {
                    kind: ObjectKind::Cluster,
                    id,
                    score: score_match(q, &display),
                    subtitle: c.type_.or(c.scope).and_then(GqlBrief::label),
                    url: self.gql_web_url(ObjectKind::Cluster, id),
                    display,
                })
            })
            .collect())
    }

    async fn search_devices(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // Devices expose clean id filters for every scope kind
        // (`site_id`/`region_id`/`site_group_id`/`location_id`), so honor all four
        // out-of-band by the resolved id. The plain `?site=` param wants a slug, so
        // a `--site` given as an id or display name would silently miss — use the
        // resolved `site_id` instead.
        let device_scope: Option<(&'static str, u64)> = match scope.map(|s| s.content_type) {
            Some("dcim.site") => scope.map(|s| ("site_id", s.id)),
            Some("dcim.region") => scope.map(|s| ("region_id", s.id)),
            Some("dcim.sitegroup") => scope.map(|s| ("site_group_id", s.id)),
            Some("dcim.location") => scope.map(|s| ("location_id", s.id)),
            _ => None,
        };
        let Some(mut params) = endpoint_params(q, f, &["status", "tenant", "role", "tag"]) else {
            return Ok(Vec::new());
        };
        if let Some((key, id)) = device_scope {
            params.push((key, id.to_string()));
        }
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

    async fn search_sites(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // The site search itself carries no scope filter (a site has no parent
        // site/region/site-group/location filter on this endpoint that maps to our
        // scope flags cleanly), so any active scope skips it rather than sending a
        // dead param.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
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

    async fn search_ips(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        vrf_id: Option<u64>,
    ) -> Result<Vec<SearchResult>> {
        // IPs carry no scope filter that maps to our flags — any active scope
        // (including `--site`) skips them.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(mut params) = endpoint_params(q, f, &["status", "tenant", "role", "tag"]) else {
            return Ok(Vec::new());
        };
        // IPs carry a VRF: apply the resolved `--vrf` id as `vrf_id=`.
        if let Some(id) = vrf_id {
            params.push(("vrf_id", id.to_string()));
        }
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

    async fn search_prefixes(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        vrf_id: Option<u64>,
    ) -> Result<Vec<SearchResult>> {
        // Scope is handled out-of-band, not through the allowlist: NetBox 4.2
        // dropped the prefix `site` FK for the polymorphic `scope`, so a plain
        // `?site=`/`?region=`/… is a dead filter. The caller resolves the single
        // active scope flag to an id up front; we translate it to
        // `scope_type=<ct>` + `scope_id=<id>` (e.g. `dcim.region`), which the 4.2+
        // API honors as an EXACT match (no hierarchy/descendant expansion). The
        // scope refs are cleared from the filters before the allowlist check
        // (otherwise `params_for` would skip the endpoint on `site`) and
        // re-expressed as scope params below.
        let without_scope = SearchFilters {
            site: None,
            region: None,
            site_group: None,
            location: None,
            ..f.clone()
        };
        let Some(mut params) =
            endpoint_params(q, &without_scope, &["status", "tenant", "role", "tag"])
        else {
            return Ok(Vec::new());
        };
        if let Some(s) = scope {
            params.push(("scope_type", s.content_type.to_string()));
            params.push(("scope_id", s.id.to_string()));
        }
        // Prefixes carry a VRF: apply the resolved `--vrf` id as `vrf_id=`. This
        // is orthogonal to scope — NetBox ANDs them, so a vrf+scope combo narrows
        // to prefixes matching both.
        if let Some(id) = vrf_id {
            params.push(("vrf_id", id.to_string()));
        }
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

    async fn search_vlans(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // VLANs honor `--site` via the resolved `site_id`. NetBox's VLAN region/
        // site-group filters exist but aren't uniformly clean (no location scope),
        // so skip VLANs for any id-based scope rather than apply an inconsistent
        // subset — matching the conservative "skip if unsure" rule.
        if skip_for_id_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(mut params) = endpoint_params(q, f, &["status", "tenant", "role", "tag"]) else {
            return Ok(Vec::new());
        };
        // Only a `dcim.site` scope reaches here (the id-based scopes skipped above):
        // filter by the resolved `site_id`, not the slug-only `?site=`.
        if let Some(s) = scope {
            params.push(("site_id", s.id.to_string()));
        }
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
                    // Match the detail view's precedence (`VlanView::build` /
                    // `vlan_scope_label`): the polymorphic `scope` wins, then a
                    // directly assigned `site`, then the VLAN `group`. Keeps the
                    // search subtitle and the detail view's location agreeing.
                    subtitle: vlan_subtitle(&v),
                    url: api_to_web_url(&v.url),
                    display,
                }
            })
            .collect())
    }

    async fn search_circuits(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // Circuits carry no scope filter that maps to our flags — any active scope
        // (including `--site`) skips them.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
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

    async fn search_aggregates(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // Aggregates carry no scope filter that maps to our flags — any active
        // scope (including `--site`) skips them.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
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

    async fn search_asns(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // ASNs carry no scope filter that maps to our flags — any active scope
        // (including `--site`) skips them.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
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

    async fn search_ip_ranges(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // IP ranges carry no scope filter that maps to our flags — any active
        // scope (including `--site`) skips them.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
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

    async fn search_tenants(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // Tenants carry no scope filter (site/region/site-group/location) — skip
        // them for any active scope rather than return an unfiltered set.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        // The tenant endpoint accepts only `q` + `tag` from our filter set
        // (no status/tenant/role), so an unsupported active filter skips it.
        let Some(params) = endpoint_params(q, f, &["tag"]) else {
            return Ok(Vec::new());
        };
        let page: Page<Tenant> = self.list(Endpoint::Tenants, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|t| SearchResult {
                kind: ObjectKind::Tenant,
                id: t.id,
                score: score_match(q, &t.name),
                subtitle: t
                    .group
                    .as_ref()
                    .map(super::models::common::BriefObject::label)
                    .or(Some(t.slug)),
                url: api_to_web_url(&t.url),
                display: t.name,
            })
            .collect())
    }

    async fn search_contacts(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // Contacts carry no scope filter — skip them for any active scope.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        // Contacts accept only `q` + `tag` (no status/tenant/role).
        let Some(params) = endpoint_params(q, f, &["tag"]) else {
            return Ok(Vec::new());
        };
        let page: Page<Contact> = self.list(Endpoint::Contacts, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|c| SearchResult {
                kind: ObjectKind::Contact,
                id: c.id,
                score: score_match(q, &c.name),
                subtitle: c
                    .group
                    .as_ref()
                    .map(super::models::common::BriefObject::label)
                    .or_else(|| non_empty(c.email)),
                url: api_to_web_url(&c.url),
                display: c.name,
            })
            .collect())
    }

    async fn search_providers(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // Providers carry no scope filter (site/region/site-group/location) — skip
        // them for any active scope rather than return an unfiltered set.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        // Providers accept only `q` + `tag` (no status/tenant/role).
        let Some(params) = endpoint_params(q, f, &["tag"]) else {
            return Ok(Vec::new());
        };
        let page: Page<Provider> = self.list(Endpoint::Providers, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|p| {
                // Prefer the first AS number as a subtitle, falling back to slug.
                let subtitle = p
                    .asns
                    .first()
                    .map(|a| format!("AS{}", a.asn))
                    .or_else(|| non_empty(Some(p.slug.clone())));
                SearchResult {
                    kind: ObjectKind::Provider,
                    id: p.id,
                    score: score_match(q, &p.name),
                    subtitle,
                    url: api_to_web_url(&p.url),
                    display: p.name,
                }
            })
            .collect())
    }

    async fn search_vms(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // VMs honor `--site` via the resolved `site_id`; for the id-based scopes
        // (region/site-group/location) the VM filters aren't uniformly clean (no
        // location scope), so skip them rather than apply an inconsistent subset.
        if skip_for_id_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(mut params) = endpoint_params(q, f, &["status", "tenant", "role", "tag"]) else {
            return Ok(Vec::new());
        };
        // Only a `dcim.site` scope reaches here (the id-based scopes skipped above):
        // filter by the resolved `site_id`, not the slug-only `?site=`.
        if let Some(s) = scope {
            params.push(("site_id", s.id.to_string()));
        }
        let page: Page<VirtualMachine> = self.list(Endpoint::VirtualMachines, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|vm| SearchResult {
                kind: ObjectKind::Vm,
                id: vm.id,
                score: score_match(q, &vm.name),
                subtitle: vm
                    .cluster
                    .as_ref()
                    .or(vm.site.as_ref())
                    .map(super::models::common::BriefObject::label),
                url: api_to_web_url(&vm.url),
                display: vm.name,
            })
            .collect())
    }

    async fn search_clusters(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // NetBox 4.2+ scopes a cluster polymorphically (same `scope_type`/
        // `scope_id` filter as prefixes), so honor a region/site-group/location
        // scope the way `search_prefixes` does: clear the scope refs from the
        // allowlist (so `--site` doesn't skip the endpoint) and re-express the
        // single active scope as `scope_type`+`scope_id`. `--site` flows through
        // here too (as `dcim.site`), since clusters honor it via the polymorphic
        // scope as well.
        let without_scope = SearchFilters {
            site: None,
            region: None,
            site_group: None,
            location: None,
            ..f.clone()
        };
        // Clusters accept `status`/`tenant`/`tag` (no `role`); scope is applied
        // out-of-band below.
        let Some(mut params) = endpoint_params(q, &without_scope, &["status", "tenant", "tag"])
        else {
            return Ok(Vec::new());
        };
        if let Some(s) = scope {
            params.push(("scope_type", s.content_type.to_string()));
            params.push(("scope_id", s.id.to_string()));
        }
        let page: Page<Cluster> = self.list(Endpoint::Clusters, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|c| SearchResult {
                kind: ObjectKind::Cluster,
                id: c.id,
                score: score_match(q, &c.name),
                subtitle: c
                    .type_
                    .as_ref()
                    .or(c.scope.as_ref())
                    .map(super::models::common::BriefObject::label),
                url: api_to_web_url(&c.url),
                display: c.name,
            })
            .collect())
    }
}

#[derive(Debug, Deserialize)]
struct GqlBrief {
    id: Option<String>,
    name: Option<String>,
    display: Option<String>,
    slug: Option<String>,
    rd: Option<String>,
}

impl GqlBrief {
    fn label(self) -> Option<String> {
        self.display
            .or(self.name)
            .or(self.slug)
            .or(self.rd)
            .or(self.id)
    }
}

trait GqlId {
    fn raw_id(&self) -> Option<&str>;

    fn id(&self) -> Option<u64> {
        let raw_id = self.raw_id()?;
        match raw_id.parse() {
            Ok(id) => Some(id),
            Err(error) => {
                tracing::debug!(
                    raw_id,
                    error = %error,
                    "dropping GraphQL search row with non-numeric id"
                );
                None
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct GqlNamedSite {
    id: Option<String>,
    name: Option<String>,
    display: Option<String>,
    site: Option<GqlBrief>,
}

impl GqlId for GqlNamedSite {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct GqlSite {
    id: Option<String>,
    name: Option<String>,
    display: Option<String>,
    slug: Option<String>,
}

impl GqlId for GqlSite {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct GqlIpAddress {
    id: Option<String>,
    address: Option<String>,
    display: Option<String>,
    dns_name: Option<String>,
}

impl GqlId for GqlIpAddress {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct GqlPrefix {
    id: Option<String>,
    prefix: Option<String>,
    display: Option<String>,
    scope: Option<GqlBrief>,
}

impl GqlId for GqlPrefix {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct GqlVlan {
    id: Option<String>,
    vid: u16,
    name: Option<String>,
    display: Option<String>,
    site: Option<GqlBrief>,
    group: Option<GqlBrief>,
}

impl GqlId for GqlVlan {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct GqlCircuit {
    id: Option<String>,
    cid: Option<String>,
    display: Option<String>,
    provider: Option<GqlBrief>,
}

impl GqlId for GqlCircuit {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct GqlPrefixLike {
    id: Option<String>,
    prefix: Option<String>,
    display: Option<String>,
    rir: Option<GqlBrief>,
}

impl GqlId for GqlPrefixLike {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct GqlAsn {
    id: Option<String>,
    asn: Option<u32>,
    rir: Option<GqlBrief>,
}

impl GqlId for GqlAsn {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct GqlIpRange {
    id: Option<String>,
    start_address: Option<String>,
    end_address: Option<String>,
    display: Option<String>,
    vrf: Option<GqlBrief>,
    role: Option<GqlBrief>,
}

impl GqlId for GqlIpRange {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct GqlTenantLike {
    id: Option<String>,
    name: Option<String>,
    display: Option<String>,
    slug: Option<String>,
    group: Option<GqlBrief>,
}

impl GqlId for GqlTenantLike {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct GqlContact {
    id: Option<String>,
    name: Option<String>,
    display: Option<String>,
    email: Option<String>,
}

impl GqlId for GqlContact {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct GqlProvider {
    id: Option<String>,
    name: Option<String>,
    display: Option<String>,
    slug: Option<String>,
    asns: Vec<GqlAsn>,
}

impl GqlId for GqlProvider {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct GqlVm {
    id: Option<String>,
    name: Option<String>,
    display: Option<String>,
    cluster: Option<GqlBrief>,
    site: Option<GqlBrief>,
}

impl GqlId for GqlVm {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct GqlCluster {
    id: Option<String>,
    name: Option<String>,
    display: Option<String>,
    #[serde(rename = "type")]
    type_: Option<GqlBrief>,
    scope: Option<GqlBrief>,
}

impl GqlId for GqlCluster {
    fn raw_id(&self) -> Option<&str> {
        self.id.as_deref()
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
        // `site` is a scope filter handled out-of-band (resolved to `site_id`), so
        // it never flows through the plain-value allowlist — only `status` does.
        let dev = endpoint_params("edge", &f, &["status", "tenant", "role"]).unwrap();
        assert!(dev.contains(&("q", "edge".to_string())));
        assert!(dev.contains(&("status", "active".to_string())));
        // The raw `site` value must NOT leak into the allowlist params.
        assert!(!dev.iter().any(|(k, _)| *k == "site"));
        // An endpoint that doesn't support `status` → skipped entirely (the active
        // `status` filter can't be satisfied).
        assert!(endpoint_params("edge", &f, &["tenant", "role"]).is_none());
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
    fn vlan_subtitle_prefers_scope_then_site_then_group() {
        use serde_json::json;

        // Polymorphic scope present → the subtitle is the scope object's label,
        // even when a site/group are also set (scope wins).
        let scoped: Vlan = serde_json::from_value(json!({
            "id": 1, "url": "u", "vid": 10, "name": "a",
            "scope_type": "dcim.region", "scope": {"id": 1, "display": "us-east"},
            "site": {"id": 2, "display": "iad1"},
            "group": {"id": 3, "display": "campus"}
        }))
        .unwrap();
        assert_eq!(vlan_subtitle(&scoped).as_deref(), Some("us-east"));

        // No scope → fall back to the directly assigned site.
        let sited: Vlan = serde_json::from_value(json!({
            "id": 2, "url": "u", "vid": 11, "name": "b",
            "site": {"id": 1, "display": "iad1"},
            "group": {"id": 2, "display": "campus"}
        }))
        .unwrap();
        assert_eq!(vlan_subtitle(&sited).as_deref(), Some("iad1"));

        // No scope, no site → fall back to the VLAN group.
        let grouped: Vlan = serde_json::from_value(json!({
            "id": 3, "url": "u", "vid": 12, "name": "c",
            "group": {"id": 1, "display": "campus"}
        }))
        .unwrap();
        assert_eq!(vlan_subtitle(&grouped).as_deref(), Some("campus"));

        // None of the three → no subtitle.
        let bare: Vlan =
            serde_json::from_value(json!({"id": 4, "url": "u", "vid": 13, "name": "d"})).unwrap();
        assert_eq!(vlan_subtitle(&bare), None);
    }

    #[test]
    fn object_kind_labels_cover_new_kinds() {
        assert_eq!(ObjectKind::Circuit.as_str(), "circuit");
        assert_eq!(ObjectKind::Aggregate.as_str(), "aggregate");
        assert_eq!(ObjectKind::Asn.as_str(), "asn");
        assert_eq!(ObjectKind::IpRange.as_str(), "ip-range");
    }
}
