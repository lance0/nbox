//! Normalized multi-endpoint search.
//!
//! There is no universal NetBox search endpoint, so `nbox search` fans out across
//! several object types in parallel using each endpoint's built-in `q=`
//! quick-search, then merges, ranks, dedups, and truncates.

use std::collections::HashSet;

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::netbox::client::NetBoxClient;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::models::circuits::{Circuit, Provider, VirtualCircuit};
use crate::netbox::models::dcim::{Device, Rack, RackGroup, Site};
use crate::netbox::models::ipam::{
    Aggregate, Asn, IpAddress, IpRange, Prefix, RouteTarget, Vlan, Vrf,
};
use crate::netbox::models::tenancy::{Contact, Tenant};
use crate::netbox::models::virtualization::{Cluster, VirtualMachine, VirtualMachineType};
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
    /// A virtual circuit (NetBox 4.2+). Searchable by CID, openable as a detail
    /// (its terminations), and a cross-navigation target. Carries no site scope,
    /// so scope filters skip it. Kept last to preserve the existing variants'
    /// order.
    VirtualCircuit,
    Aggregate,
    Asn,
    IpRange,
    Tenant,
    Contact,
    Provider,
    /// A virtual machine type (NetBox 4.6+). Searchable by name, openable as a
    /// detail, and a cross-navigation target. Carries no site scope, so scope
    /// filters skip it. Kept last to preserve the existing variants' order.
    VmType,
    Vm,
    Cluster,
    /// A rack. Searchable by name (honoring the site/region/site-group/location
    /// scope), openable in the TUI, and a cross-navigation target (e.g. a device's
    /// rack). Kept last to preserve the existing variants' order.
    Rack,
    /// A rack group (NetBox 4.6+). Searchable by name, openable as a detail, and a
    /// cross-navigation target. Carries no site scope, so scope filters skip it.
    /// Kept last to preserve the existing variants' order.
    RackGroup,
    /// A VRF (routing/forwarding instance). Searchable by name/RD, openable in the
    /// TUI as a routing-context view (its prefix tree + scoped addresses + route
    /// targets), and a cross-navigation target. Carries no site scope, so scope
    /// filters skip it. Kept last to preserve the existing variants' order.
    Vrf,
    /// A route target (BGP extended community, e.g. `65000:100`). Searchable by
    /// name, openable as a detail (the VRFs that import/export it), and the
    /// cross-navigation target the VRF view's targets tab jumps to. Carries no
    /// site scope. Kept last to preserve the existing variants' order.
    RouteTarget,
    /// A device interface. Not part of the global search fan-out (it needs a
    /// device for context) and not Nav-browsable — it exists as a *navigation
    /// target*: a device's interfaces/cables tabs open the interface's detail
    /// (its attributes + cable-path trace), addressed by numeric id. Kept last to
    /// preserve the existing variants' order.
    Interface,
    /// A MAC address (NetBox 4.2+). A reverse-lookup kind, not a search/browse
    /// kind: `nbox mac <addr>` resolves a MAC to the interface(s)/device(s)
    /// that carry it (MACs aren't substring-meaningful, so there's no browse
    /// filter and no search fan-out). Addressed by the MAC string. Kept last to
    /// preserve the existing variants' order.
    Mac,
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
            ObjectKind::VirtualCircuit => "virtual-circuit",
            ObjectKind::Aggregate => "aggregate",
            ObjectKind::Asn => "asn",
            ObjectKind::IpRange => "ip-range",
            ObjectKind::Tenant => "tenant",
            ObjectKind::Contact => "contact",
            ObjectKind::Provider => "provider",
            ObjectKind::VmType => "vm-type",
            ObjectKind::Vm => "vm",
            ObjectKind::Cluster => "cluster",
            ObjectKind::Rack => "rack",
            ObjectKind::RackGroup => "rack-group",
            ObjectKind::Vrf => "vrf",
            ObjectKind::RouteTarget => "route-target",
            ObjectKind::Interface => "interface",
            ObjectKind::Mac => "mac",
        }
    }

    /// Header for the secondary column of a homogeneous browse list — the attribute
    /// [`crate::netbox::browse::browse`] puts in [`SearchResult::subtitle`] for that
    /// kind (a VRF's RD, a route target's tenant, a prefix's/IP's status, a VLAN's
    /// VID). Only the kinds the Nav rail actually browses are reachable here (the
    /// two-column layout is gated on a `browse_kind`); the rest are best-effort and
    /// never rendered. Keep the browsable kinds in sync with `browse.rs`. A *mixed*
    /// search keeps the generic `SITE` header, since one header can't name every
    /// kind's subtitle at once.
    pub fn subtitle_header(self) -> &'static str {
        match self {
            ObjectKind::Device | ObjectKind::Rack => "SITE",
            ObjectKind::Site => "SLUG",
            // Browse shows status for prefixes/IPs (always set) and the bare VID for
            // VLANs; VRFs show the RD, falling back to the tenant when RD-less.
            ObjectKind::Prefix | ObjectKind::IpAddress => "STATUS",
            ObjectKind::Vlan => "VID",
            ObjectKind::Vrf => "RD/TENANT",
            ObjectKind::RouteTarget => "TENANT",
            // Not Nav-browsable today — never rendered; labelled for completeness.
            ObjectKind::Circuit => "PROVIDER",
            ObjectKind::VirtualCircuit => "PROVIDER NETWORK",
            ObjectKind::Aggregate | ObjectKind::Asn => "RIR",
            ObjectKind::IpRange => "VRF",
            ObjectKind::Tenant | ObjectKind::Contact => "GROUP",
            ObjectKind::Provider => "ASN",
            ObjectKind::VmType => "OWNER",
            ObjectKind::Vm => "CLUSTER",
            ObjectKind::Cluster => "TYPE",
            ObjectKind::RackGroup => "RACK COUNT",
            // Not Nav-browsable (no global interface list) — opened only from a
            // device's interfaces/cables tabs; labelled for completeness.
            ObjectKind::Interface => "DEVICE",
            // Not Nav-browsable — a reverse-lookup kind; labelled for completeness.
            ObjectKind::Mac => "ASSIGNED",
        }
    }
}

