//! Structured read-only exports.
//!
//! The export surface turns NetBox read-engine results into a fixed structured
//! shape an external system consumes — distinct from the human/JSON/CSV view
//! layer, which is presentation-oriented. Three exports live here:
//!
//! - [`prometheus_sd`] — Prometheus file-SD JSON (`[{"targets": ["ip:port"],
//!   "labels": {...}}]`), targets grouped by device.
//! - [`build_address_list`] — a firewall/blocklist address list: the CIDRs of
//!   IPs in a prefix, or the IPs and prefixes carrying a tag, de-duplicated,
//!   sorted, optionally aggregated into the minimal covering set.
//! - [`device_inventory`] — one [`InventoryRecord`] per device (name, status,
//!   role, site, model, serial, asset tag, rack, primary IP, tenant, tags),
//!   rendered as JSON or, via [`inventory_csv`], CSV.
//!
//! Each transform is pure: it takes already-gathered model rows (fetched by the
//! CLI, which reuses the query layer in `netbox::query`) and produces the output
//! shape, so grouping, label derivation, family filtering, and aggregation are
//! unit-testable without a mock NetBox.

use std::collections::BTreeMap;
use std::net::IpAddr;

use ipnet::IpNet;
use serde::Serialize;

use crate::netbox::models::common::BriefObject;
use crate::netbox::models::dcim::Device;

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

/// Format one `host:port` Prometheus target. IPv6 hosts are bracketed
/// (`[2001:db8::1]:9100`) so the address colons aren't confused with the
/// port separator; IPv4 / hostnames are left bare (`10.0.0.5:9100`). The
/// address is the bare host (mask already stripped by [`strip_prefix_len`]).
#[must_use]
fn sd_target(address: &str, port: u16) -> String {
    if address.contains(':') {
        format!("[{address}]:{port}")
    } else {
        format!("{address}:{port}")
    }
}

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
/// whose `targets` is the device's `host:port` list (IPv6 bracketed) and whose
/// `labels` carry the shared device/site/role/status. Per-IP tags are unioned
/// (de-duplicated, sorted) into a single comma-joined `tags` label so a group
/// with mixed tags still surfaces all of them. IPs that carry no assigned device
/// have no derived `site` (site comes from the device), so in practice they all
/// fall into one `device=""` group rather than being silently dropped — the
/// grouping key still includes `site`, so a caller that *does* supply a site for
/// a deviceless IP gets per-site groups.
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
            .map(|ip| sd_target(&ip.address, port))
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

// ============================== address lists ==============================

/// Parse a NetBox prefix CIDR (`10.0.0.0/24`) into an [`IpNet`]. `None` for an
/// unparseable value, so one malformed row is skipped, never fatal.
#[must_use]
pub fn parse_net(cidr: &str) -> Option<IpNet> {
    cidr.parse::<IpNet>().ok()
}

/// Turn a NetBox IP-address value (`10.0.0.5/24`, or a bare `10.0.0.5`) into its
/// single-host network — `10.0.0.5/32` for IPv4, `…/128` for IPv6. The interface
/// mask is dropped on purpose: a firewall address list wants the host, not the
/// subnet the interface sits on. `None` for an unparseable address.
#[must_use]
pub fn ip_host_net(address: &str) -> Option<IpNet> {
    let host = address.split('/').next().unwrap_or(address);
    let ip: IpAddr = host.parse().ok()?;
    let prefix_len = if ip.is_ipv4() { 32 } else { 128 };
    IpNet::new(ip, prefix_len).ok()
}

/// Build a firewall/blocklist address list from source networks: filter by IP
/// family (`Some(4)` / `Some(6)` / `None` = both), de-duplicate, and sort. With
/// `summarize`, the set is first aggregated into the minimal covering networks
/// (e.g. two contiguous /25s collapse to one /24), within each family.
///
/// Returns [`IpNet`]s in canonical order (IPv4 before IPv6, then by network then
/// prefix length) so re-runs are byte-stable; the caller renders them as CIDR
/// strings (`10.0.0.0/24`, `10.0.0.5/32`).
#[must_use]
pub fn build_address_list(nets: &[IpNet], family: Option<u8>, summarize: bool) -> Vec<IpNet> {
    let filtered: Vec<IpNet> = nets
        .iter()
        .copied()
        .filter(|n| match family {
            Some(4) => n.addr().is_ipv4(),
            Some(6) => n.addr().is_ipv6(),
            _ => true,
        })
        .collect();
    let mut out = if summarize {
        IpNet::aggregate(&filtered)
    } else {
        filtered
    };
    out.sort_unstable();
    out.dedup();
    out
}

// ============================ device inventory =============================

