//! On-demand detail loading for the TUI: fetch an object by kind + id and
//! render it (reusing the same view models as the CLI).

use anyhow::Result;

use crate::domain::device_view::DeviceView;
use crate::domain::ip_view::{IpView, most_specific};
use crate::domain::prefix_view::PrefixView;
use crate::domain::rack_view::RackView;
use crate::domain::site_view::SiteView;
use crate::domain::vlan_view::VlanView;
use crate::netbox::client::NetBoxClient;
use crate::netbox::models::dcim::{Device, Rack, Site};
use crate::netbox::models::ipam::{IpAddress, Prefix, Vlan};
use crate::netbox::search::ObjectKind;

/// A rendered detail screen: a title and a body of `key: value` / section text.
#[derive(Debug, Clone)]
pub struct DetailView {
    pub title: String,
    pub body: String,
}

/// Load and render the detail for a search result (`kind` + `id`).
pub async fn load_detail(client: &NetBoxClient, kind: ObjectKind, id: u64) -> Result<DetailView> {
    match kind {
        ObjectKind::Device => {
            let d: Device = client
                .get(
                    &format!("/api/dcim/devices/{id}/"),
                    &[("exclude", "config_context".to_string())],
                )
                .await?;
            let v = DeviceView::from_model(d);
            Ok(DetailView {
                title: format!("device {}", v.name),
                body: v.to_key_values().render(),
            })
        }
        ObjectKind::Site => {
            let s: Site = client.get(&format!("/api/dcim/sites/{id}/"), &[]).await?;
            let v = SiteView::from_model(s);
            Ok(DetailView {
                title: format!("site {}", v.name),
                body: v.to_key_values().render(),
            })
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
            Ok(DetailView {
                title: format!("ip {}", v.address),
                body: v.to_key_values().render(),
            })
        }
        ObjectKind::Prefix => {
            let p: Prefix = client
                .get(&format!("/api/ipam/prefixes/{id}/"), &[])
                .await?;
            let cidr = p.prefix.clone();
            let children = client.prefix_children(&cidr, 50).await?;
            let ips = client.prefix_ips(&cidr, 50).await?;
            let v = PrefixView::build(p, children, ips);
            Ok(DetailView {
                title: format!("prefix {}", v.prefix),
                body: v.to_plain(),
            })
        }
        ObjectKind::Vlan => {
            let vlan: Vlan = client.get(&format!("/api/ipam/vlans/{id}/"), &[]).await?;
            let prefixes = client.vlan_prefixes(vlan.id, 50).await?;
            let v = VlanView::build(vlan, prefixes);
            Ok(DetailView {
                title: format!("vlan {}", v.vid),
                body: v.to_plain(),
            })
        }
    }
}

/// Load and render a rack detail (used by the `rack` command path / future TUI).
pub async fn load_rack(client: &NetBoxClient, id: u64) -> Result<DetailView> {
    let r: Rack = client.get(&format!("/api/dcim/racks/{id}/"), &[]).await?;
    let v = RackView::from_model(r);
    Ok(DetailView {
        title: format!("rack {}", v.name),
        body: v.to_key_values().render(),
    })
}
