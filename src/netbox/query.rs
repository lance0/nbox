//! Endpoint-specific query helpers on [`NetBoxClient`].
//!
//! These resolve user-facing references (names, slugs, IDs) into model objects.

use anyhow::Result;

use crate::error::NboxError;
use crate::netbox::client::NetBoxClient;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::models::circuits::{
    Circuit, Provider, VirtualCircuit, VirtualCircuitTermination,
};
use crate::netbox::models::dcim::{
    Device, Interface, Location, MacAddress, Rack, RackGroup, Region, Site, SiteGroup,
};
use crate::netbox::models::extras::{JournalEntry, ObjectChange, TagInfo, TaggedObject};
use crate::netbox::models::ipam::{
    Aggregate, Asn, AvailableIp, AvailablePrefix, IpAddress, IpRange, Prefix, RouteTarget, Service,
    Vlan, VlanGroup, Vrf,
};
use crate::netbox::models::tenancy::{Contact, Tenant};
use crate::netbox::models::virtualization::{Cluster, VirtualMachine, VirtualMachineType};
use crate::netbox::pagination::Page;

/// `limit` sent on the `available-prefixes` GET. Without an explicit `limit`
/// NetBox returns its default of 50 free blocks, which silently truncates the
/// candidate set for a large/fragmented parent (a fitting block past the 50th
/// would be missed by the client-side `--length` filter). 1000 is NetBox's
/// server-side cap, so it requests a full page without an unbounded list.
const AVAILABLE_PREFIXES_LIMIT: usize = 1000;

/// Resolve a fuzzy (name-contains) result set: the single match, `None` if empty,
/// or an [`NboxError::Ambiguous`] listing the candidates when more than one match.
fn ambiguous_or_first<T>(
    noun: &str,
    value: &str,
    results: Vec<T>,
    label: impl Fn(&T) -> String,
) -> Result<Option<T>> {
    if results.len() > 1 {
        let matches = results
            .iter()
            .take(8)
            .map(&label)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(NboxError::Ambiguous {
            noun: noun.to_string(),
            value: value.to_string(),
            matches,
        }
        .into());
    }
    Ok(results.into_iter().next())
}

/// Map a NetBox scope content type (`scope_type`) to a friendly label, e.g.
/// `dcim.site` → `site`, `dcim.location` → `location`, `dcim.region` → `region`,
/// `dcim.sitegroup` → `site-group`. Unknown content types pass through verbatim,
/// so a NetBox release that adds a new scope kind still gets a usable label.
pub(crate) fn friendly_scope_type(content_type: &str) -> String {
    match content_type {
        "dcim.site" => "site".to_string(),
        "dcim.location" => "location".to_string(),
        "dcim.region" => "region".to_string(),
        "dcim.sitegroup" => "site-group".to_string(),
        other => other.to_string(),
    }
}

/// Disambiguation label for a prefix, e.g. `10.0.0.0/24 (vrf: blue)` / `(global)`.
pub(crate) fn prefix_scope_label(p: &Prefix) -> String {
    match &p.vrf {
        Some(v) => format!("{} (vrf: {})", p.prefix, v.label()),
        None => format!("{} (global)", p.prefix),
    }
}

/// Disambiguation label for a VLAN, e.g. `208 users (site: iad1)`. Prefers a
/// polymorphic `scope` (labelled by its friendly type) when present, falling back
/// to the direct `site`, then the VLAN `group`.
pub(crate) fn vlan_scope_label(v: &Vlan) -> String {
    let scope = match (&v.scope, &v.scope_type, &v.site, &v.group) {
        (Some(s), Some(t), _, _) => format!(" ({}: {})", friendly_scope_type(t), s.label()),
        (Some(s), None, _, _) => format!(" (scope: {})", s.label()),
        (None, _, Some(s), _) => format!(" (site: {})", s.label()),
        (None, _, None, Some(g)) => format!(" (group: {})", g.label()),
        (None, _, None, None) => String::new(),
    };
    format!("{} {}{}", v.vid, v.name, scope)
}

/// Disambiguation label for an IP, e.g. `10.0.0.1/24 (vrf: blue)` / `(global)`.
pub(crate) fn ip_scope_label(ip: &IpAddress) -> String {
    match &ip.vrf {
        Some(v) => format!("{} (vrf: {})", ip.address, v.label()),
        None => format!("{} (global)", ip.address),
    }
}

