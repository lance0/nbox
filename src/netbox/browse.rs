//! Browse-by-kind: list all objects of one kind for the TUI's Navigation pane.
//!
//! Where `search` fans out across kinds for a `q=` query, browse lists a single
//! kind straight off its endpoint (paginated via [`NetBoxClient::list_all`], capped
//! so a huge instance can't pull unbounded rows) and normalizes the rows into the
//! same [`SearchResult`] the Results pane already renders. Results are sorted by
//! display name (there's no query to rank against).

use anyhow::Result;

use crate::netbox::client::NetBoxClient;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::models::common::BriefObject;
use crate::netbox::models::dcim::{Device, Rack, Site};
use crate::netbox::models::ipam::{IpAddress, Prefix, Vlan};
use crate::netbox::search::{ObjectKind, SearchResult};
use crate::util::format::api_to_web_url;

/// Upper bound on rows pulled for a single browse (keeps a large instance from
/// dragging the whole table into memory; search is the tool for finding a needle).
pub const BROWSE_CAP: usize = 500;

/// List all objects of `kind`, normalized to [`SearchResult`] and sorted by
/// display. Kinds without a browse mapping (e.g. composite/derived ones) return
/// an empty list — the Nav pane only ever offers the kinds handled here.
pub async fn browse(
    client: &NetBoxClient,
    kind: ObjectKind,
    max: usize,
) -> Result<Vec<SearchResult>> {
    let mut out = match kind {
        ObjectKind::Device => {
            let rows: Vec<Device> = client.list_all(Endpoint::Devices, vec![], max).await?;
            rows.into_iter()
                .map(|d| SearchResult {
                    kind: ObjectKind::Device,
                    id: d.id,
                    subtitle: d.site.as_ref().map(BriefObject::label),
                    url: api_to_web_url(&d.url),
                    display: d.name,
                    score: 0,
                })
                .collect()
        }
        ObjectKind::Site => {
            let rows: Vec<Site> = client.list_all(Endpoint::Sites, vec![], max).await?;
            rows.into_iter()
                .map(|s| SearchResult {
                    kind: ObjectKind::Site,
                    id: s.id,
                    subtitle: Some(s.slug),
                    url: api_to_web_url(&s.url),
                    display: s.name,
                    score: 0,
                })
                .collect()
        }
        ObjectKind::Rack => {
            let rows: Vec<Rack> = client.list_all(Endpoint::Racks, vec![], max).await?;
            rows.into_iter()
                .map(|r| SearchResult {
                    kind: ObjectKind::Rack,
                    id: r.id,
                    subtitle: r.site.as_ref().map(BriefObject::label),
                    url: api_to_web_url(&r.url),
                    display: r.name,
                    score: 0,
                })
                .collect()
        }
        ObjectKind::Prefix => {
            let rows: Vec<Prefix> = client.list_all(Endpoint::Prefixes, vec![], max).await?;
            rows.into_iter()
                .map(|p| SearchResult {
                    kind: ObjectKind::Prefix,
                    id: p.id,
                    subtitle: p.status.map(|c| c.value),
                    url: api_to_web_url(&p.url),
                    display: p.prefix,
                    score: 0,
                })
                .collect()
        }
        ObjectKind::IpAddress => {
            let rows: Vec<IpAddress> = client.list_all(Endpoint::IpAddresses, vec![], max).await?;
            rows.into_iter()
                .map(|ip| SearchResult {
                    kind: ObjectKind::IpAddress,
                    id: ip.id,
                    subtitle: ip
                        .dns_name
                        .filter(|s| !s.is_empty())
                        .or_else(|| ip.status.map(|c| c.value)),
                    url: api_to_web_url(&ip.url),
                    display: ip.address,
                    score: 0,
                })
                .collect()
        }
        ObjectKind::Vlan => {
            let rows: Vec<Vlan> = client.list_all(Endpoint::Vlans, vec![], max).await?;
            rows.into_iter()
                .map(|v| SearchResult {
                    kind: ObjectKind::Vlan,
                    id: v.id,
                    subtitle: Some(format!("vlan {}", v.vid)),
                    url: api_to_web_url(&v.url),
                    display: v.name,
                    score: 0,
                })
                .collect()
        }
        // Kinds the Nav pane never offers for browse.
        _ => Vec::new(),
    };
    out.sort_by(|a, b| a.display.cmp(&b.display));
    Ok(out)
}
