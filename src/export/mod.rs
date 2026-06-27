//! Structured read-only exports.
//!
//! The export surface turns NetBox read-engine results into a fixed structured
//! shape an external system consumes — distinct from the human/JSON/CSV view
//! layer, which is presentation-oriented. The first (and currently only) export
//! is Prometheus service-discovery JSON: `[{"targets": ["ip:port"], "labels":
//! {"key": "value"}}]`, the file-based SD format Prometheus scrapes.
//!
//! [`prometheus_sd`] is pure: it takes already-enriched [`ExportIp`] records
//! (gathered by the CLI, which reuses the query layer in `netbox::query`) and a
//! port, and produces [`Vec<TargetGroup>`]. Grouping, label derivation, and the
//! `ip:port` target string live here so they're unit-testable without a mock
//! NetBox.

use std::collections::BTreeMap;

use serde::Serialize;

/// One enriched IP for export: the address (without prefix length) plus the
/// device/site/role/status/tag metadata that becomes Prometheus labels.
///
/// The CLI gathers these from the read engine — IPs in a prefix
/// ([`crate::netbox::query`] `prefix_ips`) or carrying a tag (IP addresses
/// endpoint `?tag=<slug>`), enriched with the assigned device's site/role via
/// a single `id__in` device fetch — and hands them to [`prometheus_sd`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportIp {
    /// The IP address without its prefix length, e.g. `10.0.0.5`.
    pub address: String,
    /// The assigned device's name, when the IP is on a device interface.
    pub device: Option<String>,
    /// The device's site name (NetBox `site.display`), when known.
    pub site: Option<String>,
    /// The device's role (NetBox `role.display`), when known.
    pub role: Option<String>,
    /// The IP or device status value (e.g. `active`), when known.
    pub status: Option<String>,
    /// Tag slugs carried by the IP, in NetBox order.
    pub tags: Vec<String>,
}

/// A Prometheus service-discovery target group: a list of `host:port` targets
/// sharing one label map. Serialized as `{"targets": [...], "labels": {...}}`,
/// the shape Prometheus's file-SD config scrapes.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TargetGroup {
    pub targets: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

/// Default scrape port when `--port` is omitted. `9100` is the conventional
/// `node_exporter` port — the common ops default for a Prometheus host target.
pub const DEFAULT_PORT: u16 = 9100;

/// Strip a `/prefixlen` suffix from a NetBox address (`10.0.0.5/24` →
/// `10.0.0.5`). Passes through unchanged when there is no slash (an address
/// already given without a mask, or an IPv6 with no trailing length).
#[must_use]
pub fn strip_prefix_len(address: &str) -> String {
    address.split('/').next().unwrap_or(address).to_string()
}

