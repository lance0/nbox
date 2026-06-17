//! On-demand detail loading for the TUI: fetch an object by kind + id (or by a
//! user reference) and render it, reusing the same view models as the CLI.

use anyhow::{Context, Result};

use crate::domain::aggregate_view::AggregateView;
use crate::domain::asn_view::AsnView;
use crate::domain::circuit_view::CircuitView;
use crate::domain::device_detail::DeviceDetail;
use crate::domain::ip_range_view::IpRangeView;
use crate::domain::ip_view::{IpView, most_specific};
use crate::domain::prefix_view::PrefixView;
use crate::domain::site_view::SiteView;
use crate::domain::vlan_view::VlanView;
use crate::netbox::client::NetBoxClient;
use crate::netbox::models::circuits::Circuit;
use crate::netbox::models::dcim::{Device, Site};
use crate::netbox::models::ipam::{Aggregate, Asn, IpAddress, IpRange, Prefix, Vlan};
use crate::netbox::search::ObjectKind;

/// Maximum sub-resources to load for a device detail screen.
const DEVICE_SECTION_CAP: usize = 200;

/// A switchable section on a detail screen (e.g. a device's interfaces).
#[derive(Debug, Clone)]
pub struct DetailTab {
    pub key: char,
    pub label: String,
    pub body: String,
}

/// A rendered detail screen: the object's identity, a title, the summary body,
/// and any switchable tabs (empty for objects without sub-resources).
#[derive(Debug, Clone)]
pub struct DetailView {
    pub kind: ObjectKind,
    pub id: u64,
    pub title: String,
    pub body: String,
    pub tabs: Vec<DetailTab>,
}

impl DetailView {
    fn new(kind: ObjectKind, id: u64, title: String, body: String) -> Self {
        Self {
            kind,
            id,
            title,
            body,
            tabs: Vec::new(),
        }
    }

    fn with_tabs(mut self, tabs: Vec<DetailTab>) -> Self {
        self.tabs = tabs;
        self
    }
}

/// Build a device detail (summary body + i/p/c/v tabs) from its sub-resources.
async fn load_device_detail(
    client: &NetBoxClient,
    device: Device,
) -> Result<(String, String, Vec<DetailTab>)> {
    let id = device.id;
    let name = device.name.clone();
    let (interfaces, ips, services) = tokio::try_join!(
        client.device_interfaces(id, DEVICE_SECTION_CAP),
        client.device_ips(id, DEVICE_SECTION_CAP),
        client.device_services(id, DEVICE_SECTION_CAP),
    )?;
    let detail = DeviceDetail::build(device, interfaces, ips, services);
    let tabs = detail
        .sections()
        .into_iter()
        .map(|(key, label, body)| DetailTab {
            key,
            label: label.to_string(),
            body,
        })
        .collect();
    Ok((format!("device {name}"), detail.summary_plain(), tabs))
}

/// Load and render the detail for a search result (`kind` + `id`).
pub async fn load_detail(client: &NetBoxClient, kind: ObjectKind, id: u64) -> Result<DetailView> {
    let mut tabs = Vec::new();
    let (title, body) = match kind {
        ObjectKind::Device => {
            let d: Device = client
                .get(
                    &format!("/api/dcim/devices/{id}/"),
                    &[("exclude", "config_context".to_string())],
                )
                .await?;
            let (title, body, device_tabs) = load_device_detail(client, d).await?;
            tabs = device_tabs;
            (title, body)
        }
        ObjectKind::Site => {
            let s: Site = client.get(&format!("/api/dcim/sites/{id}/"), &[]).await?;
            let v = SiteView::from_model(s);
            (format!("site {}", v.name), v.to_key_values().render())
        }
        ObjectKind::IpAddress => {
            let ip: IpAddress = client
                .get(&format!("/api/ipam/ip-addresses/{id}/"), &[])
                .await?;
            let host = ip
                .address
                .split('/')
                .next()
                .unwrap_or(&ip.address)
                .to_string();
            let vrf_id = ip.vrf.as_ref().map(|v| v.id);
            let parent = most_specific(client.prefixes_containing(&host, vrf_id).await?);
            let v = IpView::build(ip, parent);
            (format!("ip {}", v.address), v.to_key_values().render())
        }
        ObjectKind::Prefix => {
            let p: Prefix = client
                .get(&format!("/api/ipam/prefixes/{id}/"), &[])
                .await?;
            let cidr = p.prefix.clone();
            let children = client.prefix_children(&cidr, 50).await?;
            let ips = client.prefix_ips(&cidr, 50).await?;
            let v = PrefixView::build(p, children, ips);
            (format!("prefix {}", v.prefix), v.to_plain())
        }
        ObjectKind::Vlan => {
            let vlan: Vlan = client.get(&format!("/api/ipam/vlans/{id}/"), &[]).await?;
            let prefixes = client.vlan_prefixes(vlan.id, 50).await?;
            let v = VlanView::build(vlan, prefixes);
            (format!("vlan {}", v.vid), v.to_plain())
        }
        ObjectKind::Circuit => {
            let c: Circuit = client
                .get(&format!("/api/circuits/circuits/{id}/"), &[])
                .await?;
            let v = CircuitView::from_model(c);
            (format!("circuit {}", v.cid), v.to_key_values().render())
        }
        ObjectKind::Aggregate => {
            let a: Aggregate = client
                .get(&format!("/api/ipam/aggregates/{id}/"), &[])
                .await?;
            let v = AggregateView::from_model(a);
            (
                format!("aggregate {}", v.prefix),
                v.to_key_values().render(),
            )
        }
        ObjectKind::Asn => {
            let a: Asn = client.get(&format!("/api/ipam/asns/{id}/"), &[]).await?;
            let v = AsnView::from_model(a);
            (format!("asn {}", v.asn), v.to_key_values().render())
        }
        ObjectKind::IpRange => {
            let r: IpRange = client
                .get(&format!("/api/ipam/ip-ranges/{id}/"), &[])
                .await?;
            let v = IpRangeView::from_model(r);
            (
                format!("ip-range {}-{}", v.start_address, v.end_address),
                v.to_key_values().render(),
            )
        }
    };
    Ok(DetailView::new(kind, id, title, body).with_tabs(tabs))
}

