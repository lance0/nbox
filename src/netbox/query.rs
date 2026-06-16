//! Endpoint-specific query helpers on [`NetBoxClient`].
//!
//! These resolve user-facing references (names, slugs, IDs) into model objects.

use anyhow::Result;

use crate::error::NboxError;
use crate::netbox::client::NetBoxClient;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::models::circuits::Circuit;
use crate::netbox::models::dcim::{Device, Interface, Rack, Site};
use crate::netbox::models::ipam::{AvailableIp, AvailablePrefix, IpAddress, Prefix, Vlan};
use crate::netbox::pagination::Page;

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

/// Disambiguation label for a prefix, e.g. `10.0.0.0/24 (vrf: blue)` / `(global)`.
pub(crate) fn prefix_scope_label(p: &Prefix) -> String {
    match &p.vrf {
        Some(v) => format!("{} (vrf: {})", p.prefix, v.label()),
        None => format!("{} (global)", p.prefix),
    }
}

/// Disambiguation label for a VLAN, e.g. `208 users (site: iad1)`.
pub(crate) fn vlan_scope_label(v: &Vlan) -> String {
    let scope = match (&v.site, &v.group) {
        (Some(s), _) => format!(" (site: {})", s.label()),
        (None, Some(g)) => format!(" (group: {})", g.label()),
        (None, None) => String::new(),
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

    /// Prefixes that contain `address` (NetBox `contains` filter).
    pub async fn prefixes_containing(&self, address: &str) -> Result<Vec<Prefix>> {
        let page: Page<Prefix> = self
            .list(Endpoint::Prefixes, vec![("contains", address.to_string())])
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
    pub async fn prefix_available_prefixes(&self, prefix_id: u64) -> Result<Vec<AvailablePrefix>> {
        self.get(
            &format!("/api/ipam/prefixes/{prefix_id}/available-prefixes/"),
            &[],
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
