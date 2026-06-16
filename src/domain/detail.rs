//! On-demand detail loading for the TUI: fetch an object by kind + id (or by a
//! user reference) and render it, reusing the same view models as the CLI.

use anyhow::{Context, Result};

use crate::domain::device_view::DeviceView;
use crate::domain::ip_view::{IpView, most_specific};
use crate::domain::prefix_view::PrefixView;
use crate::domain::site_view::SiteView;
use crate::domain::vlan_view::VlanView;
use crate::netbox::client::NetBoxClient;
use crate::netbox::models::dcim::{Device, Site};
use crate::netbox::models::ipam::{IpAddress, Prefix, Vlan};
use crate::netbox::search::ObjectKind;

/// A rendered detail screen: the object's identity plus a title and body.
#[derive(Debug, Clone)]
pub struct DetailView {
    pub kind: ObjectKind,
    pub id: u64,
    pub title: String,
    pub body: String,
}

impl DetailView {
    fn new(kind: ObjectKind, id: u64, title: String, body: String) -> Self {
        Self {
            kind,
            id,
            title,
            body,
        }
    }
}

/// Load and render the detail for a search result (`kind` + `id`).
pub async fn load_detail(client: &NetBoxClient, kind: ObjectKind, id: u64) -> Result<DetailView> {
    let (title, body) = match kind {
        ObjectKind::Device => {
            let d: Device = client
                .get(
                    &format!("/api/dcim/devices/{id}/"),
                    &[("exclude", "config_context".to_string())],
                )
                .await?;
            let v = DeviceView::from_model(d);
            (format!("device {}", v.name), v.to_key_values().render())
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
    Ok(DetailView::new(kind, id, title, body))
}

/// Load and render a detail by user reference (name/slug/cidr/vid/address),
/// used by the command palette.
pub async fn load_detail_by_ref(
    client: &NetBoxClient,
    kind: ObjectKind,
    value: &str,
) -> Result<DetailView> {
    let (id, title, body) = match kind {
        ObjectKind::Device => {
            let d = client
                .device_by_ref(value)
                .await?
                .with_context(|| format!("no device matched \"{value}\""))?;
            let id = d.id;
            let v = DeviceView::from_model(d);
            (id, format!("device {}", v.name), v.to_key_values().render())
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
    Ok(DetailView::new(kind, id, title, body))
}
