//! Endpoint-specific query helpers on [`NetBoxClient`].
//!
//! These resolve user-facing references (names, slugs, IDs) into model objects.

use anyhow::Result;

use crate::netbox::client::NetBoxClient;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::models::dcim::{Device, Interface, Rack, Site};
use crate::netbox::models::ipam::{IpAddress, Prefix, Vlan};
use crate::netbox::pagination::Page;

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
        Ok(contains.results.into_iter().next())
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

    /// Resolve a prefix by its exact CIDR.
    pub async fn prefix_by_cidr(&self, cidr: &str) -> Result<Option<Prefix>> {
        let page: Page<Prefix> = self
            .list(Endpoint::Prefixes, vec![("prefix", cidr.to_string())])
            .await?;
        Ok(page.results.into_iter().next())
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

    /// Resolve a VLAN by VID (if numeric) or by name (exact, then contains).
    pub async fn vlan_by_ref(&self, value: &str) -> Result<Option<Vlan>> {
        if let Ok(vid) = value.parse::<u16>() {
            let page: Page<Vlan> = self
                .list(Endpoint::Vlans, vec![("vid", vid.to_string())])
                .await?;
            return Ok(page.results.into_iter().next());
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
        Ok(contains.results.into_iter().next())
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
        Ok(contains.results.into_iter().next())
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
        Ok(contains.results.into_iter().next())
    }
}
