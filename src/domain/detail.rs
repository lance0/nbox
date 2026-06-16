//! On-demand detail loading for the TUI: fetch an object by kind + id (or by a
//! user reference) and render it, reusing the same view models as the CLI.

use anyhow::{Context, Result};

use crate::domain::device_detail::DeviceDetail;
use crate::domain::ip_view::{IpView, most_specific};
use crate::domain::prefix_view::PrefixView;
use crate::domain::site_view::SiteView;
use crate::domain::vlan_view::VlanView;
use crate::netbox::client::NetBoxClient;
use crate::netbox::models::dcim::{Device, Site};
use crate::netbox::models::ipam::{IpAddress, Prefix, Vlan};
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
    let (interfaces, ips) = tokio::try_join!(
        client.device_interfaces(id, DEVICE_SECTION_CAP),
        client.device_ips(id, DEVICE_SECTION_CAP)
    )?;
    let detail = DeviceDetail::build(device, interfaces, ips);
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
            let parent = most_specific(client.prefixes_containing(&host).await?);
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
            let parent = most_specific(client.prefixes_containing(&host).await?);
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
    };
    Ok(DetailView::new(kind, id, title, body).with_tabs(tabs))
}