/// Structured filters for a search, mapped to NetBox query params (by slug/value).
///
/// `site`/`region`/`site_group`/`location` are *scope* filters: NetBox 4.2's
/// prefix `scope` is a single polymorphic type+id, so at most one of them may be
/// set at a time (enforced in [`NetBoxClient::search`]). All four are resolved to
/// a numeric id up front and handled out-of-band per endpoint. Prefixes/clusters
/// use exact `scope_type=dcim.site` + `scope_id=<id>` for `--site`, and NetBox's
/// tree-aware `region_id`/`site_group_id`/`location_id` filters for the non-site
/// scopes. Other endpoints use the clean `site_id`/`region_id`/… filter when
/// available. Scope filters never flow through the plain-value allowlist below
/// (the plain `?site=` param wants a slug, so an id or display name would
/// silently match nothing).
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
    /// Owner (NetBox 4.5+) — a user, by name (username). Sent as `owner=`; an
    /// endpoint that carries no `owner` filter is skipped when this is set.
    /// Orthogonal to the scope filters and to `owner_group`.
    pub owner: Option<String>,
    /// Owner group (NetBox 4.5+) — by name. Sent as `owner_group=`; orthogonal to
    /// `owner`. (Owner is polymorphic: a user *or* a group, so the two are
    /// separate filters rather than one ambiguous ref.)
    pub owner_group: Option<String>,
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
    /// endpoint (`scope_type`+`scope_id` for exact `--site` on scoped models,
    /// tree-aware `region_id`/`site_group_id`/`location_id` for non-site scopes,
    /// or `site_id`/`region_id`/… on other endpoints). The plain `?site=` param
    /// expects a *slug*, so a `--site` given as an id or display name would
    /// silently match nothing; the resolved id avoids that.
    fn params_for(&self, supported: &[&str]) -> Option<Vec<(&'static str, String)>> {
        let active: [(&'static str, &Option<String>); 6] = [
            ("status", &self.status),
            ("tenant", &self.tenant),
            ("role", &self.role),
            ("tag", &self.tag),
            ("owner", &self.owner),
            ("owner_group", &self.owner_group),
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

/// Floor for the per-endpoint search row cap. Each search branch fetches at
/// most `min(page_size, max(req.limit, SEARCH_BRANCH_FLOOR))` rows — so a
/// `--limit 25` search pulls 25 per endpoint (not the full `page_size`), while a
/// tiny `--limit 5` still fetches `FLOOR` so the merge has enough candidates to
/// rank across endpoints (nbox re-sorts by `score_match`, which is coarser than
/// NetBox's own `q` relevance, so a narrow per-branch cap could miss a
/// high-scoring row the server ranked lower). 25 matches the CLI/MCP default
/// `--limit` and is wide enough that the top result set is stable in practice.
pub(crate) const SEARCH_BRANCH_FLOOR: usize = 25;

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

fn scoped_model_params(scope: Option<&ResolvedScope>) -> Result<Vec<(&'static str, String)>> {
    let Some(s) = scope else {
        return Ok(Vec::new());
    };
    match s.content_type {
        // Preserve exact site semantics. The polymorphic `scope` exact match is
        // deliberately narrower than NetBox's `_site` helper and avoids widening
        // `--site` if scoped models ever grow indirect site inheritance.
        "dcim.site" => Ok(vec![
            ("scope_type", s.content_type.to_string()),
            ("scope_id", s.id.to_string()),
        ]),
        // NetBox's ScopedFilterSet backs these with TreeNodeMultipleChoiceFilter,
        // so the selected node and its descendants are matched server-side.
        "dcim.region" => Ok(vec![("region_id", s.id.to_string())]),
        "dcim.sitegroup" => Ok(vec![("site_group_id", s.id.to_string())]),
        "dcim.location" => Ok(vec![("location_id", s.id.to_string())]),
        other => anyhow::bail!(
            "unsupported scoped-model search scope {other}; refusing an unscoped query"
        ),
    }
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
/// The REST filter params each search endpoint accepts (alongside `q`).
///
/// Single source of truth for the search fan-out: every `search_*` branch passes
/// its endpoint's slice here, so the supported-filter set can't drift from what
/// the schema canary (``mod schema_canary`` below) validates against a pinned
/// NetBox OpenAPI snapshot. The scope filters (`site`/`region`/`site_group`/
/// `location`) are *not* here — they're resolved to an id once and applied
/// out-of-band per endpoint (`scope_type`+`scope_id` for exact `--site` on
/// scoped models, native scoped id filters for non-site scopes, or
/// `site_id`/`region_id`/… elsewhere) by each branch, never as the raw `?site=`
/// slug.
///
/// `tag`/`owner`/`owner_group` are common to most kinds; `status`/`tenant`/
/// `role` vary. Returns an empty slice for endpoints that aren't part of the
/// search fan-out (the canary skips those).
fn search_supported(ep: Endpoint) -> &'static [&'static str] {
    match ep {
        Endpoint::Devices
        | Endpoint::IpAddresses
        | Endpoint::Vlans
        | Endpoint::IpRanges
        | Endpoint::VirtualMachines
        | Endpoint::Racks => &["status", "tenant", "role", "tag", "owner", "owner_group"],
        Endpoint::Sites | Endpoint::Circuits | Endpoint::VirtualCircuits => {
            &["status", "tenant", "tag", "owner", "owner_group"]
        }
        Endpoint::Aggregates | Endpoint::Asns | Endpoint::Vrfs | Endpoint::RouteTargets => {
            &["tenant", "tag", "owner", "owner_group"]
        }
        Endpoint::Tenants | Endpoint::Contacts | Endpoint::Providers => {
            &["tag", "owner", "owner_group"]
        }
        // 4.6 kinds: rack-groups and vm-types carry no `tenant` filter in the
        // OpenAPI schema (verified against 4.6.2). Declaring it here would send
        // a `tenant=` NetBox silently ignores, returning the kind *unfiltered* —
        // a silent over-broad result. The schema canary pins this.
        Endpoint::RackGroups | Endpoint::VirtualMachineTypes => &["tag", "owner", "owner_group"],
        // Prefixes and clusters apply scope out-of-band, so they pass a
        // scope-stripped filters variant and a smaller in-band set (no
        // owner/role — those aren't sent in-band on these branches).
        Endpoint::Prefixes => &["status", "tenant", "role", "tag"],
        Endpoint::Clusters => &["status", "tenant", "tag"],
        _ => &[],
    }
}

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
        // up front. Prefixes/clusters keep exact `--site` semantics via
        // `scope_type`+`scope_id`, while non-site scopes use NetBox's native
        // tree-aware scoped id filters. An unknown ref is a hard not-found error
        // (exit 4) so search fails loudly rather than quietly returning nothing.
        // Resolve the (optional) `--vrf` reference (id | rd | name) to a numeric
        // id once, up front. An unknown VRF is a hard not-found error (exit 4) so
        // search fails loudly rather than quietly returning nothing — matching the
        // scope-filter behavior. The resolved id is applied as `vrf_id=` on the
        // VRF-capable endpoints (IPs, prefixes); the rest skip the vrf filter.
        // `--vrf` is orthogonal to the scope filters: both may be active at once.
        //
        // The scope and VRF resolvers are independent and each can make 1-4
        // round-trips, so run them concurrently — a `--scope` + `--vrf` search would
        // otherwise pay both serial tails before the fan-out even starts. (With
        // neither filter set both return `Ok(None)` after zero network calls.)
        let (scope, vrf_id) = tokio::try_join!(self.resolve_scope(f), self.resolve_vrf(f))?;

        // Cap each branch's per-endpoint fetch at `min(page_size, max(req.limit,
        // SEARCH_BRANCH_FLOOR))` so a `--limit 25` search deserializes ~25 rows
        // per endpoint (not the full `page_size` 100) — the merge truncates to
        // `req.limit` anyway, so the extra rows are ranked only to be thrown away.
        // The floor keeps each branch wide enough to contribute its share of the
        // merged result (nbox's `score_match` is coarser than NetBox's `q`
        // relevance, so a cap below the floor could miss a high-scoring row the
        // server ranked lower). See [`SEARCH_BRANCH_FLOOR`].
        let branch_limit = self.page_size().min(req.limit.max(SEARCH_BRANCH_FLOOR));

        let (
            devices,
            sites,
            ips,
            prefixes,
            vlans,
            circuits,
            virtual_circuits,
            aggregates,
            asns,
            ip_ranges,
            tenants,
            contacts,
            providers,
            vms,
            vm_types,
            clusters,
            racks,
            rack_groups,
            vrfs,
            route_targets,
        ) = tokio::join!(
            self.search_devices(&q, f, scope.as_ref(), branch_limit),
            self.search_sites(&q, f, scope.as_ref(), branch_limit),
            self.search_ips(&q, f, scope.as_ref(), vrf_id, branch_limit),
            self.search_prefixes(&q, f, scope.as_ref(), vrf_id, branch_limit),
            self.search_vlans(&q, f, scope.as_ref(), branch_limit),
            self.search_circuits(&q, f, scope.as_ref(), branch_limit),
            self.search_virtual_circuits(&q, f, scope.as_ref(), branch_limit),
            self.search_aggregates(&q, f, scope.as_ref(), branch_limit),
            self.search_asns(&q, f, scope.as_ref(), branch_limit),
            self.search_ip_ranges(&q, f, scope.as_ref(), branch_limit),
            self.search_tenants(&q, f, scope.as_ref(), branch_limit),
            self.search_contacts(&q, f, scope.as_ref(), branch_limit),
            self.search_providers(&q, f, scope.as_ref(), branch_limit),
            self.search_vms(&q, f, scope.as_ref(), branch_limit),
            self.search_vm_types(&q, f, scope.as_ref(), branch_limit),
            self.search_clusters(&q, f, scope.as_ref(), branch_limit),
            self.search_racks(&q, f, scope.as_ref(), branch_limit),
            self.search_rack_groups(&q, f, scope.as_ref(), branch_limit),
            self.search_vrfs(&q, f, scope.as_ref(), branch_limit),
            self.search_route_targets(&q, f, scope.as_ref(), branch_limit),
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
            ("virtual-circuits", virtual_circuits),
            ("aggregates", aggregates),
            ("asns", asns),
            ("ip-ranges", ip_ranges),
            ("tenants", tenants),
            ("contacts", contacts),
            ("providers", providers),
            ("vms", vms),
            ("vm-types", vm_types),
            ("clusters", clusters),
            ("racks", racks),
            ("rack-groups", rack_groups),
            ("vrfs", vrfs),
            ("route-targets", route_targets),
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

    async fn search_devices(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
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
        let Some(mut params) = endpoint_params(q, f, search_supported(Endpoint::Devices)) else {
            return Ok(Vec::new());
        };
        if let Some((key, id)) = device_scope {
            params.push((key, id.to_string()));
        }
        let page: Page<Device> = self.list_limited(Endpoint::Devices, params, limit).await?;
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
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // The site search itself carries no scope filter (a site has no parent
        // site/region/site-group/location filter on this endpoint that maps to our
        // scope flags cleanly), so any active scope skips it rather than sending a
        // dead param.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(params) = endpoint_params(q, f, search_supported(Endpoint::Sites)) else {
            return Ok(Vec::new());
        };
        let page: Page<Site> = self.list_limited(Endpoint::Sites, params, limit).await?;
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
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // IPs carry no scope filter that maps to our flags — any active scope
        // (including `--site`) skips them.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(mut params) = endpoint_params(q, f, search_supported(Endpoint::IpAddresses))
        else {
            return Ok(Vec::new());
        };
        // IPs carry a VRF: apply the resolved `--vrf` id as `vrf_id=`.
        if let Some(id) = vrf_id {
            params.push(("vrf_id", id.to_string()));
        }
        let page: Page<IpAddress> = self
            .list_limited(Endpoint::IpAddresses, params, limit)
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

    async fn search_prefixes(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        vrf_id: Option<u64>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // Scope is handled out-of-band, not through the allowlist. NetBox 4.2
        // dropped the prefix `site` FK for the polymorphic `scope`, so scope refs
        // are resolved to ids up front, cleared before the allowlist check, and
        // re-expressed below. `--site` stays exact via `scope_type`+`scope_id`;
        // region/site-group/location use NetBox's tree-aware scoped id filters
        // (`region_id`, `site_group_id`, `location_id`) so descendants are
        // included by the server without a client-side hierarchy crawl.
        let without_scope = SearchFilters {
            site: None,
            region: None,
            site_group: None,
            location: None,
            ..f.clone()
        };
        let Some(mut params) =
            endpoint_params(q, &without_scope, search_supported(Endpoint::Prefixes))
        else {
            return Ok(Vec::new());
        };
        params.extend(scoped_model_params(scope)?);
        // Prefixes carry a VRF: apply the resolved `--vrf` id as `vrf_id=`. This
        // is orthogonal to scope — NetBox ANDs them, so a vrf+scope combo narrows
        // to prefixes matching both.
        if let Some(id) = vrf_id {
            params.push(("vrf_id", id.to_string()));
        }
        let page: Page<Prefix> = self.list_limited(Endpoint::Prefixes, params, limit).await?;
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
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // VLANs honor `--site` via the resolved `site_id`. NetBox's VLAN region/
        // site-group filters exist but aren't uniformly clean (no location scope),
        // so skip VLANs for any id-based scope rather than apply an inconsistent
        // subset — matching the conservative "skip if unsure" rule.
        if skip_for_id_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(mut params) = endpoint_params(q, f, search_supported(Endpoint::Vlans)) else {
            return Ok(Vec::new());
        };
        // Only a `dcim.site` scope reaches here (the id-based scopes skipped above):
        // filter by the resolved `site_id`, not the slug-only `?site=`.
        if let Some(s) = scope {
            params.push(("site_id", s.id.to_string()));
        }
        let page: Page<Vlan> = self.list_limited(Endpoint::Vlans, params, limit).await?;
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
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // Circuits carry no scope filter that maps to our flags — any active scope
        // (including `--site`) skips them.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(params) = endpoint_params(q, f, search_supported(Endpoint::Circuits)) else {
            return Ok(Vec::new());
        };
        let page: Page<Circuit> = self.list_limited(Endpoint::Circuits, params, limit).await?;
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

    async fn search_virtual_circuits(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // Virtual circuits carry no scope filter that maps to our flags — any
        // active scope (including `--site`) skips them.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(params) = endpoint_params(q, f, search_supported(Endpoint::VirtualCircuits))
        else {
            return Ok(Vec::new());
        };
        let page: Page<VirtualCircuit> = self
            .list_limited(Endpoint::VirtualCircuits, params, limit)
            .await?;
        Ok(page
            .results
            .into_iter()
            .map(|vc| SearchResult {
                kind: ObjectKind::VirtualCircuit,
                id: vc.id,
                score: score_match(q, &vc.cid),
                subtitle: vc
                    .provider_network
                    .as_ref()
                    .map(super::models::common::BriefObject::label),
                url: api_to_web_url(&vc.url),
                display: vc.cid,
            })
            .collect())
    }

    async fn search_aggregates(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // Aggregates carry no scope filter that maps to our flags — any active
        // scope (including `--site`) skips them.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(params) = endpoint_params(q, f, search_supported(Endpoint::Aggregates)) else {
            return Ok(Vec::new());
        };
        let page: Page<Aggregate> = self
            .list_limited(Endpoint::Aggregates, params, limit)
            .await?;
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
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // ASNs carry no scope filter that maps to our flags — any active scope
        // (including `--site`) skips them.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(mut params) = endpoint_params(q, f, search_supported(Endpoint::Asns)) else {
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
        let page: Page<Asn> = self.list_limited(Endpoint::Asns, params, limit).await?;
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
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // IP ranges carry no scope filter that maps to our flags — any active
        // scope (including `--site`) skips them.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(params) = endpoint_params(q, f, search_supported(Endpoint::IpRanges)) else {
            return Ok(Vec::new());
        };
        let page: Page<IpRange> = self.list_limited(Endpoint::IpRanges, params, limit).await?;
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
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // Tenants carry no scope filter (site/region/site-group/location) — skip
        // them for any active scope rather than return an unfiltered set.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        // The tenant endpoint accepts only `q` + `tag` from our filter set
        // (no status/tenant/role), so an unsupported active filter skips it.
        let Some(params) = endpoint_params(q, f, search_supported(Endpoint::Tenants)) else {
            return Ok(Vec::new());
        };
        let page: Page<Tenant> = self.list_limited(Endpoint::Tenants, params, limit).await?;
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
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // Contacts carry no scope filter — skip them for any active scope.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        // Contacts accept only `q` + `tag` (no status/tenant/role).
        let Some(params) = endpoint_params(q, f, search_supported(Endpoint::Contacts)) else {
            return Ok(Vec::new());
        };
        let page: Page<Contact> = self.list_limited(Endpoint::Contacts, params, limit).await?;
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
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // Providers carry no scope filter (site/region/site-group/location) — skip
        // them for any active scope rather than return an unfiltered set.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        // Providers accept only `q` + `tag` (no status/tenant/role).
        let Some(params) = endpoint_params(q, f, search_supported(Endpoint::Providers)) else {
            return Ok(Vec::new());
        };
        let page: Page<Provider> = self
            .list_limited(Endpoint::Providers, params, limit)
            .await?;
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
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // VMs honor `--site` via the resolved `site_id`; for the id-based scopes
        // (region/site-group/location) the VM filters aren't uniformly clean (no
        // location scope), so skip them rather than apply an inconsistent subset.
        if skip_for_id_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(mut params) = endpoint_params(q, f, search_supported(Endpoint::VirtualMachines))
        else {
            return Ok(Vec::new());
        };
        // Only a `dcim.site` scope reaches here (the id-based scopes skipped above):
        // filter by the resolved `site_id`, not the slug-only `?site=`.
        if let Some(s) = scope {
            params.push(("site_id", s.id.to_string()));
        }
        let page: Page<VirtualMachine> = self
            .list_limited(Endpoint::VirtualMachines, params, limit)
            .await?;
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

    async fn search_vm_types(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // VM types carry no scope filter (site/region/site-group/location) —
        // skip them for any active scope rather than return an unfiltered set.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(params) = endpoint_params(q, f, search_supported(Endpoint::VirtualMachineTypes))
        else {
            return Ok(Vec::new());
        };
        // `virtual-machine-types/` is a NetBox 4.6+ endpoint. On an older
        // release it 404s — treat that as "this kind isn't available here" and
        // return empty, so the search fan-out stays green on 4.2–4.5 instead
        // of failing closed on a version-gated endpoint miss.
        let page: Page<VirtualMachineType> = match self
            .list_limited(Endpoint::VirtualMachineTypes, params, limit)
            .await
        {
            Ok(page) => page,
            Err(e) if crate::error::is_not_found(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(page
            .results
            .into_iter()
            .map(|t| SearchResult {
                kind: ObjectKind::VmType,
                id: t.id,
                score: score_match(q, &t.name),
                subtitle: t
                    .owner
                    .as_ref()
                    .map(super::models::common::BriefObject::label),
                url: api_to_web_url(&t.url),
                display: t.name,
            })
            .collect())
    }

    async fn search_clusters(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // NetBox 4.2+ scopes a cluster polymorphically, like prefixes. Clear the
        // scope refs before the allowlist check, then apply the same exact-site /
        // descendant-aware non-site scope params as `search_prefixes`.
        let without_scope = SearchFilters {
            site: None,
            region: None,
            site_group: None,
            location: None,
            ..f.clone()
        };
        // Clusters accept `status`/`tenant`/`tag` (no `role`); scope is applied
        // out-of-band below.
        let Some(mut params) =
            endpoint_params(q, &without_scope, search_supported(Endpoint::Clusters))
        else {
            return Ok(Vec::new());
        };
        params.extend(scoped_model_params(scope)?);
        let page: Page<Cluster> = self.list_limited(Endpoint::Clusters, params, limit).await?;
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
        limit: usize,
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
        let Some(mut params) = endpoint_params(q, f, search_supported(Endpoint::Racks)) else {
            return Ok(Vec::new());
        };
        if let Some((key, id)) = rack_scope {
            params.push((key, id.to_string()));
        }
        let page: Page<Rack> = self.list_limited(Endpoint::Racks, params, limit).await?;
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

    async fn search_rack_groups(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // Rack groups carry no scope filter that maps to our flags — any active
        // scope (including `--site`) skips them.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        let Some(params) = endpoint_params(q, f, search_supported(Endpoint::RackGroups)) else {
            return Ok(Vec::new());
        };
        // `rack-groups/` is a NetBox 4.6+ endpoint; on an older release it 404s.
        // Swallow that as an empty result so the search fan-out stays green on
        // 4.2–4.5 (the kind is simply absent), rather than fail closed on a
        // version-gated endpoint miss.
        let page: Page<RackGroup> =
            match self.list_limited(Endpoint::RackGroups, params, limit).await {
                Ok(page) => page,
                Err(e) if crate::error::is_not_found(&e) => return Ok(Vec::new()),
                Err(e) => return Err(e),
            };
        Ok(page
            .results
            .into_iter()
            .map(|rg| SearchResult {
                kind: ObjectKind::RackGroup,
                id: rg.id,
                score: score_match(q, &rg.name),
                subtitle: rg
                    .owner
                    .as_ref()
                    .map(super::models::common::BriefObject::label),
                url: api_to_web_url(&rg.url),
                display: rg.name,
            })
            .collect())
    }

    async fn search_vrfs(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // VRFs carry no site scope (site/region/site-group/location) — skip them
        // for any active scope rather than return an unfiltered set.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        // The VRF endpoint accepts `q` + `tenant` + `tag` from our filter set
        // (no status/role/site), so an unsupported active filter skips it.
        let Some(params) = endpoint_params(q, f, search_supported(Endpoint::Vrfs)) else {
            return Ok(Vec::new());
        };
        let page: Page<Vrf> = self.list_limited(Endpoint::Vrfs, params, limit).await?;
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

    async fn search_route_targets(
        &self,
        q: &str,
        f: &SearchFilters,
        scope: Option<&ResolvedScope>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // Route targets carry no site scope — skip them for any active scope.
        if skip_for_any_scope(scope) {
            return Ok(Vec::new());
        }
        // The route-target endpoint accepts `q` + `tenant` + `tag` (no
        // status/role/site), so an unsupported active filter skips it.
        let Some(params) = endpoint_params(q, f, search_supported(Endpoint::RouteTargets)) else {
            return Ok(Vec::new());
        };
        let page: Page<RouteTarget> = self
            .list_limited(Endpoint::RouteTargets, params, limit)
            .await?;
        Ok(page
            .results
            .into_iter()
            .map(|rt| SearchResult {
                kind: ObjectKind::RouteTarget,
                id: rt.id,
                score: score_match(q, &rt.name),
                subtitle: rt
                    .tenant
                    .as_ref()
                    .map(super::models::common::BriefObject::label),
                url: api_to_web_url(&rt.url),
                display: rt.name,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Schema-drift canary -----------------------------------------------
    //
    // Pins the search fan-out's declared filter set (`search_supported`)
    // against a compact NetBox OpenAPI snapshot embedded at compile time
    // (`tests/schema/netbox-4.6.2.json`). Refresh the snapshot against a new
    // NetBox release (scripts/gen_schema_snapshot.py from /api/schema/) and
    // the canary immediately flags any endpoint/filter nbox uses that the new
    // release dropped — the manual schema-curling this replaces.

    /// `(Endpoint, api_path)` for every endpoint the search fan-out hits. The
    /// path is the snapshot key and must match `Endpoint::path()` exactly.
    const CANARY_EXPECTED: &[(Endpoint, &str)] = &[
        (Endpoint::Devices, "/api/dcim/devices/"),
        (Endpoint::Sites, "/api/dcim/sites/"),
        (Endpoint::IpAddresses, "/api/ipam/ip-addresses/"),
        (Endpoint::Prefixes, "/api/ipam/prefixes/"),
        (Endpoint::Vlans, "/api/ipam/vlans/"),
        (Endpoint::Circuits, "/api/circuits/circuits/"),
        (Endpoint::VirtualCircuits, "/api/circuits/virtual-circuits/"),
        (Endpoint::Aggregates, "/api/ipam/aggregates/"),
        (Endpoint::Asns, "/api/ipam/asns/"),
        (Endpoint::IpRanges, "/api/ipam/ip-ranges/"),
        (Endpoint::Tenants, "/api/tenancy/tenants/"),
        (Endpoint::Contacts, "/api/tenancy/contacts/"),
        (Endpoint::Providers, "/api/circuits/providers/"),
        (
            Endpoint::VirtualMachines,
            "/api/virtualization/virtual-machines/",
        ),
        (
            Endpoint::VirtualMachineTypes,
            "/api/virtualization/virtual-machine-types/",
        ),
        (Endpoint::Clusters, "/api/virtualization/clusters/"),
        (Endpoint::Racks, "/api/dcim/racks/"),
        (Endpoint::RackGroups, "/api/dcim/rack-groups/"),
        (Endpoint::Vrfs, "/api/ipam/vrfs/"),
        (Endpoint::RouteTargets, "/api/ipam/route-targets/"),
    ];

    #[test]
    fn schema_canary_search_filters_match_pinned_openapi_snapshot() {
        let snapshot: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/schema/netbox-4.6.2.json"))
                .expect("pinned snapshot is valid JSON");

        // The snapshot holds exactly the fan-out endpoints (plus `_meta`).
        let mut snap_keys: std::collections::HashSet<String> = snapshot
            .as_object()
            .expect("snapshot is an object")
            .keys()
            .cloned()
            .collect();
        snap_keys.remove("_meta");
        let expected_paths: std::collections::HashSet<String> =
            CANARY_EXPECTED.iter().map(|(_, p)| p.to_string()).collect();
        assert_eq!(
            snap_keys, expected_paths,
            "snapshot endpoints diverge from the fan-out set — refresh the snapshot \
             (scripts/gen_schema_snapshot.py) and reconcile search_supported"
        );

        for (ep, path) in CANARY_EXPECTED {
            // The snapshot is keyed by the canonical path, so a `path()` change
            // shows up here too.
            assert_eq!(
                ep.path(),
                *path,
                "CANARY_EXPECTED path drifts from Endpoint::path()"
            );
            let supported = search_supported(*ep);
            assert!(
                !supported.is_empty(),
                "{path}: search_supported({ep:?}) is empty — endpoint forgotten in the \
                 table (it would silently send no filters)"
            );
            let params = snapshot[path]
                .as_array()
                .unwrap_or_else(|| panic!("{path}: snapshot entry is not a param list"));
            let param_set: std::collections::HashSet<&str> =
                params.iter().filter_map(|v| v.as_str()).collect();
            for filter in supported {
                assert!(
                    param_set.contains(*filter),
                    "{path}: search_supported declares `{filter}` but the pinned NetBox \
                     snapshot has no such GET param on this endpoint — the server would \
                     silently ignore it (over-broad results). Fix search_supported or \
                     refresh the snapshot."
                );
            }
        }
    }

    #[test]
    fn schema_canary_scoped_models_keep_tree_scope_filters() {
        let snapshot: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/schema/netbox-4.6.2.json"))
                .expect("pinned snapshot is valid JSON");
        for path in ["/api/ipam/prefixes/", "/api/virtualization/clusters/"] {
            let params = snapshot[path]
                .as_array()
                .unwrap_or_else(|| panic!("{path}: snapshot entry is not a param list"));
            let param_set: std::collections::HashSet<&str> =
                params.iter().filter_map(|v| v.as_str()).collect();
            for filter in ["region_id", "site_group_id", "site_id", "location_id"] {
                assert!(
                    param_set.contains(filter),
                    "{path}: missing `{filter}` in the pinned NetBox snapshot; \
                     search scope filtering relies on ScopedFilterSet's tree-aware id filters"
                );
            }
        }
    }

    #[test]
    fn schema_canary_supported_table_matches_fanout_set_exactly() {
        // Every endpoint in the fan-out set returns a non-empty slice, and every
        // other Endpoint variant returns empty — so the table tracks the fan-out
        // exactly. A new endpoint added to the fan-out but not to `search_supported`
        // (or vice versa) fails here.
        let all = [
            Endpoint::Devices,
            Endpoint::Interfaces,
            Endpoint::FrontPorts,
            Endpoint::Sites,
            Endpoint::Regions,
            Endpoint::SiteGroups,
            Endpoint::Locations,
            Endpoint::Racks,
            Endpoint::RackGroups,
            Endpoint::IpAddresses,
            Endpoint::Prefixes,
            Endpoint::Vlans,
            Endpoint::VlanGroups,
            Endpoint::Vrfs,
            Endpoint::RouteTargets,
            Endpoint::Tenants,
            Endpoint::Contacts,
            Endpoint::VirtualMachines,
            Endpoint::VirtualMachineTypes,
            Endpoint::Clusters,
            Endpoint::Circuits,
            Endpoint::CircuitTerminations,
            Endpoint::VirtualCircuits,
            Endpoint::VirtualCircuitTerminations,
            Endpoint::Providers,
            Endpoint::Aggregates,
            Endpoint::Asns,
            Endpoint::Services,
            Endpoint::IpRanges,
            Endpoint::JournalEntries,
            Endpoint::Tags,
            Endpoint::TaggedObjects,
            Endpoint::MacAddresses,
        ];
        let in_fanout = |e: Endpoint| CANARY_EXPECTED.iter().any(|(x, _)| *x == e);
        for e in all {
            let nonempty = !search_supported(e).is_empty();
            let in_set = in_fanout(e);
            assert_eq!(
                nonempty, in_set,
                "{e:?}: search_supported non-empty={nonempty} but fan-out membership={in_set} \
                 — keep the table in sync with the search fan-out",
            );
        }
    }

    #[test]
    fn scoring_orders_exact_prefix_contains() {
        assert!(score_match("edge01", "edge01") > score_match("edge", "edge01"));
        assert!(score_match("edge", "edge01") > score_match("dge", "edge01"));
        assert!(score_match("dge", "edge01") > score_match("zzz", "edge01"));
    }

    #[test]
    fn subtitle_header_names_each_kinds_secondary_field() {
        // The browsable kinds' headers name exactly what `browse.rs` puts in the
        // subtitle, so the header and the values under it agree: prefixes/IPs show
        // status, VLANs their VID, VRFs the RD (tenant fallback), route targets the
        // tenant. (These four are the kinds the Nav rail actually browses.)
        assert_eq!(ObjectKind::Prefix.subtitle_header(), "STATUS");
        assert_eq!(ObjectKind::IpAddress.subtitle_header(), "STATUS");
        assert_eq!(ObjectKind::Vlan.subtitle_header(), "VID");
        assert_eq!(ObjectKind::Vrf.subtitle_header(), "RD/TENANT");
        assert_eq!(ObjectKind::RouteTarget.subtitle_header(), "TENANT");
        // Site-bearing kinds keep "SITE"; sites show their slug.
        assert_eq!(ObjectKind::Device.subtitle_header(), "SITE");
        assert_eq!(ObjectKind::Rack.subtitle_header(), "SITE");
        assert_eq!(ObjectKind::Site.subtitle_header(), "SLUG");
        // Every kind yields a short, non-empty, uppercase header.
        for kind in [
            ObjectKind::Device,
            ObjectKind::Site,
            ObjectKind::IpAddress,
            ObjectKind::Prefix,
            ObjectKind::Vlan,
            ObjectKind::Circuit,
            ObjectKind::Aggregate,
            ObjectKind::Asn,
            ObjectKind::IpRange,
            ObjectKind::Tenant,
            ObjectKind::Contact,
            ObjectKind::Provider,
            ObjectKind::Vm,
            ObjectKind::Cluster,
            ObjectKind::Rack,
            ObjectKind::Vrf,
            ObjectKind::RouteTarget,
            ObjectKind::Interface,
        ] {
            let h = kind.subtitle_header();
            assert!(!h.is_empty(), "{kind:?} has an empty subtitle header");
            assert_eq!(h, h.to_uppercase(), "{kind:?} header should be uppercase");
        }
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
    fn scoped_model_params_fail_closed_for_unknown_scope_type() {
        assert_eq!(
            scoped_model_params(None).unwrap(),
            Vec::<(&'static str, String)>::new()
        );
        assert_eq!(
            scoped_model_params(Some(&ResolvedScope {
                content_type: "dcim.region",
                id: 3,
            }))
            .unwrap(),
            vec![("region_id", "3".to_string())]
        );

        let err = scoped_model_params(Some(&ResolvedScope {
            content_type: "extras.gadget",
            id: 99,
        }))
        .expect_err("unsupported scopes must not become unscoped queries");
        assert!(
            format!("{err:#}").contains("refusing an unscoped query"),
            "got: {err:#}"
        );
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
