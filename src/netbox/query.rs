//! Endpoint-specific query helpers on [`NetBoxClient`].
//!
//! These resolve user-facing references (names, slugs, IDs) into model objects.

use anyhow::Result;

use crate::error::NboxError;
use crate::netbox::client::NetBoxClient;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::models::circuits::Circuit;
use crate::netbox::models::dcim::{Device, Interface, Location, Rack, Region, Site, SiteGroup};
use crate::netbox::models::extras::{JournalEntry, TagInfo};
use crate::netbox::models::ipam::{
    Aggregate, Asn, AvailableIp, AvailablePrefix, IpAddress, IpRange, Prefix, Service, Vlan,
    VlanGroup,
};
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

    /// Prefixes nested within `cidr` (up to `max`).
    pub async fn prefix_children(&self, cidr: &str, max: usize) -> Result<Vec<Prefix>> {
        self.list_all(Endpoint::Prefixes, vec![("within", cidr.to_string())], max)
            .await
    }

    /// IP addresses within `cidr` (up to `max`).
    pub async fn prefix_ips(&self, cidr: &str, max: usize) -> Result<Vec<IpAddress>> {
        self.list_all(
            Endpoint::IpAddresses,
            vec![("parent", cidr.to_string())],
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

    /// Resolve a site by slug, then exact name, then name-contains.
    pub async fn site_by_ref(&self, value: &str) -> Result<Option<Site>> {
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

    /// Resolve a region by slug, then exact name, then name-contains. Mirrors
    /// [`site_by_ref`](Self::site_by_ref); used to translate `--region` into a
    /// numeric id for prefix `scope_type=dcim.region` filtering.
    pub async fn region_by_ref(&self, value: &str) -> Result<Option<Region>> {
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

    /// Resolve a site group by slug, then exact name, then name-contains. Mirrors
    /// [`site_by_ref`](Self::site_by_ref); used to translate `--site-group` into a
    /// numeric id for prefix `scope_type=dcim.sitegroup` filtering.
    pub async fn site_group_by_ref(&self, value: &str) -> Result<Option<SiteGroup>> {
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

    /// Resolve a location by slug, then exact name, then name-contains. Mirrors
    /// [`site_by_ref`](Self::site_by_ref); used to translate `--location` into a
    /// numeric id for prefix `scope_type=dcim.location` filtering.
    pub async fn location_by_ref(&self, value: &str) -> Result<Option<Location>> {
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
}