/// One device-inventory record: a fixed, downstream-stable projection of a
/// NetBox device, distinct from the presentation-oriented `device` detail view.
/// All fields are always present (absent values serialize as `null`) so the JSON
/// schema and the CSV columns line up one-to-one.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InventoryRecord {
    pub name: String,
    pub status: Option<String>,
    pub role: Option<String>,
    pub site: Option<String>,
    /// The device type, i.e. its model (NetBox `device_type.display`).
    pub model: Option<String>,
    pub platform: Option<String>,
    pub serial: Option<String>,
    pub asset_tag: Option<String>,
    pub rack: Option<String>,
    /// Primary IP as a bare host (mask stripped) — IPv4 preferred, else IPv6.
    pub primary_ip: Option<String>,
    pub tenant: Option<String>,
    pub tags: Vec<String>,
}

/// The inventory column order, shared by the JSON field order and the CSV header
/// so the two outputs stay aligned.
pub const INVENTORY_COLUMNS: &[&str] = &[
    "name",
    "status",
    "role",
    "site",
    "model",
    "platform",
    "serial",
    "asset_tag",
    "rack",
    "primary_ip",
    "tenant",
    "tags",
];

/// `Some(s)` unless `s` is empty — NetBox returns `""` for an unset serial or
/// asset tag, which reads better as absent than as a blank string.
fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.is_empty())
}