/// Load and render a detail by user reference (name/slug/cidr/vid/address),
/// used by the command palette.
pub async fn load_detail_by_ref(
    client: &NetBoxClient,
    kind: ObjectKind,
    value: &str,
) -> Result<DetailView> {
    let mut tabs = Vec::new();
    let (id, title, body) = match kind {
        ObjectKind::Device => {
            let d = client
                .device_by_ref(value)
                .await?
                .with_context(|| format!("no device matched \"{value}\""))?;
            let id = d.id;
            let (title, body, device_tabs) = load_device_detail(client, d).await?;
            tabs = device_tabs;
            (id, title, body)
        }
        ObjectKind::Site => {
            let s = client
                .site_by_ref(value)
                .await?
                .with_context(|| format!("no site matched \"{value}\""))?;
            let id = s.id;
            let v = SiteView::from_model(s);
            (id, format!("site {}", v.name), v.to_key_values().render())
        }
        ObjectKind::IpAddress => {
            let ip = client
                .ip_candidates(value)
                .await?
                .into_iter()
                .next()
                .with_context(|| format!("no IP address matched \"{value}\""))?;
            let id = ip.id;
            let host = ip
                .address
                .split('/')
                .next()
                .unwrap_or(&ip.address)
                .to_string();
            let vrf_id = ip.vrf.as_ref().map(|v| v.id);
            let parent = most_specific(client.prefixes_containing(&host, vrf_id).await?);
            let v = IpView::build(ip, parent);
            (id, format!("ip {}", v.address), v.to_key_values().render())
        }
        ObjectKind::Prefix => {
            let p = client
                .prefix_by_cidr(value)
                .await?
                .with_context(|| format!("no prefix matched \"{value}\""))?;
            let id = p.id;
            let cidr = p.prefix.clone();
            let children = client.prefix_children(&cidr, 50).await?;
            let ips = client.prefix_ips(&cidr, 50).await?;
            let v = PrefixView::build(p, children, ips);
            (id, format!("prefix {}", v.prefix), v.to_plain())
        }
        ObjectKind::Vlan => {
            let vlan = client
                .vlan_by_ref(value)
                .await?
                .with_context(|| format!("no VLAN matched \"{value}\""))?;
            let id = vlan.id;
            let prefixes = client.vlan_prefixes(vlan.id, 50).await?;
            let v = VlanView::build(vlan, prefixes);
            (id, format!("vlan {}", v.vid), v.to_plain())
        }
        ObjectKind::Circuit => {
            let c = client
                .circuit_by_ref(value)
                .await?
                .with_context(|| format!("no circuit matched \"{value}\""))?;
            let id = c.id;
            let v = CircuitView::from_model(c);
            (id, format!("circuit {}", v.cid), v.to_key_values().render())
        }
        ObjectKind::Aggregate => {
            let a = client
                .aggregate_by_ref(value)
                .await?
                .with_context(|| format!("no aggregate matched \"{value}\""))?;
            let id = a.id;
            let v = AggregateView::from_model(a);
            (
                id,
                format!("aggregate {}", v.prefix),
                v.to_key_values().render(),
            )
        }
        ObjectKind::Asn => {
            let asn: u32 = value
                .trim()
                .trim_start_matches(['A', 'a', 'S', 's'])
                .parse()
                .with_context(|| format!("invalid AS number \"{value}\""))?;
            let a = client
                .asn_by_ref(asn)
                .await?
                .with_context(|| format!("no ASN matched \"{value}\""))?;
            let id = a.id;
            let v = AsnView::from_model(a);
            (id, format!("asn {}", v.asn), v.to_key_values().render())
        }
        ObjectKind::IpRange => {
            let r = client
                .ip_range_by_ref(value)
                .await?
                .with_context(|| format!("no IP range matched \"{value}\""))?;
            let id = r.id;
            let v = IpRangeView::from_model(r);
            (
                id,
                format!("ip-range {}-{}", v.start_address, v.end_address),
                v.to_key_values().render(),
            )
        }
    };
    Ok(DetailView::new(kind, id, title, body).with_tabs(tabs))
}
