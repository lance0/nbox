//! NetBox version probe (`/api/status/`) and minimum-version enforcement.
//!
//! nbox targets NetBox 4.2+ (the polymorphic `scope` model). The TUI calls
//! [`NetBoxClient::verify_compatible`] on launch to fail fast against older
//! instances and surface the version in the status line. One-shot CLI commands
//! skip the probe to avoid an extra round-trip per invocation.

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::netbox::client::NetBoxClient;

/// Minimum supported NetBox major version.
pub const MIN_MAJOR: u32 = 4;
/// Minimum supported NetBox minor version.
pub const MIN_MINOR: u32 = 2;

/// The subset of `/api/status/` that nbox cares about. NetBox returns more keys
/// (python version, plugins, …); they are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct Status {
    #[serde(rename = "netbox-version")]
    pub netbox_version: String,
}

impl NetBoxClient {
    /// Fetch `/api/status/`.
    pub async fn status(&self) -> Result<Status> {
        self.get("/api/status/", &[]).await
    }

    /// Probe the instance and fail if it is below the supported floor.
    pub async fn verify_compatible(&self) -> Result<Status> {
        let status = self.status().await?;
        if !meets_minimum(&status.netbox_version, MIN_MAJOR, MIN_MINOR) {
            bail!(
                "NetBox {} is unsupported; nbox requires {MIN_MAJOR}.{MIN_MINOR}+",
                status.netbox_version
            );
        }
        Ok(status)
    }
}

/// Whether `version` (e.g. `"4.5.5"`, `"4.2.0-dev"`) is at least `major.minor`.
pub fn meets_minimum(version: &str, min_major: u32, min_minor: u32) -> bool {
    parse_major_minor(version) >= (min_major, min_minor)
}

/// Extract `(major, minor)` from a version string, tolerating pre-release
/// suffixes like `-dev` or `-beta1`. Missing parts read as 0.
fn parse_major_minor(version: &str) -> (u32, u32) {
    let mut parts = version.split('.').map(parse_leading_number);
    let major = parts.next().flatten().unwrap_or(0);
    let minor = parts.next().flatten().unwrap_or(0);
    (major, minor)
}

fn parse_leading_number(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_floor_comparisons() {
        assert!(meets_minimum("4.5.5", MIN_MAJOR, MIN_MINOR));
        assert!(meets_minimum("4.2.0", MIN_MAJOR, MIN_MINOR));
        assert!(meets_minimum("5.0.0", MIN_MAJOR, MIN_MINOR));
        assert!(!meets_minimum("4.1.9", MIN_MAJOR, MIN_MINOR));
        assert!(!meets_minimum("3.7.8", MIN_MAJOR, MIN_MINOR));
    }

    #[test]
    fn tolerates_prerelease_suffixes() {
        assert!(meets_minimum("4.2.0-dev", MIN_MAJOR, MIN_MINOR));
        assert!(meets_minimum("4.3.0-beta1", MIN_MAJOR, MIN_MINOR));
        assert!(!meets_minimum("4.1.0-rc1", MIN_MAJOR, MIN_MINOR));
    }

    #[test]
    fn missing_or_garbage_parts_read_as_zero() {
        assert_eq!(parse_major_minor("4"), (4, 0));
        assert_eq!(parse_major_minor(""), (0, 0));
        assert_eq!(parse_major_minor("x.y.z"), (0, 0));
    }
}
