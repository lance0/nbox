//! Endpoint-specific query helpers on [`NetBoxClient`].
//!
//! These resolve user-facing references (names, slugs, IDs) into model objects.

use anyhow::Result;

use crate::netbox::client::NetBoxClient;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::models::dcim::Device;
use crate::netbox::models::ipam::{IpAddress, Prefix};
use crate::netbox::pagination::Page;

impl NetBoxClient {
    /// Resolve a device by numeric ID, then exact (case-insensitive) name, then
    /// a name-contains fallback. Returns `None` when nothing matches a name.
    pub async fn device_by_ref(&self, value: &str) -> Result<Option<Device>> {
        if let Ok(id) = value.parse::<u64>() {
            let device: Device = self
                .get(
                    &format!("/api/dcim/devices/{id}/"),
                    &[("exclude", "config_context".to_string())],
                )
                .await?;
            return Ok(Some(device));
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
}