/// Project NetBox devices into inventory records, sorted by name for stable
/// output. Pure: the caller has already fetched (and filtered) the devices.
#[must_use]
pub fn device_inventory(devices: &[Device]) -> Vec<InventoryRecord> {
    let mut out: Vec<InventoryRecord> = devices
        .iter()
        .map(|d| InventoryRecord {
            name: d.name.clone(),
            status: d.status.as_ref().map(|c| c.value.clone()),
            role: d.role.as_ref().map(BriefObject::label),
            site: d.site.as_ref().map(BriefObject::label),
            model: d.device_type.as_ref().map(BriefObject::label),
            platform: d.platform.as_ref().map(BriefObject::label),
            serial: non_empty(d.serial.clone()),
            asset_tag: non_empty(d.asset_tag.clone()),
            rack: d.rack.as_ref().map(BriefObject::label),
            primary_ip: d
                .primary_ip4
                .as_ref()
                .or(d.primary_ip6.as_ref())
                .map(|b| strip_prefix_len(&b.label())),
            tenant: d.tenant.as_ref().map(BriefObject::label),
            tags: d.tags.iter().map(|t| t.slug.clone()).collect(),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Render inventory records as CSV (header + one row per device), reusing the
/// shared CSV writer so quoting and empty-cell handling match `-o csv`. Tags are
/// comma-joined inside their cell (quoted by RFC-4180 escaping). Columns follow
/// [`INVENTORY_COLUMNS`].
///
/// # Errors
/// Propagates a serialization error from the shared CSV writer (not expected for
/// the array shape built here).
pub fn inventory_csv(records: &[InventoryRecord]) -> anyhow::Result<String> {
    let rows: Vec<serde_json::Value> = records
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.name,
                "status": r.status,
                "role": r.role,
                "site": r.site,
                "model": r.model,
                "platform": r.platform,
                "serial": r.serial,
                "asset_tag": r.asset_tag,
                "rack": r.rack,
                "primary_ip": r.primary_ip,
                "tenant": r.tenant,
                "tags": r.tags.join(","),
            })
        })
        .collect();
    let cols: Vec<String> = INVENTORY_COLUMNS.iter().map(|s| (*s).to_string()).collect();
    crate::output::csv::to_csv(&serde_json::Value::Array(rows), Some(&cols))
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
    fn ipv6_targets_are_bracketed_ipv4_is_bare() {
        let ips = vec![
            ip("2001:db8::1", Some("edge01"), None),
            ip("10.0.0.5", Some("edge01"), None),
        ];
        let groups = prometheus_sd(&ips, 9100);
        assert_eq!(groups.len(), 1, "{groups:?}");
        let targets = &groups[0].targets;
        assert!(
            targets.contains(&"[2001:db8::1]:9100".to_string()),
            "IPv6 must be bracketed: {targets:?}"
        );
        assert!(
            targets.contains(&"10.0.0.5:9100".to_string()),
            "IPv4 stays bare: {targets:?}"
        );
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

    // ----------------------------- address lists ---------------------------

    fn net(s: &str) -> IpNet {
        s.parse().expect("valid CIDR")
    }

    #[test]
    fn ip_host_net_strips_interface_mask() {
        assert_eq!(ip_host_net("10.0.0.5/24"), Some(net("10.0.0.5/32")));
        assert_eq!(ip_host_net("10.0.0.5"), Some(net("10.0.0.5/32")));
        assert_eq!(ip_host_net("2001:db8::1/64"), Some(net("2001:db8::1/128")));
        assert_eq!(ip_host_net("not-an-ip"), None);
    }

    #[test]
    fn parse_net_parses_cidr_and_rejects_junk() {
        assert_eq!(parse_net("10.0.0.0/24"), Some(net("10.0.0.0/24")));
        assert_eq!(parse_net("garbage"), None);
    }

    #[test]
    fn address_list_dedups_and_sorts() {
        let input = [
            net("10.0.0.7/32"),
            net("10.0.0.5/32"),
            net("10.0.0.5/32"), // duplicate
            net("10.0.0.0/24"),
            net("2001:db8::1/128"),
        ];
        let out = build_address_list(&input, None, false);
        let rendered: Vec<String> = out.iter().map(ToString::to_string).collect();
        // IPv4 before IPv6; network before host within the same prefix; deduped.
        assert_eq!(
            rendered,
            vec![
                "10.0.0.0/24",
                "10.0.0.5/32",
                "10.0.0.7/32",
                "2001:db8::1/128",
            ]
        );
    }

    #[test]
    fn address_list_filters_by_family() {
        let input = [net("10.0.0.5/32"), net("2001:db8::1/128")];
        let v4 = build_address_list(&input, Some(4), false);
        assert_eq!(v4, vec![net("10.0.0.5/32")]);
        let v6 = build_address_list(&input, Some(6), false);
        assert_eq!(v6, vec![net("2001:db8::1/128")]);
    }

    #[test]
    fn address_list_summarize_aggregates_contiguous() {
        let input = [net("10.0.0.0/25"), net("10.0.0.128/25")];
        let out = build_address_list(&input, None, true);
        assert_eq!(out, vec![net("10.0.0.0/24")], "two /25s collapse to a /24");
    }

    // ---------------------------- device inventory -------------------------

    fn dev(value: serde_json::Value) -> Device {
        serde_json::from_value(value).expect("valid device row")
    }

    #[test]
    fn device_inventory_projects_fields() {
        let d = dev(serde_json::json!({
            "id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01",
            "status": {"value": "active", "label": "Active"},
            "role": {"id": 9, "display": "router"},
            "site": {"id": 1, "display": "iad1"},
            "device_type": {"id": 3, "display": "ASR-9001"},
            "platform": {"id": 4, "display": "ios-xr"},
            "serial": "ABC123",
            "asset_tag": "",
            "rack": {"id": 7, "display": "R12"},
            "primary_ip4": {"id": 50, "display": "10.0.0.5/24"},
            "tenant": {"id": 2, "display": "acme"},
            "tags": [{"id": 1, "name": "prod", "slug": "prod"}]
        }));
        let recs = device_inventory(&[d]);
        assert_eq!(recs.len(), 1);
        let r = &recs[0];
        assert_eq!(r.name, "edge01");
        assert_eq!(r.status.as_deref(), Some("active"));
        assert_eq!(r.role.as_deref(), Some("router"));
        assert_eq!(r.site.as_deref(), Some("iad1"));
        assert_eq!(r.model.as_deref(), Some("ASR-9001"));
        assert_eq!(r.platform.as_deref(), Some("ios-xr"));
        assert_eq!(r.serial.as_deref(), Some("ABC123"));
        assert_eq!(r.asset_tag, None, "empty asset tag becomes absent");
        assert_eq!(r.rack.as_deref(), Some("R12"));
        assert_eq!(r.primary_ip.as_deref(), Some("10.0.0.5"), "mask stripped");
        assert_eq!(r.tenant.as_deref(), Some("acme"));
        assert_eq!(r.tags, vec!["prod".to_string()]);
    }

    #[test]
    fn device_inventory_prefers_ipv4_then_falls_back_to_ipv6() {
        let v6only = dev(serde_json::json!({
            "id": 2, "url": "u", "name": "edge02",
            "primary_ip6": {"id": 9, "display": "2001:db8::1/64"}
        }));
        let recs = device_inventory(&[v6only]);
        assert_eq!(recs[0].primary_ip.as_deref(), Some("2001:db8::1"));
    }

    #[test]
    fn device_inventory_sorted_by_name() {
        let a = dev(serde_json::json!({"id": 1, "url": "u", "name": "zebra"}));
        let b = dev(serde_json::json!({"id": 2, "url": "u", "name": "alpha"}));
        let recs = device_inventory(&[a, b]);
        assert_eq!(recs[0].name, "alpha");
        assert_eq!(recs[1].name, "zebra");
    }

    #[test]
    fn inventory_csv_has_header_and_joined_tags() {
        let d = dev(serde_json::json!({
            "id": 1, "url": "u", "name": "edge01",
            "status": {"value": "active", "label": "Active"},
            "tags": [{"id": 1, "name": "prod", "slug": "prod"},
                     {"id": 2, "name": "us-east", "slug": "us-east"}]
        }));
        let recs = device_inventory(&[d]);
        let csv = inventory_csv(&recs).unwrap();
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "name,status,role,site,model,platform,serial,asset_tag,rack,primary_ip,tenant,tags"
        );
        let row = lines.next().unwrap();
        assert!(row.starts_with("edge01,active,"), "row: {row}");
        // Comma-joined tags land in one quoted cell.
        assert!(row.contains("\"prod,us-east\""), "row: {row}");
    }
}
