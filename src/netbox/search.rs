//! Normalized multi-endpoint search.
//!
//! There is no universal NetBox search endpoint, so `nbox search` fans out across
//! several object types in parallel using each endpoint's built-in `q=`
//! quick-search, then merges, ranks, dedups, and truncates.

use std::collections::HashSet;

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::netbox::client::{MAX_PAGE_SIZE, NetBoxClient};
use crate::netbox::endpoints::Endpoint;
use crate::netbox::graphql::GraphqlCapabilities;
use crate::netbox::models::circuits::{Circuit, Provider};
use crate::netbox::models::dcim::{Device, Rack, Site};
use crate::netbox::models::ipam::{Aggregate, Asn, IpAddress, IpRange, Prefix, Vlan, Vrf};
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
    /// A rack. Searchable by name (honoring the site/region/site-group/location
    /// scope), openable in the TUI, and a cross-navigation target (e.g. a device's
    /// rack). Kept last to preserve the existing variants' order.
    Rack,
    /// A VRF (routing/forwarding instance). Searchable by name/RD, openable in the
    /// TUI as a routing-context view (its prefix tree + scoped addresses + route
    /// targets), and a cross-navigation target. Carries no site scope, so scope
    /// filters skip it. Kept last to preserve the existing variants' order.
    Vrf,
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
            ObjectKind::Vrf => "vrf",
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
    /// Search across devices, sites, racks, IPs, prefixes, VLANs, circuits,
    /// aggregates, ASNs, IP ranges, tenants, contacts, providers, virtual
    /// machines, and clusters in parallel.
    ///
    /// Returns ranked results plus a list of endpoints that failed. If every
    /// endpoint fails and nothing matched, returns the underlying `Err` (so a
    /// bad token surfaces as an auth error, not an empty result set). A *partial*
    /// failure — some endpoints down, others returning data — is reported via
    /// [`SearchOutcome::errors`] for the caller to act on.
    pub async fn search(&self, req: SearchRequest) -> Result<SearchOutcome> {
        // `nbox search` means canonical NetBox search semantics, so it always
        // runs over REST. NetBox's GraphQL API has no equivalent to REST's
        // full-text `q` filter (filtering moved to per-field Strawberry lookups
        // in 4.3), so GraphQL never backs the search surface. The fan-out is a
        // large future; box it here so spawned call sites can await `search()`
        // normally (clippy::large_futures).
        Box::pin(self.search_rest(req)).await
    }

    /// The REST search fan-out. Split out from [`search`](Self::search) so its
    /// large `join!` future stays behind a `Box::pin`.
    async fn search_rest(&self, req: SearchRequest) -> Result<SearchOutcome> {
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
            racks,
            vrfs,
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
            self.search_racks(&q, f, scope.as_ref()),
            self.search_vrfs(&q, f, scope.as_ref()),
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
            ("racks", racks),
            ("vrfs", vrfs),
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

    /// Fetch a VRF's prefixes + IP addresses in a single GraphQL query (the VRF
    /// detail "bundle"), normalized into the REST wire models so the downstream
    /// `VrfDetail` build is byte-identical to the REST path. The caller resolves
    /// the VRF id and its header over REST first (identity stays canonical); this
    /// fetches only the children. A real GraphQL/transport error propagates (fail
    /// closed) rather than degrading to empty data.
    pub(crate) async fn graphql_vrf_bundle(
        &self,
        vrf_id: u64,
        limit: usize,
    ) -> Result<(Vec<Prefix>, Vec<IpAddress>)> {
        let caps = self.graphql_capabilities().await?;
        let mut prefix_filters = Map::new();
        let mut ip_filters = Map::new();
        Self::gql_add_filter(
            &caps,
            "prefix_list",
            &mut prefix_filters,
            "vrf_id",
            json!(vrf_id),
        );
        Self::gql_add_filter(
            &caps,
            "ip_address_list",
            &mut ip_filters,
            "vrf_id",
            json!(vrf_id),
        );
        let prefix_type = caps
            .list("prefix_list")
            .and_then(|f| f.filter_type())
            .context("GraphQL schema is missing the prefix_list filter type")?;
        let ip_type = caps
            .list("ip_address_list")
            .and_then(|f| f.filter_type())
            .context("GraphQL schema is missing the ip_address_list filter type")?;
        let page = limit.clamp(1, MAX_PAGE_SIZE);
        let prefix_pag = gql_pagination(&caps, "prefix_list", page);
        let ip_pag = gql_pagination(&caps, "ip_address_list", page);

        let query = format!(
            "query($pf: {prefix_type}, $if: {ip_type}) {{ \
             prefix_list(filters: $pf{prefix_pag}) {{ id prefix _depth status description }} \
             ip_address_list(filters: $if{ip_pag}) {{ id address status dns_name description }} }}"
        );
        let data: Map<String, Value> = self
            .graphql(
                &query,
                json!({ "pf": Value::Object(prefix_filters), "if": Value::Object(ip_filters) }),
            )
            .await?;

        let prefixes = gql_rows(&data, "prefix_list")
            .iter()
            .filter_map(gql_row_to_prefix)
            .collect();
        let addresses = gql_rows(&data, "ip_address_list")
            .iter()
            .filter_map(gql_row_to_ip)
            .collect();
        Ok((prefixes, addresses))
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

    async fn search_racks(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // Racks expose clean id filters for every scope kind
        // (`site_id`/`region_id`/`site_group_id`/`location_id`), like devices, so
        // honor all four out-of-band by the resolved id (the plain `?site=` slug
        // param would silently miss a `--site` given as an id or display name).
        let rack_scope: Option<(&'static str, u64)> = match scope.map(|s| s.content_type) {
            Some("dcim.site") => scope.map(|s| ("site_id", s.id)),
            Some("dcim.region") => scope.map(|s| ("region_id", s.id)),
            Some("dcim.sitegroup") => scope.map(|s| ("site_group_id", s.id)),
            Some("dcim.location") => scope.map(|s| ("location_id", s.id)),
            _ => None,
        };
        let Some(mut params) = endpoint_params(q, f, &["status", "tenant", "role", "tag"]) else {
            return Ok(Vec::new());
        };
        if let Some((key, id)) = rack_scope {
            params.push((key, id.to_string()));
        }
        let page: Page<Rack> = self.list(Endpoint::Racks, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|r| SearchResult {
                kind: ObjectKind::Rack,
                id: r.id,
                score: score_match(q, &r.name),
                subtitle: r
                    .site
                    .as_ref()
                    .map(super::models::common::BriefObject::label),
                url: api_to_web_url(&r.url),
                display: r.name,
            })
            .collect())
    }

    async fn search_vrfs(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<SearchResult>> {
        // VRFs carry no site scope (site/region/site-group/location) — skip them
        // for any active scope rather than return an unfiltered set.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        // The VRF endpoint accepts `q` + `tenant` + `tag` from our filter set
        // (no status/role/site), so an unsupported active filter skips it.
        let Some(params) = endpoint_params(q, f, &["tenant", "tag"]) else {
            return Ok(Vec::new());
        };
        let page: Page<Vrf> = self.list(Endpoint::Vrfs, params).await?;
        Ok(page
            .results
            .into_iter()
            .map(|v| SearchResult {
                kind: ObjectKind::Vrf,
                id: v.id,
                score: score_match(q, &v.name),
                // The RD identifies a VRF at a glance; fall back to the tenant.
                subtitle: v.rd.clone().or_else(|| {
                    v.tenant
                        .as_ref()
                        .map(super::models::common::BriefObject::label)
                }),
                url: api_to_web_url(&v.url),
                display: v.name,
            })
            .collect())
    }
}

/// Pagination clause for a GraphQL list field, empty when it isn't paginated.
fn gql_pagination(caps: &GraphqlCapabilities, list_name: &str, limit: usize) -> String {
    match caps.list(list_name) {
        Some(field) if field.has_pagination() => {
            format!(", pagination: {{offset: 0, limit: {limit}}}")
        }
        _ => String::new(),
    }
}

/// The rows array for a GraphQL `*_list` field in a combined-query response.
fn gql_rows<'a>(data: &'a Map<String, Value>, key: &str) -> &'a [Value] {
    data.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Reshape a GraphQL string `status` (e.g. `"active"`) into the REST `Choice`