impl NetBoxClient {
    /// Resolve a device by numeric ID, then exact (case-insensitive) name, then
    /// a name-contains fallback. Returns `None` when nothing matches a name.
    pub async fn device_by_ref(&self, value: &str) -> Result<Option<Device>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(
                    &format!("/api/dcim/devices/{id}/"),
                    &[("exclude", "config_context".to_string())],
                )
                .await;
        }

        let exact: Page<Device> = self
            .list(Endpoint::Devices, vec![("name__ie", value.to_string())])
            .await?;
        if let Some(device) = exact.results.into_iter().next() {
            return Ok(Some(device));
        }

        let contains: Page<Device> = self
            .list(Endpoint::Devices, vec![("name__ic", value.to_string())])
            .await?;
        ambiguous_or_first("device", value, contains.results, |d| d.name.clone())
    }

    /// All interfaces on a device (up to `max`).
    pub async fn device_interfaces(&self, device_id: u64, max: usize) -> Result<Vec<Interface>> {
        self.list_all(
            Endpoint::Interfaces,
            vec![("device_id", device_id.to_string())],
            max,
        )
        .await
    }

    /// Services declared on a device (up to `max`).
    pub async fn device_services(&self, device_id: u64, max: usize) -> Result<Vec<Service>> {
        self.list_all(
            Endpoint::Services,
            vec![("device_id", device_id.to_string())],
            max,
        )
        .await
    }

    /// All IP addresses assigned on a device (up to `max`).
    pub async fn device_ips(&self, device_id: u64, max: usize) -> Result<Vec<IpAddress>> {
        self.list_all(
            Endpoint::IpAddresses,
            vec![("device_id", device_id.to_string())],
            max,
        )
        .await
    }

    /// Resolve a single interface on a device by exact name, then case-insensitive.
    pub async fn device_interface(&self, device_id: u64, name: &str) -> Result<Option<Interface>> {
        let exact: Page<Interface> = self
            .list(
                Endpoint::Interfaces,
                vec![
                    ("device_id", device_id.to_string()),
                    ("name", name.to_string()),
                ],
            )
            .await?;
        if let Some(i) = exact.results.into_iter().next() {
            return Ok(Some(i));
        }
        let ci: Page<Interface> = self
            .list(
                Endpoint::Interfaces,
                vec![
                    ("device_id", device_id.to_string()),
                    ("name__ic", name.to_string()),
                ],
            )
            .await?;
        Ok(ci.results.into_iter().next())
    }

    /// Trace the cable path from an interface (`…/interfaces/{id}/trace/`).
    /// Returns the raw hop array — each hop is `[near terminations, cable, far
    /// terminations]` — kept as JSON for permissive rendering.
    pub async fn interface_trace(&self, interface_id: u64) -> Result<Vec<serde_json::Value>> {
        self.get(&format!("/api/dcim/interfaces/{interface_id}/trace/"), &[])
            .await
    }

    /// IP addresses assigned to a single interface (up to `max`).
    pub async fn interface_ips(&self, interface_id: u64, max: usize) -> Result<Vec<IpAddress>> {
        self.list_all(
            Endpoint::IpAddresses,
            vec![("interface_id", interface_id.to_string())],
            max,
        )
        .await
    }

    /// IP addresses matching `address` (NetBox host-aware `address` filter).
    pub async fn ip_candidates(&self, address: &str) -> Result<Vec<IpAddress>> {
        let page: Page<IpAddress> = self
            .list(
                Endpoint::IpAddresses,
                vec![("address", address.to_string())],
            )
            .await?;
        Ok(page.results)
    }

    /// All MAC addresses matching `mac` (NetBox 4.2+). MACs aren't enforced
    /// globally unique — the same MAC can appear on several interfaces — so this
    /// returns the full candidate set; the resolver surfaces >1 as ambiguous
    /// (exit 5) rather than silently picking one. `mac` should be normalized to
    /// NetBox's canonical form first (see `normalize_mac`).
    pub async fn mac_candidates(&self, mac: &str) -> Result<Vec<MacAddress>> {
        let page: Page<MacAddress> = self
            .list(
                Endpoint::MacAddresses,
                vec![("mac_address", mac.to_string())],
            )
            .await?;
        Ok(page.results)
    }

    /// Prefixes that contain `address` (NetBox `contains` filter), scoped to a
    /// VRF so parent-prefix enrichment doesn't cross VRFs with overlapping space.
    /// `vrf_id = Some(id)` restricts to that VRF; `None` restricts to the global
    /// table (`vrf_id=null`) — matching an IP that has no VRF.
    pub async fn prefixes_containing(
        &self,
        address: &str,
        vrf_id: Option<u64>,
    ) -> Result<Vec<Prefix>> {
        let vrf = vrf_id.map_or_else(|| "null".to_string(), |id| id.to_string());
        let page: Page<Prefix> = self
            .list(
                Endpoint::Prefixes,
                vec![("contains", address.to_string()), ("vrf_id", vrf)],
            )
            .await?;
        Ok(page.results)
    }

    /// All prefixes that exactly match a CIDR (one per VRF, typically).
    pub async fn prefix_candidates(&self, cidr: &str) -> Result<Vec<Prefix>> {
        let page: Page<Prefix> = self
            .list(Endpoint::Prefixes, vec![("prefix", cidr.to_string())])
            .await?;
        Ok(page.results)
    }

    /// Resolve a prefix by its exact CIDR. Ambiguous (exit 5) when the CIDR exists
    /// in more than one VRF — use [`prefix_candidates`](Self::prefix_candidates)
    /// with a VRF filter to scope.
    pub async fn prefix_by_cidr(&self, cidr: &str) -> Result<Option<Prefix>> {
        let candidates = self.prefix_candidates(cidr).await?;
        ambiguous_or_first("prefix", cidr, candidates, prefix_scope_label)
    }

    /// Available IP addresses within a prefix (`…/available-ips/`), up to `limit`.
    /// This endpoint returns a bare JSON array, not a paginated page.
    pub async fn prefix_available_ips(
        &self,
        prefix_id: u64,
        limit: usize,
    ) -> Result<Vec<AvailableIp>> {
        self.get(
            &format!("/api/ipam/prefixes/{prefix_id}/available-ips/"),
            &[("limit", limit.to_string())],
        )
        .await
    }

    /// Available (free) child prefixes within a prefix (`…/available-prefixes/`).
    /// Passes an explicit `limit` ([`AVAILABLE_PREFIXES_LIMIT`]) so the candidate
    /// set isn't truncated at NetBox's 50-block default.
    pub async fn prefix_available_prefixes(&self, prefix_id: u64) -> Result<Vec<AvailablePrefix>> {
        self.get(
            &format!("/api/ipam/prefixes/{prefix_id}/available-prefixes/"),
            &[("limit", AVAILABLE_PREFIXES_LIMIT.to_string())],
        )
        .await
    }

    /// Prefixes nested within `cidr` (up to `max`), scoped to a VRF so children
    /// don't cross VRFs that share the CIDR. `vrf_id = Some(id)` restricts to that
    /// VRF; `None` restricts to the global table (`vrf_id=null`).
    pub async fn prefix_children(
        &self,
        cidr: &str,
        vrf_id: Option<u64>,
        max: usize,
    ) -> Result<Vec<Prefix>> {
        let vrf = vrf_id.map_or_else(|| "null".to_string(), |id| id.to_string());
        self.list_all(
            Endpoint::Prefixes,
            vec![("within", cidr.to_string()), ("vrf_id", vrf)],
            max,
        )
        .await
    }

    /// IP addresses within `cidr` (up to `max`), scoped to a VRF so members don't
    /// cross VRFs that share the CIDR. `vrf_id = Some(id)` restricts to that VRF;
    /// `None` restricts to the global table (`vrf_id=null`).
    pub async fn prefix_ips(
        &self,
        cidr: &str,
        vrf_id: Option<u64>,
        max: usize,
    ) -> Result<Vec<IpAddress>> {
        let vrf = vrf_id.map_or_else(|| "null".to_string(), |id| id.to_string());
        self.list_all(
            Endpoint::IpAddresses,
            vec![("parent", cidr.to_string()), ("vrf_id", vrf)],
            max,
        )
        .await
    }

    /// All VLANs with a given VID (one per site/group, typically).
    pub async fn vlan_candidates_by_vid(&self, vid: u16) -> Result<Vec<Vlan>> {
        let page: Page<Vlan> = self
            .list(Endpoint::Vlans, vec![("vid", vid.to_string())])
            .await?;
        Ok(page.results)
    }

    /// Resolve a VLAN by VID (if numeric) or by name (exact, then contains).
    /// A VID present at several sites/groups is ambiguous (exit 5).
    pub async fn vlan_by_ref(&self, value: &str) -> Result<Option<Vlan>> {
        if let Ok(vid) = value.parse::<u16>() {
            let candidates = self.vlan_candidates_by_vid(vid).await?;
            return ambiguous_or_first("VLAN", value, candidates, vlan_scope_label);
        }
        let exact: Page<Vlan> = self
            .list(Endpoint::Vlans, vec![("name__ie", value.to_string())])
            .await?;
        if let Some(v) = exact.results.into_iter().next() {
            return Ok(Some(v));
        }
        let contains: Page<Vlan> = self
            .list(Endpoint::Vlans, vec![("name__ic", value.to_string())])
            .await?;
        ambiguous_or_first("VLAN", value, contains.results, |v| {
            format!("{} {}", v.vid, v.name)
        })
    }

    /// Fetch a VLAN group by numeric id (`/api/ipam/vlan-groups/<id>/`). A VLAN
    /// group, unlike a VLAN, is polymorphically scoped; the VLAN serializer's
    /// nested `group` brief omits that scope, so this follow-up fetch reads it.
    /// 404 → `Ok(None)` (a stale group reference is "not found", not an error).
    pub async fn vlan_group_by_id(&self, id: u64) -> Result<Option<VlanGroup>> {
        self.get_optional(&format!("/api/ipam/vlan-groups/{id}/"), &[])
            .await
    }

    /// Prefixes that reference a VLAN (up to `max`).
    pub async fn vlan_prefixes(&self, vlan_id: u64, max: usize) -> Result<Vec<Prefix>> {
        self.list_all(
            Endpoint::Prefixes,
            vec![("vlan_id", vlan_id.to_string())],
            max,
        )
        .await
    }

    /// Resolve a site by numeric id, then slug, then exact name, then
    /// name-contains. The id fast-path hits the detail endpoint directly; a hit
    /// returns immediately (so `--site 5` resolves), but a 404 FALLS THROUGH to
    /// the slug/name lookups — a site whose slug or name is itself numeric (e.g.
    /// `"5"`) still resolves. Mirrors [`tenant_by_ref`](Self::tenant_by_ref).
    pub async fn site_by_ref(&self, value: &str) -> Result<Option<Site>> {
        if let Ok(id) = value.parse::<u64>()
            && let Some(s) = self
                .get_optional::<Site>(&format!("/api/dcim/sites/{id}/"), &[])
                .await?
        {
            return Ok(Some(s));
        }
        let by_slug: Page<Site> = self
            .list(Endpoint::Sites, vec![("slug", value.to_string())])
            .await?;
        if let Some(s) = by_slug.results.into_iter().next() {
            return Ok(Some(s));
        }
        let exact: Page<Site> = self
            .list(Endpoint::Sites, vec![("name__ie", value.to_string())])
            .await?;
        if let Some(s) = exact.results.into_iter().next() {
            return Ok(Some(s));
        }
        let contains: Page<Site> = self
            .list(Endpoint::Sites, vec![("name__ic", value.to_string())])
            .await?;
        ambiguous_or_first("site", value, contains.results, |s| s.name.clone())
    }

    /// Resolve a region by numeric id, then slug, then exact name, then
    /// name-contains. Mirrors [`site_by_ref`](Self::site_by_ref); used to
    /// translate `--region` into a numeric id for prefix `scope_type=dcim.region`
    /// filtering. The id fast-path hits the detail endpoint; a hit returns
    /// immediately (so `--region 5` resolves), but a 404 FALLS THROUGH to the
    /// slug/name lookups (a numeric slug/name still resolves).
    pub async fn region_by_ref(&self, value: &str) -> Result<Option<Region>> {
        if let Ok(id) = value.parse::<u64>()
            && let Some(r) = self
                .get_optional::<Region>(&format!("/api/dcim/regions/{id}/"), &[])
                .await?
        {
            return Ok(Some(r));
        }
        let by_slug: Page<Region> = self
            .list(Endpoint::Regions, vec![("slug", value.to_string())])
            .await?;
        if let Some(r) = by_slug.results.into_iter().next() {
            return Ok(Some(r));
        }
        let exact: Page<Region> = self
            .list(Endpoint::Regions, vec![("name__ie", value.to_string())])
            .await?;
        if let Some(r) = exact.results.into_iter().next() {
            return Ok(Some(r));
        }
        let contains: Page<Region> = self
            .list(Endpoint::Regions, vec![("name__ic", value.to_string())])
            .await?;
        ambiguous_or_first("region", value, contains.results, |r| r.name.clone())
    }

    /// Resolve a site group by numeric id, then slug, then exact name, then
    /// name-contains. Mirrors [`site_by_ref`](Self::site_by_ref); used to
    /// translate `--site-group` into a numeric id for prefix
    /// `scope_type=dcim.sitegroup` filtering. The id fast-path hits the detail
    /// endpoint; a hit returns immediately (so `--site-group 5` resolves), but a
    /// 404 FALLS THROUGH to the slug/name lookups (a numeric slug/name still
    /// resolves).
    pub async fn site_group_by_ref(&self, value: &str) -> Result<Option<SiteGroup>> {
        if let Ok(id) = value.parse::<u64>()
            && let Some(g) = self
                .get_optional::<SiteGroup>(&format!("/api/dcim/site-groups/{id}/"), &[])
                .await?
        {
            return Ok(Some(g));
        }
        let by_slug: Page<SiteGroup> = self
            .list(Endpoint::SiteGroups, vec![("slug", value.to_string())])
            .await?;
        if let Some(g) = by_slug.results.into_iter().next() {
            return Ok(Some(g));
        }
        let exact: Page<SiteGroup> = self
            .list(Endpoint::SiteGroups, vec![("name__ie", value.to_string())])
            .await?;
        if let Some(g) = exact.results.into_iter().next() {
            return Ok(Some(g));
        }
        let contains: Page<SiteGroup> = self
            .list(Endpoint::SiteGroups, vec![("name__ic", value.to_string())])
            .await?;
        ambiguous_or_first("site group", value, contains.results, |g| g.name.clone())
    }

    /// Resolve a location by numeric id, then slug, then exact name, then
    /// name-contains. Mirrors [`site_by_ref`](Self::site_by_ref); used to
    /// translate `--location` into a numeric id for prefix
    /// `scope_type=dcim.location` filtering. The id fast-path hits the detail
    /// endpoint; a hit returns immediately (so `--location 5` resolves), but a 404
    /// FALLS THROUGH to the slug/name lookups (a numeric slug/name still resolves).
    pub async fn location_by_ref(&self, value: &str) -> Result<Option<Location>> {
        if let Ok(id) = value.parse::<u64>()
            && let Some(l) = self
                .get_optional::<Location>(&format!("/api/dcim/locations/{id}/"), &[])
                .await?
        {
            return Ok(Some(l));
        }
        let by_slug: Page<Location> = self
            .list(Endpoint::Locations, vec![("slug", value.to_string())])
            .await?;
        if let Some(l) = by_slug.results.into_iter().next() {
            return Ok(Some(l));
        }
        let exact: Page<Location> = self
            .list(Endpoint::Locations, vec![("name__ie", value.to_string())])
            .await?;
        if let Some(l) = exact.results.into_iter().next() {
            return Ok(Some(l));
        }
        let contains: Page<Location> = self
            .list(Endpoint::Locations, vec![("name__ic", value.to_string())])
            .await?;
        ambiguous_or_first("location", value, contains.results, |l| l.name.clone())
    }

    /// Resolve a VRF by numeric id, then exact RD, then exact name, then
    /// name-contains. VRFs have no slug (unlike sites/regions), so the order is
    /// id → `rd` → `name__ie` → `name__ic`. Used to translate `--vrf` into a
    /// numeric id for the `vrf_id=` search/list filter on IPs and prefixes. The id
    /// fast-path hits the detail endpoint; a hit returns immediately, but a 404
    /// FALLS THROUGH to the RD/name lookups (a numeric RD or name still resolves).
    pub async fn vrf_by_ref(&self, value: &str) -> Result<Option<Vrf>> {
        if let Ok(id) = value.parse::<u64>()
            && let Some(v) = self
                .get_optional::<Vrf>(&format!("/api/ipam/vrfs/{id}/"), &[])
                .await?
        {
            return Ok(Some(v));
        }
        let by_rd: Page<Vrf> = self
            .list(Endpoint::Vrfs, vec![("rd", value.to_string())])
            .await?;
        if let Some(v) = by_rd.results.into_iter().next() {
            return Ok(Some(v));
        }
        let exact: Page<Vrf> = self
            .list(Endpoint::Vrfs, vec![("name__ie", value.to_string())])
            .await?;
        if let Some(v) = exact.results.into_iter().next() {
            return Ok(Some(v));
        }
        let contains: Page<Vrf> = self
            .list(Endpoint::Vrfs, vec![("name__ic", value.to_string())])
            .await?;
        ambiguous_or_first("VRF", value, contains.results, |v| v.name.clone())
    }

    /// Resolve a route target by numeric ID, or by its name (the BGP extended
    /// community value, e.g. `65000:100`). Route targets have no slug/RD, so the
    /// name is the only string key: exact (case-insensitive) first, then a
    /// contains-match that surfaces an ambiguous candidate list.
    pub async fn route_target_by_ref(&self, value: &str) -> Result<Option<RouteTarget>> {
        if let Ok(id) = value.parse::<u64>()
            && let Some(rt) = self
                .get_optional::<RouteTarget>(&format!("/api/ipam/route-targets/{id}/"), &[])
                .await?
        {
            return Ok(Some(rt));
        }
        let exact: Page<RouteTarget> = self
            .list(
                Endpoint::RouteTargets,
                vec![("name__ie", value.to_string())],
            )
            .await?;
        if let Some(rt) = exact.results.into_iter().next() {
            return Ok(Some(rt));
        }
        let contains: Page<RouteTarget> = self
            .list(
                Endpoint::RouteTargets,
                vec![("name__ic", value.to_string())],
            )
            .await?;
        ambiguous_or_first("route target", value, contains.results, |rt| {
            rt.name.clone()
        })
    }

    /// Resolve an IP range by numeric ID, or by its start address.
    pub async fn ip_range_by_ref(&self, value: &str) -> Result<Option<IpRange>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(&format!("/api/ipam/ip-ranges/{id}/"), &[])
                .await;
        }
        let page: Page<IpRange> = self
            .list(
                Endpoint::IpRanges,
                vec![("start_address", value.to_string())],
            )
            .await?;
        ambiguous_or_first("IP range", value, page.results, |r| {
            format!("{} – {}", r.start_address, r.end_address)
        })
    }

    /// Resolve an aggregate by numeric ID, or by its exact CIDR.
    pub async fn aggregate_by_ref(&self, value: &str) -> Result<Option<Aggregate>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(&format!("/api/ipam/aggregates/{id}/"), &[])
                .await;
        }
        let page: Page<Aggregate> = self
            .list(Endpoint::Aggregates, vec![("prefix", value.to_string())])
            .await?;
        ambiguous_or_first("aggregate", value, page.results, |a| a.prefix.clone())
    }

    /// Resolve an ASN by its AS number.
    pub async fn asn_by_ref(&self, asn: u32) -> Result<Option<Asn>> {
        let page: Page<Asn> = self
            .list(Endpoint::Asns, vec![("asn", asn.to_string())])
            .await?;
        Ok(page.results.into_iter().next())
    }

    /// All tags (up to `max`), ordered by name.
    pub async fn tags(&self, max: usize) -> Result<Vec<TagInfo>> {
        self.list_all(Endpoint::Tags, vec![("ordering", "name".to_string())], max)
            .await
    }

    /// Resolve a tag reference (numeric id, exact name, or exact slug) to one
    /// tag. Tag names may contain colons (e.g. `prod:us-east`), so the name is
    /// matched exactly with `?name=` — a substring lookup would be ambiguous and
    /// is not what `nbox tagged` means. Returns `None` when no tag matches.
    pub async fn tag_by_ref(&self, value: &str) -> Result<Option<TagInfo>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(&format!("/api/extras/tags/{id}/"), &[])
                .await;
        }
        // Name (exact) first — names are unique, and the common operator spelling.
        let by_name: crate::netbox::pagination::Page<TagInfo> = self
            .list(Endpoint::Tags, vec![("name", value.to_string())])
            .await?;
        if let Some(t) = by_name.results.into_iter().next() {
            return Ok(Some(t));
        }
        // Then slug (exact) — slugs strip the colons, so `prod:us-east` →
        // `produs-east`; resolving by slug covers the normalized spelling.
        let by_slug: crate::netbox::pagination::Page<TagInfo> = self
            .list(Endpoint::Tags, vec![("slug", value.to_string())])
            .await?;
        Ok(by_slug.results.into_iter().next())
    }

    /// Every object carrying a tag (NetBox 4.3+ `/api/extras/tagged-objects/`),
    /// across object kinds, up to `max`. Filtered server-side by `tag_id` — the
    /// endpoint is polymorphic over all content types, so an unfiltered list is
    /// enormous and never fetched. `tag_id` (not `tag`) is the supported filter;
    /// `tag=<name>` 400s on this endpoint.
    pub async fn tagged_objects(&self, tag_id: u64, max: usize) -> Result<Vec<TaggedObject>> {
        self.list_all(
            Endpoint::TaggedObjects,
            vec![("tag_id", tag_id.to_string())],
            max,
        )
        .await
    }

    /// Journal entries for an object (newest first, up to `max`), addressed by
    /// its dotted content type (e.g. `dcim.device`) and numeric ID.
    pub async fn journal_entries(
        &self,
        content_type: &str,
        object_id: u64,
        max: usize,
    ) -> Result<Vec<JournalEntry>> {
        self.list_all(
            Endpoint::JournalEntries,
            vec![
                ("assigned_object_type", content_type.to_string()),
                ("assigned_object_id", object_id.to_string()),
                ("ordering", "-created".to_string()),
            ],
            max,
        )
        .await
    }

    /// Recent object changes (audit-log entries) for one object, newest first,
    /// from `/api/core/object-changes/` (NetBox 4.x). Mirrors
    /// [`journal_entries`](Self::journal_entries) but against the system audit log
    /// rather than operator journal notes. Scoped by dotted content type + numeric
    /// id — `changed_object_id` alone is ambiguous across types.
    pub async fn object_changes(
        &self,
        content_type: &str,
        object_id: u64,
        max: usize,
    ) -> Result<Vec<ObjectChange>> {
        self.list_all(
            Endpoint::CoreObjectChanges,
            vec![
                ("changed_object_type", content_type.to_string()),
                ("changed_object_id", object_id.to_string()),
                ("ordering", "-time".to_string()),
            ],
            max,
        )
        .await
    }

    /// The A/Z terminations of a circuit (`?circuit_id=`). Returns both sides with
    /// their endpoint, cable, and link peers in one call; the caller orders them.
    pub async fn circuit_terminations(
        &self,
        circuit_id: u64,
    ) -> Result<Vec<crate::netbox::models::circuits::CircuitTermination>> {
        self.list_all(
            Endpoint::CircuitTerminations,
            vec![("circuit_id", circuit_id.to_string())],
            8,
        )
        .await
    }

    /// The terminations of a virtual circuit (`?virtual_circuit_id=`). Virtual
    /// circuits are multi-point (no A/Z sides), so this is a flat list; each
    /// termination lands on a device interface. Capped at a generous bound — a
    /// virtual circuit's termination set is small in practice.
    pub async fn virtual_circuit_terminations(
        &self,
        virtual_circuit_id: u64,
    ) -> Result<Vec<VirtualCircuitTermination>> {
        self.list_all(
            Endpoint::VirtualCircuitTerminations,
            vec![("virtual_circuit_id", virtual_circuit_id.to_string())],
            8,
        )
        .await
    }

    /// Resolve a virtual circuit by numeric ID, then exact CID, then a
    /// CID-contains fallback (ambiguous → exit 5).
    pub async fn virtual_circuit_by_ref(&self, value: &str) -> Result<Option<VirtualCircuit>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(&format!("/api/circuits/virtual-circuits/{id}/"), &[])
                .await;
        }
        let exact: Page<VirtualCircuit> = self
            .list(Endpoint::VirtualCircuits, vec![("cid", value.to_string())])
            .await?;
        if let Some(vc) = exact.results.into_iter().next() {
            return Ok(Some(vc));
        }
        let contains: Page<VirtualCircuit> = self
            .list(
                Endpoint::VirtualCircuits,
                vec![("cid__ic", value.to_string())],
            )
            .await?;
        ambiguous_or_first("virtual circuit", value, contains.results, |vc| {
            vc.cid.clone()
        })
    }

    /// Resolve a circuit by numeric ID, then exact CID, then a CID-contains
    /// fallback (ambiguous → exit 5).
    pub async fn circuit_by_ref(&self, value: &str) -> Result<Option<Circuit>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(&format!("/api/circuits/circuits/{id}/"), &[])
                .await;
        }
        let exact: Page<Circuit> = self
            .list(Endpoint::Circuits, vec![("cid", value.to_string())])
            .await?;
        if let Some(c) = exact.results.into_iter().next() {
            return Ok(Some(c));
        }
        let contains: Page<Circuit> = self
            .list(Endpoint::Circuits, vec![("cid__ic", value.to_string())])
            .await?;
        ambiguous_or_first("circuit", value, contains.results, |c| c.cid.clone())
    }

    /// Resolve a rack by numeric ID, then exact name, then name-contains.
    pub async fn rack_by_ref(&self, value: &str) -> Result<Option<Rack>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(&format!("/api/dcim/racks/{id}/"), &[])
                .await;
        }
        let exact: Page<Rack> = self
            .list(Endpoint::Racks, vec![("name__ie", value.to_string())])
            .await?;
        if let Some(r) = exact.results.into_iter().next() {
            return Ok(Some(r));
        }
        let contains: Page<Rack> = self
            .list(Endpoint::Racks, vec![("name__ic", value.to_string())])
            .await?;
        ambiguous_or_first("rack", value, contains.results, |r| r.name.clone())
    }

    /// Resolve a rack group by numeric ID, then slug, then exact name, then
    /// name-contains (ambiguous → exit 5). Rack groups carry a slug.
    pub async fn rack_group_by_ref(&self, value: &str) -> Result<Option<RackGroup>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(&format!("/api/dcim/rack-groups/{id}/"), &[])
                .await;
        }
        let by_slug: Page<RackGroup> = self
            .list(Endpoint::RackGroups, vec![("slug", value.to_string())])
            .await?;
        if let Some(rg) = by_slug.results.into_iter().next() {
            return Ok(Some(rg));
        }
        let exact: Page<RackGroup> = self
            .list(Endpoint::RackGroups, vec![("name__ie", value.to_string())])
            .await?;
        if let Some(rg) = exact.results.into_iter().next() {
            return Ok(Some(rg));
        }
        let contains: Page<RackGroup> = self
            .list(Endpoint::RackGroups, vec![("name__ic", value.to_string())])
            .await?;
        ambiguous_or_first("rack group", value, contains.results, |rg| rg.name.clone())
    }

    /// Resolve a tenant by numeric ID, then slug, then exact name, then
    /// name-contains. Mirrors [`site_by_ref`](Self::site_by_ref) (tenants, like
    /// sites, carry a slug), with an id fast-path for numeric refs.
    pub async fn tenant_by_ref(&self, value: &str) -> Result<Option<Tenant>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(&format!("/api/tenancy/tenants/{id}/"), &[])
                .await;
        }
        let by_slug: Page<Tenant> = self
            .list(Endpoint::Tenants, vec![("slug", value.to_string())])
            .await?;
        if let Some(t) = by_slug.results.into_iter().next() {
            return Ok(Some(t));
        }
        let exact: Page<Tenant> = self
            .list(Endpoint::Tenants, vec![("name__ie", value.to_string())])
            .await?;
        if let Some(t) = exact.results.into_iter().next() {
            return Ok(Some(t));
        }
        let contains: Page<Tenant> = self
            .list(Endpoint::Tenants, vec![("name__ic", value.to_string())])
            .await?;
        ambiguous_or_first("tenant", value, contains.results, |t| t.name.clone())
    }

    /// Resolve a contact by numeric ID, then exact name, then name-contains.
    /// Contacts have no slug, so the order is id → `name__ie` → `name__ic`
    /// (ambiguous → exit 5).
    pub async fn contact_by_ref(&self, value: &str) -> Result<Option<Contact>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(&format!("/api/tenancy/contacts/{id}/"), &[])
                .await;
        }
        let exact: Page<Contact> = self
            .list(Endpoint::Contacts, vec![("name__ie", value.to_string())])
            .await?;
        if let Some(c) = exact.results.into_iter().next() {
            return Ok(Some(c));
        }
        let contains: Page<Contact> = self
            .list(Endpoint::Contacts, vec![("name__ic", value.to_string())])
            .await?;
        ambiguous_or_first("contact", value, contains.results, |c| c.name.clone())
    }

    /// Resolve a provider by numeric ID, then slug, then exact name, then
    /// name-contains. Providers carry a slug, so the order mirrors
    /// [`tenant_by_ref`](Self::tenant_by_ref): id → slug → `name__ie` →
    /// `name__ic` (ambiguous → exit 5).
    pub async fn provider_by_ref(&self, value: &str) -> Result<Option<Provider>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(&format!("/api/circuits/providers/{id}/"), &[])
                .await;
        }
        let by_slug: Page<Provider> = self
            .list(Endpoint::Providers, vec![("slug", value.to_string())])
            .await?;
        if let Some(p) = by_slug.results.into_iter().next() {
            return Ok(Some(p));
        }
        let exact: Page<Provider> = self
            .list(Endpoint::Providers, vec![("name__ie", value.to_string())])
            .await?;
        if let Some(p) = exact.results.into_iter().next() {
            return Ok(Some(p));
        }
        let contains: Page<Provider> = self
            .list(Endpoint::Providers, vec![("name__ic", value.to_string())])
            .await?;
        ambiguous_or_first("provider", value, contains.results, |p| p.name.clone())
    }

    /// Resolve a virtual machine by numeric ID, then exact name, then
    /// name-contains. VMs have no slug, so the order mirrors
    /// [`device_by_ref`](Self::device_by_ref): id → `name__ie` → `name__ic`
    /// (ambiguous → exit 5). The VM serializer carries a (potentially large)
    /// `config_context`, excluded by default — so the id fast-path opts out
    /// explicitly, matching `device_by_ref`.
    pub async fn vm_by_ref(&self, value: &str) -> Result<Option<VirtualMachine>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(
                    &format!("/api/virtualization/virtual-machines/{id}/"),
                    &[("exclude", "config_context".to_string())],
                )
                .await;
        }
        let exact: Page<VirtualMachine> = self
            .list(
                Endpoint::VirtualMachines,
                vec![("name__ie", value.to_string())],
            )
            .await?;
        if let Some(vm) = exact.results.into_iter().next() {
            return Ok(Some(vm));
        }
        let contains: Page<VirtualMachine> = self
            .list(
                Endpoint::VirtualMachines,
                vec![("name__ic", value.to_string())],
            )
            .await?;
        ambiguous_or_first("virtual machine", value, contains.results, |v| {
            v.name.clone()
        })
    }

    /// Resolve a virtual machine type by numeric ID, then slug, then exact name,
    /// then name-contains (ambiguous → exit 5). VM types carry a slug.
    pub async fn vm_type_by_ref(&self, value: &str) -> Result<Option<VirtualMachineType>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(
                    &format!("/api/virtualization/virtual-machine-types/{id}/"),
                    &[],
                )
                .await;
        }
        let by_slug: Page<VirtualMachineType> = self
            .list(
                Endpoint::VirtualMachineTypes,
                vec![("slug", value.to_string())],
            )
            .await?;
        if let Some(t) = by_slug.results.into_iter().next() {
            return Ok(Some(t));
        }
        let exact: Page<VirtualMachineType> = self
            .list(
                Endpoint::VirtualMachineTypes,
                vec![("name__ie", value.to_string())],
            )
            .await?;
        if let Some(t) = exact.results.into_iter().next() {
            return Ok(Some(t));
        }
        let contains: Page<VirtualMachineType> = self
            .list(
                Endpoint::VirtualMachineTypes,
                vec![("name__ic", value.to_string())],
            )
            .await?;
        ambiguous_or_first("virtual machine type", value, contains.results, |t| {
            t.name.clone()
        })
    }

    /// Resolve a cluster by numeric ID, then exact name, then name-contains.
    /// Clusters have no slug, so the order is id → `name__ie` → `name__ic`
    /// (ambiguous → exit 5).
    pub async fn cluster_by_ref(&self, value: &str) -> Result<Option<Cluster>> {
        if let Ok(id) = value.parse::<u64>() {
            return self
                .get_optional(&format!("/api/virtualization/clusters/{id}/"), &[])
                .await;
        }
        let exact: Page<Cluster> = self
            .list(Endpoint::Clusters, vec![("name__ie", value.to_string())])
            .await?;
        if let Some(c) = exact.results.into_iter().next() {
            return Ok(Some(c));
        }
        let contains: Page<Cluster> = self
            .list(Endpoint::Clusters, vec![("name__ic", value.to_string())])
            .await?;
        ambiguous_or_first("cluster", value, contains.results, |c| c.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProfileConfig;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client_for(server: &MockServer) -> NetBoxClient {
        let profile = ProfileConfig {
            url: server.uri(),
            ..Default::default()
        };
        NetBoxClient::new(&profile, None).unwrap()
    }

    #[tokio::test]
    async fn available_prefixes_requests_full_page_not_default_50() {
        let server = MockServer::start().await;
        // Return 60 free blocks — more than NetBox's 50-block default — to prove
        // candidates past the 50th are carried through when `limit` is sent.
        let blocks: Vec<_> = (0..60)
            .map(|i| json!({"family": 4, "prefix": format!("10.0.{i}.0/24")}))
            .collect();
        Mock::given(method("GET"))
            .and(path("/api/ipam/prefixes/5/available-prefixes/"))
            .and(query_param("limit", "1000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!(blocks)))
            .mount(&server)
            .await;

        let free = client_for(&server)
            .prefix_available_prefixes(5)
            .await
            .expect("available-prefixes");

        // The matcher above already enforces `limit=1000`; assert the full set
        // (beyond 50) is parsed back, including a block past the default cap.
        assert_eq!(free.len(), 60);
        assert_eq!(free[55].prefix, "10.0.55.0/24");
    }

    #[tokio::test]
    async fn region_by_ref_resolves_by_slug() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/regions/"))
            .and(query_param("slug", "us-east"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 3, "name": "US East", "slug": "us-east"}]
            })))
            .mount(&server)
            .await;

        let region = client_for(&server)
            .region_by_ref("us-east")
            .await
            .expect("region lookup")
            .expect("region present");
        assert_eq!(region.id, 3);
    }

    #[tokio::test]
    async fn vrf_by_ref_resolves_by_id() {
        // A numeric ref hits the detail endpoint directly (no list filtering).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/vrfs/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 7, "url": "http://nb/api/ipam/vrfs/7/", "name": "blue", "rd": "65000:7"
            })))
            .mount(&server)
            .await;

        let vrf = client_for(&server)
            .vrf_by_ref("7")
            .await
            .expect("vrf lookup")
            .expect("vrf present");
        assert_eq!(vrf.id, 7);
    }

    #[tokio::test]
    async fn vrf_by_ref_resolves_by_rd() {
        // VRFs have no slug; a non-numeric ref is tried as `rd` first.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/vrfs/"))
            .and(query_param("rd", "65000:7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 7, "url": "http://nb/api/ipam/vrfs/7/", "name": "blue", "rd": "65000:7"}]
            })))
            .mount(&server)
            .await;

        let vrf = client_for(&server)
            .vrf_by_ref("65000:7")
            .await
            .expect("vrf lookup")
            .expect("vrf present");
        assert_eq!(vrf.id, 7);
        assert_eq!(vrf.name, "blue");
    }

    #[tokio::test]
    async fn vrf_by_ref_resolves_by_name() {
        // RD lookup misses, name (exact, `name__ie`) resolves.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/vrfs/"))
            .and(query_param("rd", "blue"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 0, "next": null, "previous": null, "results": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/vrfs/"))
            .and(query_param("name__ie", "blue"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 7, "url": "http://nb/api/ipam/vrfs/7/", "name": "blue"}]
            })))
            .mount(&server)
            .await;

        let vrf = client_for(&server)
            .vrf_by_ref("blue")
            .await
            .expect("vrf lookup")
            .expect("vrf present");
        assert_eq!(vrf.id, 7);
    }

    #[tokio::test]
    async fn site_group_by_ref_falls_back_to_name_then_returns_none() {
        let server = MockServer::start().await;
        // Slug + name__ie + name__ic all empty → unresolved (None, not an error).
        Mock::given(method("GET"))
            .and(path("/api/dcim/site-groups/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 0, "next": null, "previous": null, "results": []
            })))
            .mount(&server)
            .await;

        let resolved = client_for(&server)
            .site_group_by_ref("nope")
            .await
            .expect("site-group lookup");
        assert!(resolved.is_none());
    }

    #[tokio::test]
    async fn region_by_ref_falls_back_slug_then_name_ie_then_name_ic() {
        // The resolver tries slug, then exact name (`name__ie`), then contains
        // (`name__ic`). Slug + exact miss; the contains query resolves the one hit.
        let server = MockServer::start().await;
        for q in [("slug", "us east"), ("name__ie", "us east")] {
            Mock::given(method("GET"))
                .and(path("/api/dcim/regions/"))
                .and(query_param(q.0, q.1))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 0, "next": null, "previous": null, "results": []
                })))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/api/dcim/regions/"))
            .and(query_param("name__ic", "us east"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 9, "name": "US East", "slug": "us-east"}]
            })))
            .mount(&server)
            .await;

        let region = client_for(&server)
            .region_by_ref("us east")
            .await
            .expect("region lookup")
            .expect("region present");
        assert_eq!(region.id, 9);
    }

    #[tokio::test]
    async fn location_by_ref_resolves_by_slug() {
        // `--location` resolution mirrors site/region: slug first.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/locations/"))
            .and(query_param("slug", "row-a"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 4, "name": "Row A", "slug": "row-a"}]
            })))
            .mount(&server)
            .await;

        let loc = client_for(&server)
            .location_by_ref("row-a")
            .await
            .expect("location lookup")
            .expect("location present");
        assert_eq!(loc.id, 4);
    }

    #[tokio::test]
    async fn location_by_ref_ambiguous_name_contains_is_exit_5() {
        // Slug + exact miss; the contains query returns two → an Ambiguous error
        // (exit code 5), listing the candidates.
        let server = MockServer::start().await;
        for key in ["slug", "name__ie"] {
            Mock::given(method("GET"))
                .and(path("/api/dcim/locations/"))
                .and(query_param(key, "row"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 0, "next": null, "previous": null, "results": []
                })))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/api/dcim/locations/"))
            .and(query_param("name__ic", "row"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 2, "next": null, "previous": null,
                "results": [
                    {"id": 1, "name": "Row A", "slug": "row-a"},
                    {"id": 2, "name": "Row B", "slug": "row-b"}
                ]
            })))
            .mount(&server)
            .await;

        let err = client_for(&server)
            .location_by_ref("row")
            .await
            .expect_err("ambiguous location should error");
        assert_eq!(NboxError::exit_code_for(&err), 5);
        // The candidate names are surfaced for the user to disambiguate.
        let msg = format!("{err}");
        assert!(msg.contains("Row A") && msg.contains("Row B"), "got: {msg}");
    }

    #[tokio::test]
    async fn region_by_ref_unknown_returns_none() {
        // Slug + name__ie + name__ic all empty → unresolved (None). In search this
        // becomes a not-found (exit 4); the resolver itself returns None, no error.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/regions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 0, "next": null, "previous": null, "results": []
            })))
            .mount(&server)
            .await;

        let resolved = client_for(&server)
            .region_by_ref("does-not-exist")
            .await
            .expect("region lookup");
        assert!(resolved.is_none());
    }

    #[tokio::test]
    async fn vrf_by_ref_ambiguous_name_contains_is_exit_5() {
        // id parse fails; rd + name__ie miss; name__ic returns two → Ambiguous
        // (exit 5). Confirms the VRF resolver shares the exit-5 contract.
        let server = MockServer::start().await;
        for key in ["rd", "name__ie"] {
            Mock::given(method("GET"))
                .and(path("/api/ipam/vrfs/"))
                .and(query_param(key, "blu"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 0, "next": null, "previous": null, "results": []
                })))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/api/ipam/vrfs/"))
            .and(query_param("name__ic", "blu"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 2, "next": null, "previous": null,
                "results": [
                    {"id": 1, "url": "u", "name": "blue"},
                    {"id": 2, "url": "u", "name": "blue-mgmt"}
                ]
            })))
            .mount(&server)
            .await;

        let err = client_for(&server)
            .vrf_by_ref("blu")
            .await
            .expect_err("ambiguous VRF should error");
        assert_eq!(NboxError::exit_code_for(&err), 5);
    }

    #[test]
    fn friendly_scope_type_maps_known_and_passes_through_unknown() {
        assert_eq!(friendly_scope_type("dcim.site"), "site");
        assert_eq!(friendly_scope_type("dcim.location"), "location");
        assert_eq!(friendly_scope_type("dcim.region"), "region");
        assert_eq!(friendly_scope_type("dcim.sitegroup"), "site-group");
        // Unknown content types are surfaced verbatim, not dropped.
        assert_eq!(friendly_scope_type("dcim.newscope"), "dcim.newscope");
    }

    #[test]
    fn vlan_scope_label_prefers_polymorphic_scope_then_site_then_group() {
        let scoped: Vlan = serde_json::from_value(json!({
            "id": 1, "url": "u", "vid": 10, "name": "a",
            "scope_type": "dcim.region", "scope": {"id": 1, "display": "us-east"}
        }))
        .unwrap();
        assert_eq!(vlan_scope_label(&scoped), "10 a (region: us-east)");

        let sited: Vlan = serde_json::from_value(json!({
            "id": 2, "url": "u", "vid": 11, "name": "b",
            "site": {"id": 1, "display": "iad1"}
        }))
        .unwrap();
        assert_eq!(vlan_scope_label(&sited), "11 b (site: iad1)");

        let grouped: Vlan = serde_json::from_value(json!({
            "id": 3, "url": "u", "vid": 12, "name": "c",
            "group": {"id": 1, "display": "campus"}
        }))
        .unwrap();
        assert_eq!(vlan_scope_label(&grouped), "12 c (group: campus)");

        let bare: Vlan =
            serde_json::from_value(json!({"id": 4, "url": "u", "vid": 13, "name": "d"})).unwrap();
        assert_eq!(vlan_scope_label(&bare), "13 d");
    }

    // --- M4: numeric by-id 404 falls through to slug/name/RD lookup ---

    #[tokio::test]
    async fn site_by_ref_numeric_404_falls_through_to_slug() {
        // A numeric `--site 5` whose id detail 404s must still resolve a site whose
        // SLUG is literally "5" (the old fast-path returned None on 404).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/5/"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/"))
            .and(query_param("slug", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 42, "url": "u", "name": "Site Five", "slug": "5"}]
            })))
            .mount(&server)
            .await;

        let site = client_for(&server)
            .site_by_ref("5")
            .await
            .expect("site lookup")
            .expect("site resolved by slug after 404");
        assert_eq!(site.id, 42);
    }

    #[tokio::test]
    async fn site_by_ref_valid_id_returns_immediately() {
        // A genuine id hit returns straight off the detail endpoint — no slug/name
        // list calls (mounted `.expect(0)` to prove the fast-path short-circuits).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/9/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 9, "url": "u", "name": "iad1", "slug": "iad1"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 0, "next": null, "previous": null, "results": []
            })))
            .expect(0)
            .mount(&server)
            .await;

        let site = client_for(&server)
            .site_by_ref("9")
            .await
            .expect("site lookup")
            .expect("site resolved by id");
        assert_eq!(site.id, 9);
    }

    #[tokio::test]
    async fn region_by_ref_numeric_404_falls_through_to_slug() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/regions/5/"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/regions/"))
            .and(query_param("slug", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 42, "url": "u", "name": "Region Five", "slug": "5"}]
            })))
            .mount(&server)
            .await;

        let region = client_for(&server)
            .region_by_ref("5")
            .await
            .expect("region lookup")
            .expect("region resolved by slug after 404");
        assert_eq!(region.id, 42);
    }

    #[tokio::test]
    async fn site_group_by_ref_numeric_404_falls_through_to_slug() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/site-groups/5/"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/site-groups/"))
            .and(query_param("slug", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 42, "url": "u", "name": "Group Five", "slug": "5"}]
            })))
            .mount(&server)
            .await;

        let group = client_for(&server)
            .site_group_by_ref("5")
            .await
            .expect("site-group lookup")
            .expect("site-group resolved by slug after 404");
        assert_eq!(group.id, 42);
    }

    #[tokio::test]
    async fn location_by_ref_numeric_404_falls_through_to_slug() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/locations/5/"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/locations/"))
            .and(query_param("slug", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 42, "url": "u", "name": "Location Five", "slug": "5"}]
            })))
            .mount(&server)
            .await;

        let loc = client_for(&server)
            .location_by_ref("5")
            .await
            .expect("location lookup")
            .expect("location resolved by slug after 404");
        assert_eq!(loc.id, 42);
    }

    #[tokio::test]
    async fn vrf_by_ref_numeric_404_falls_through_to_rd() {
        // VRFs have no slug; a numeric `--vrf 5` that 404s must still resolve a VRF
        // whose RD is literally "5".
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/vrfs/5/"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/ipam/vrfs/"))
            .and(query_param("rd", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 42, "url": "u", "name": "vrf-five", "rd": "5"}]
            })))
            .mount(&server)
            .await;

        let vrf = client_for(&server)
            .vrf_by_ref("5")
            .await
            .expect("vrf lookup")
            .expect("vrf resolved by rd after 404");
        assert_eq!(vrf.id, 42);
    }

    #[tokio::test]
    async fn site_by_ref_numeric_404_with_no_other_match_is_none() {
        // Numeric id 404s, and slug/name lookups all miss → unresolved (None, not
        // an error). In search this becomes a not-found (exit 4).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/5/"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 0, "next": null, "previous": null, "results": []
            })))
            .mount(&server)
            .await;

        let resolved = client_for(&server)
            .site_by_ref("5")
            .await
            .expect("site lookup");
        assert!(resolved.is_none());
    }
}
