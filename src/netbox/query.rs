//! Endpoint-specific query helpers on [`NetBoxClient`].
//!
//! These resolve user-facing references (names, slugs, IDs) into model objects.

use anyhow::Result;

use crate::netbox::client::NetBoxClient;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::models::dcim::Device;
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
}