/// Build Prometheus service-discovery target groups from enriched IPs.
///
/// Targets are grouped by device: every IP sharing a device lands in one group
/// whose `targets` is the device's `ip:port` list and whose `labels` carry the
/// shared device/site/role/status. Per-IP tags are unioned (de-duplicated,
/// sorted) into a single comma-joined `tags` label so a group with mixed tags
/// still surfaces all of them. IPs without an assigned device form per-site
/// groups (or a single `device=""` group when site is unknown), so unassigned
/// addresses aren't silently dropped.
///
/// Groups are returned in a stable order: assigned-device groups sorted by
/// device name, then unassigned groups sorted by site (with the empty-site
/// group last). Within a group, targets are sorted by address so re-runs are
/// deterministic.
#[must_use]
pub fn prometheus_sd(ips: &[ExportIp], port: u16) -> Vec<TargetGroup> {
    use std::collections::{BTreeSet, HashMap};

    // Group key: (device, site) for assigned IPs; (None, site) for unassigned.
    // A device's site is constant across its IPs in practice, but we keep the
    // site on the key so two IPs of the same device in (theoretically)
    // different sites don't collapse.
    let mut groups: HashMap<(Option<String>, Option<String>), Vec<&ExportIp>> = HashMap::new();
    for ip in ips {
        let key = (ip.device.clone(), ip.site.clone());
        groups.entry(key).or_default().push(ip);
    }

    let mut out: Vec<TargetGroup> = Vec::with_capacity(groups.len());
    for ((device, site), mut members) in groups {
        // Deterministic target order within a group: by address.
        members.sort_by(|a, b| a.address.cmp(&b.address));

        let targets: Vec<String> = members
            .iter()
            .map(|ip| format!("{}:{}", ip.address, port))
            .collect();

        // Shared labels: device/site/role/status come from the (constant across
        // the group) device. Role/status are taken from the first member — they
        // are device-level, so they agree across a device's IPs.
        let role = members.first().and_then(|m| m.role.clone());
        let status = members.first().and_then(|m| m.status.clone());

        // Union the tags across the group, sorted + de-duplicated.
        let mut tagset: BTreeSet<&str> = BTreeSet::new();
        for m in &members {
            for t in &m.tags {
                tagset.insert(t.as_str());
            }
        }

        let mut labels: BTreeMap<String, String> = BTreeMap::new();
        if let Some(dev) = &device {
            labels.insert("device".to_string(), dev.clone());
        } else {
            // An empty string keeps the label present so Prometheus's `relabel`
            // can match unassigned targets uniformly.
            labels.insert("device".to_string(), String::new());
        }
        if let Some(s) = &site {
            labels.insert("site".to_string(), s.clone());
        }
        if let Some(r) = &role {
            labels.insert("role".to_string(), r.clone());
        }
        if let Some(st) = &status {
            labels.insert("status".to_string(), st.clone());
        }
        if !tagset.is_empty() {
            labels.insert(
                "tags".to_string(),
                tagset.into_iter().collect::<Vec<_>>().join(","),
            );
        }

        out.push(TargetGroup { targets, labels });
    }

    // Stable group order: assigned-device groups first (by device name), then
    // unassigned (by site, empty-site last).
    out.sort_by(|a, b| {
        let a_dev = a.labels.get("device").map_or("", String::as_str);
        let b_dev = b.labels.get("device").map_or("", String::as_str);
        let a_unassigned = a_dev.is_empty();
        let b_unassigned = b_dev.is_empty();
        (
            a_unassigned,
            a_dev,
            a.labels.get("site").map_or("", String::as_str),
        )
            .cmp(&(
                b_unassigned,
                b_dev,
                b.labels.get("site").map_or("", String::as_str),
            ))
    });

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(address: &str, device: Option<&str>, site: Option<&str>) -> ExportIp {
        ExportIp {
            address: address.to_string(),
            device: device.map(str::to_string),
            site: site.map(str::to_string),
            role: None,
            status: Some("active".to_string()),
            tags: Vec::new(),
        }
    }

    #[test]
    fn strip_prefix_len_handles_v4_and_bare() {
        assert_eq!(strip_prefix_len("10.0.0.5/24"), "10.0.0.5");
        assert_eq!(strip_prefix_len("10.0.0.5"), "10.0.0.5");
        assert_eq!(strip_prefix_len("2001:db8::1/64"), "2001:db8::1");
        assert_eq!(strip_prefix_len(""), "");
    }

    #[test]
    fn groups_by_device_and_appends_port() {
        let ips = vec![
            ip("10.0.0.5", Some("edge01"), Some("iad1")),
            ip("10.0.0.6", Some("edge01"), Some("iad1")),
            ip("10.0.0.7", Some("edge02"), Some("iad1")),
        ];
        let groups = prometheus_sd(&ips, 9100);
        assert_eq!(groups.len(), 2, "{groups:?}");

        let edge01 = groups
            .iter()
            .find(|g| g.labels.get("device").map(String::as_str) == Some("edge01"))
            .expect("edge01 group");
        assert_eq!(edge01.targets, vec!["10.0.0.5:9100", "10.0.0.6:9100"]);
        assert_eq!(edge01.labels.get("site").map(String::as_str), Some("iad1"));
        assert_eq!(
            edge01.labels.get("status").map(String::as_str),
            Some("active")
        );

        let edge02 = groups
            .iter()
            .find(|g| g.labels.get("device").map(String::as_str) == Some("edge02"))
            .expect("edge02 group");
        assert_eq!(edge02.targets, vec!["10.0.0.7:9100"]);
    }

    #[test]
    fn unions_tags_sorted_and_deduped() {
        let ips = vec![
            ExportIp {
                address: "10.0.0.5".to_string(),
                device: Some("edge01".to_string()),
                site: Some("iad1".to_string()),
                role: None,
                status: Some("active".to_string()),
                tags: vec!["prod".to_string(), "us-east".to_string()],
            },
            ExportIp {
                address: "10.0.0.6".to_string(),
                device: Some("edge01".to_string()),
                site: Some("iad1".to_string()),
                role: None,
                status: Some("active".to_string()),
                tags: vec!["us-east".to_string(), "monitoring".to_string()],
            },
        ];
        let groups = prometheus_sd(&ips, 9100);
        assert_eq!(groups.len(), 1);
        let tags = groups[0].labels.get("tags").expect("tags label");
        assert_eq!(tags, "monitoring,prod,us-east");
    }

    #[test]
    fn unassigned_ips_group_by_site() {
        let ips = vec![
            ip("10.0.0.9", None, Some("iad1")),
            ip("10.0.0.10", None, Some("iad1")),
            ip("10.0.0.11", None, None),
        ];
        let groups = prometheus_sd(&ips, 9100);
        // Two unassigned groups: one with site iad1, one with no site.
        assert_eq!(groups.len(), 2);
        let unassigned_site = groups
            .iter()
            .find(|g| g.labels.get("site").map(String::as_str) == Some("iad1"))
            .expect("iad1 unassigned group");
        assert_eq!(
            unassigned_site.labels.get("device").map(String::as_str),
            Some("")
        );
        assert_eq!(unassigned_site.targets.len(), 2);

        let no_site = groups
            .iter()
            .find(|g| !g.labels.contains_key("site"))
            .expect("no-site group");
        assert_eq!(no_site.targets, vec!["10.0.0.11:9100"]);
    }

    #[test]
    fn empty_input_yields_empty_array() {
        let groups = prometheus_sd(&[], 9100);
        assert!(groups.is_empty());
    }

    #[test]
    fn serializes_to_prometheus_sd_shape() {
        let ips = vec![ip("10.0.0.5", Some("edge01"), Some("iad1"))];
        let groups = prometheus_sd(&ips, 9100);
        let json = serde_json::to_value(&groups).unwrap();
        let arr = json.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        let group = &arr[0];
        assert!(group.get("targets").is_some());
        assert!(group.get("labels").is_some());
        let targets = group["targets"].as_array().expect("targets array");
        assert_eq!(targets[0], "10.0.0.5:9100");
        assert_eq!(group["labels"]["device"], "edge01");
        assert_eq!(group["labels"]["site"], "iad1");
    }

    #[test]
    fn role_and_status_become_labels() {
        let mut ip1 = ip("10.0.0.5", Some("edge01"), Some("iad1"));
        ip1.role = Some("router".to_string());
        let groups = prometheus_sd(&[ip1], 9100);
        assert_eq!(
            groups[0].labels.get("role").map(String::as_str),
            Some("router")
        );
        assert_eq!(
            groups[0].labels.get("status").map(String::as_str),
            Some("active")
        );
    }
}