/// object shape (`{value,label}`) so it deserializes into the wire model.
fn gql_status_value(row: &Value) -> Value {
    match row.get("status").and_then(Value::as_str) {
        Some(s) => json!({ "value": s, "label": s }),
        None => Value::Null,
    }
}

/// Convert one GraphQL `prefix_list` row into the REST [`Prefix`] wire model
/// (only the fields the VRF tree needs), reusing its `Deserialize`.
fn gql_row_to_prefix(row: &Value) -> Option<Prefix> {
    let id: u64 = row.get("id")?.as_str()?.parse().ok()?;
    let prefix = row.get("prefix")?.as_str()?.to_string();
    serde_json::from_value(json!({
        "id": id,
        "url": "",
        "prefix": prefix,
        "status": gql_status_value(row),
        "description": row.get("description").and_then(Value::as_str),
        "_depth": row.get("_depth").and_then(Value::as_u64),
    }))
    .ok()
}

/// Convert one GraphQL `ip_address_list` row into the REST [`IpAddress`] model.
fn gql_row_to_ip(row: &Value) -> Option<IpAddress> {
    let id: u64 = row.get("id")?.as_str()?.parse().ok()?;
    let address = row.get("address")?.as_str()?.to_string();
    serde_json::from_value(json!({
        "id": id,
        "url": "",
        "address": address,
        "status": gql_status_value(row),
        "dns_name": row.get("dns_name").and_then(Value::as_str),
        "description": row.get("description").and_then(Value::as_str),
    }))
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gql_row_to_prefix_reshapes_status_id_and_depth() {
        // GraphQL gives string id, string status, and `_depth`; the wire model
        // wants numeric id, a Choice{value,label} status, and `depth`.
        let row = json!({
            "id": "34", "prefix": "10.20.0.0/16", "_depth": 0,
            "status": "container", "description": "supernet"
        });
        let p = gql_row_to_prefix(&row).expect("prefix");
        assert_eq!(p.id, 34);
        assert_eq!(p.prefix, "10.20.0.0/16");
        assert_eq!(p.depth, Some(0));
        assert_eq!(
            p.status.as_ref().map(|c| c.value.as_str()),
            Some("container")
        );
        assert_eq!(p.description.as_deref(), Some("supernet"));

        // A null status (GraphQL can omit it) stays None, not an error.
        let row = json!({"id": "35", "prefix": "10.20.1.0/24", "_depth": 1, "status": null});
        let p = gql_row_to_prefix(&row).expect("prefix");
        assert!(p.status.is_none());
        assert_eq!(p.depth, Some(1));
    }

    #[test]
    fn gql_row_to_ip_reshapes_fields() {
        let row = json!({
            "id": "6", "address": "10.20.1.10/24", "status": "active",
            "dns_name": "web-01.customer"
        });
        let ip = gql_row_to_ip(&row).expect("ip");
        assert_eq!(ip.id, 6);
        assert_eq!(ip.address, "10.20.1.10/24");
        assert_eq!(ip.status.as_ref().map(|c| c.value.as_str()), Some("active"));
        assert_eq!(ip.dns_name.as_deref(), Some("web-01.customer"));
    }

    #[test]
    fn gql_row_to_prefix_rejects_nonnumeric_id() {
        // A non-numeric id (shouldn't happen, but be defensive) drops the row
        // rather than panicking.
        assert!(gql_row_to_prefix(&json!({"id": "abc", "prefix": "10.0.0.0/8"})).is_none());
    }

    #[tokio::test]
    async fn graphql_vrf_bundle_fetches_scoped_children_in_one_query() {
        use crate::config::{ApiConfig, BackendPreference, ProfileConfig};
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // Schema probe: prefix_list + ip_address_list, each with a filters arg
        // and offset pagination.
        let list_field = |name: &str, filter: &str| {
            json!({
                "name": name,
                "args": [
                    {"name": "filters", "type": {"kind": "INPUT_OBJECT", "name": filter}},
                    {"name": "pagination", "type": {"kind": "INPUT_OBJECT", "name": "PaginationInput"}}
                ]
            })
        };
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("__schema"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"__schema": {"queryType": {"fields": [
                    list_field("prefix_list", "PrefixFilter"),
                    list_field("ip_address_list", "IPAddressFilter"),
                ]}}}
            })))
            .mount(&server)
            .await;

        // Filter probe: batch A carries PrefixFilter + IPAddressFilter; both
        // expose a vrf_id lookup so the bundle can scope its children.
        let vrf_id_field =
            json!({"name": "vrf_id", "type": {"kind": "INPUT_OBJECT", "name": "IntegerLookup"}});
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("DeviceFilter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "prefix": {"inputFields": [vrf_id_field.clone()]},
                    "ip": {"inputFields": [vrf_id_field]}
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("ASNFilter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {}})))
            .mount(&server)
            .await;

        // The bundle itself: one POST carrying both list selections.
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("ip_address_list(filters"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "prefix_list": [
                        {"id": "1", "prefix": "10.50.0.0/16", "_depth": 0, "status": "container", "description": "supernet"},
                        {"id": "2", "prefix": "10.50.1.0/24", "_depth": 1, "status": "active", "description": ""}
                    ],
                    "ip_address_list": [
                        {"id": "9", "address": "10.50.1.1/24", "status": "active", "dns_name": "gw.customer", "description": ""}
                    ]
                }
            })))
            .mount(&server)
            .await;

        let client = NetBoxClient::new(
            &ProfileConfig {
                url: server.uri(),
                api: Some(ApiConfig {
                    search: None,
                    vrf: Some(BackendPreference::Graphql),
                }),
                ..Default::default()
            },
            None,
        )
        .unwrap();

        let (prefixes, addresses) = client.graphql_vrf_bundle(42, 500).await.expect("bundle");

        // GraphQL's string ids / plain-string status / `_depth` are reshaped into
        // the REST wire models.
        assert_eq!(prefixes.len(), 2);
        assert_eq!(prefixes[0].id, 1);
        assert_eq!(prefixes[0].prefix, "10.50.0.0/16");
        assert_eq!(prefixes[0].depth, Some(0));
        assert_eq!(
            prefixes[0].status.as_ref().map(|c| c.value.as_str()),
            Some("container")
        );
        assert_eq!(prefixes[1].id, 2);
        assert_eq!(prefixes[1].depth, Some(1));
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].id, 9);
        assert_eq!(addresses[0].address, "10.50.1.1/24");
        assert_eq!(addresses[0].dns_name.as_deref(), Some("gw.customer"));

        // The children come back in a SINGLE round-trip, and both lists are
        // scoped by the resolved vrf id.
        let requests = server.received_requests().await.unwrap();
        let bundles: Vec<_> = requests
            .iter()
            .filter(|r| String::from_utf8_lossy(&r.body).contains("ip_address_list(filters"))
            .collect();
        assert_eq!(bundles.len(), 1, "VRF children must be one bundled POST");
        let body: Value = serde_json::from_slice(&bundles[0].body).unwrap();
        assert_eq!(body["variables"]["pf"]["vrf_id"], json!({"exact": 42}));
        assert_eq!(body["variables"]["if"]["vrf_id"], json!({"exact": 42}));
    }

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
