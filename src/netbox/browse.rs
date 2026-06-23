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
use crate::netbox::models::ipam::{IpAddress, Prefix, RouteTarget, Vlan, Vrf};
use crate::netbox::pagination::Page;
use crate::netbox::search::{ObjectKind, SearchResult};
use crate::util::format::api_to_web_url;

/// Upper bound on rows pulled for a single browse (keeps a large instance from
/// dragging the whole table into memory; search is the tool for finding a needle).
/// Sized to NetBox's per-request `MAX_PAGE_SIZE` ceiling (1000) so a cap-full
/// browse lands in a single round trip — go higher and `list_all` pages a second
/// request for the remainder.
pub const BROWSE_CAP: usize = 1000;

/// The query field a browse `filter` maps to for `kind` — a case-insensitive
/// `contains` (`__ic`) lookup on the kind's name field (`name__ic` for the
/// name-bearing kinds, `cid__ic` for circuits). `None` means the kind has no usable
/// substring filter, so the TUI routes `/` to global search instead.
///
/// Prefix, aggregate, and IP-address are deliberately `None`: their key field is a
/// CIDR/inet column, not a CharField, so NetBox exposes no `__ic` lookup on it — and
/// an unknown filter param is silently ignored (returns the whole table), so a
/// `prefix__ic`/`address__ic` filter would look applied while matching nothing it
/// claims to. Containment filters (`within_include`/`parent`) are the correct future
/// filter for those kinds. (Checked against NetBox 4.2–4.6 ipam filtersets.)
///
/// Maps more kinds than [`browse`] currently lists (VM/cluster/provider/tenant/…):
/// forward-looking, so the field is ready if those become browsable. Today
/// `browse_kind` is always one of the Nav-rail kinds, so those arms are inert.
#[must_use]
pub fn browse_filter_field(kind: ObjectKind) -> Option<&'static str> {
    match kind {
        ObjectKind::Device
        | ObjectKind::Site
        | ObjectKind::Rack
        | ObjectKind::Vlan
        | ObjectKind::Vrf
        | ObjectKind::RouteTarget
        | ObjectKind::Vm
        | ObjectKind::Cluster
        | ObjectKind::Provider
        | ObjectKind::Tenant
        | ObjectKind::Contact => Some("name__ic"),
        ObjectKind::Circuit => Some("cid__ic"),
        // Prefix/Aggregate/IpAddress key on a CIDR/inet field with no NetBox
        // substring lookup (see above) — `None`, so `/` falls back to search.
        ObjectKind::Prefix
        | ObjectKind::Aggregate
        | ObjectKind::IpAddress
        | ObjectKind::Asn
        | ObjectKind::IpRange
        | ObjectKind::Interface => None,
    }
}

/// List all objects of `kind`, normalized to [`SearchResult`] and sorted by
/// display. An optional `filter` narrows the list server-side via the kind's
/// substring field (see [`browse_filter_field`]) — so a needle is found without
/// pulling the whole table. Kinds without a browse mapping (e.g. composite/derived
/// ones) return an empty list — the Nav pane only ever offers the kinds here.
pub async fn browse(
    client: &NetBoxClient,
    kind: ObjectKind,
    max: usize,
    filter: Option<&str>,
) -> Result<Vec<SearchResult>> {
    // The optional name filter, as a query param applied to every kind's list.
    let filter_param: Option<(&'static str, String)> = filter
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|v| browse_filter_field(kind).map(|field| (field, v.to_string())));
    let base = || -> Vec<(&str, String)> { filter_param.clone().into_iter().collect() };
    let mut out = match kind {
        ObjectKind::Device => {
            let rows: Vec<Device> = client.list_all(Endpoint::Devices, base(), max).await?;
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
            // The full site serializer attaches per-site aggregate counts
            // (device/prefix/rack/vlan/circuit), each a subquery over a large table
            // — slow enough to time out the list on a sizable instance (observed:
            // 100 sites > 120s, vs 0.3s with brief). The browse index only needs
            // name + slug, both in NetBox's `brief` representation, so ask for brief
            // to skip the counts. Opening a site still fetches the full object for
            // its detail view, so nothing is lost there. Sites is the only browse
            // kind heavy enough to need this; the rest list fine at full.
            let mut params = base();
            params.push(("brief", "true".to_string()));
            let rows: Vec<Site> = client.list_all(Endpoint::Sites, params, max).await?;
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
            let rows: Vec<Rack> = client.list_all(Endpoint::Racks, base(), max).await?;
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
            let rows: Vec<Prefix> = client.list_all(Endpoint::Prefixes, base(), max).await?;
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
            let rows: Vec<IpAddress> = client.list_all(Endpoint::IpAddresses, base(), max).await?;
            rows.into_iter()
                .map(|ip| SearchResult {
                    kind: ObjectKind::IpAddress,
                    id: ip.id,
                    // Status (always set) over a sparse DNS name: a browse index
                    // reads cleaner with a column that's never empty, and the DNS
                    // name is in the detail view. Header: STATUS (subtitle_header).
                    subtitle: ip.status.map(|c| c.value),
                    url: api_to_web_url(&ip.url),
                    display: ip.address,
                    score: 0,
                })
                .collect()
        }
        ObjectKind::Vlan => {
            let rows: Vec<Vlan> = client.list_all(Endpoint::Vlans, base(), max).await?;
            rows.into_iter()
                .map(|v| SearchResult {
                    kind: ObjectKind::Vlan,
                    id: v.id,
                    // The VID identifies the VLAN (the name is the display); show
                    // the bare number under a VID header (see subtitle_header).
                    subtitle: Some(v.vid.to_string()),
                    url: api_to_web_url(&v.url),
                    display: v.name,
                    score: 0,
                })
                .collect()
        }
        ObjectKind::Vrf => {
            let rows: Vec<Vrf> = client.list_all(Endpoint::Vrfs, base(), max).await?;
            rows.into_iter()
                .map(|v| SearchResult {
                    kind: ObjectKind::Vrf,
                    id: v.id,
                    // The RD identifies a VRF at a glance; fall back to the tenant.
                    subtitle: v
                        .rd
                        .clone()
                        .or_else(|| v.tenant.as_ref().map(BriefObject::label)),
                    url: api_to_web_url(&v.url),
                    display: v.name,
                    score: 0,
                })
                .collect()
        }
        ObjectKind::RouteTarget => {
            let rows: Vec<RouteTarget> =
                client.list_all(Endpoint::RouteTargets, base(), max).await?;
            rows.into_iter()
                .map(|rt| SearchResult {
                    kind: ObjectKind::RouteTarget,
                    id: rt.id,
                    subtitle: rt.tenant.as_ref().map(BriefObject::label),
                    url: api_to_web_url(&rt.url),
                    display: rt.name,
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

/// The total object count for an endpoint (a one-row page read of NetBox's
/// `count` field — cheap; no rows are pulled).
async fn count(client: &NetBoxClient, endpoint: Endpoint) -> Result<u32> {
    let page: Page<serde_json::Value> = client
        .get(endpoint.path(), &[("limit", "1".to_string())])
        .await?;
    Ok(u32::try_from(page.count).unwrap_or(u32::MAX))
}

/// Per-kind totals for the Nav pane labels, fetched concurrently and best-effort:
/// a kind whose count probe fails is simply omitted (its label shows no number).
pub async fn nav_counts(client: &NetBoxClient) -> Vec<(ObjectKind, u32)> {
    let (devices, prefixes, ips, vlans, vrfs, route_targets, sites, racks) = tokio::join!(
        count(client, Endpoint::Devices),
        count(client, Endpoint::Prefixes),
        count(client, Endpoint::IpAddresses),
        count(client, Endpoint::Vlans),
        count(client, Endpoint::Vrfs),
        count(client, Endpoint::RouteTargets),
        count(client, Endpoint::Sites),
        count(client, Endpoint::Racks),
    );
    [
        (ObjectKind::Device, devices),
        (ObjectKind::Prefix, prefixes),
        (ObjectKind::IpAddress, ips),
        (ObjectKind::Vlan, vlans),
        (ObjectKind::Vrf, vrfs),
        (ObjectKind::RouteTarget, route_targets),
        (ObjectKind::Site, sites),
        (ObjectKind::Rack, racks),
    ]
    .into_iter()
    .filter_map(|(kind, res)| res.ok().map(|c| (kind, c)))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProfileConfig;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client_for(server: &MockServer) -> NetBoxClient {
        let profile = ProfileConfig {
            url: server.uri(),
            ..Default::default()
        };
        NetBoxClient::new(&profile, None).unwrap()
    }

    #[tokio::test]
    async fn site_browse_requests_brief_to_skip_count_annotations() {
        // Regression: the site list serializer's per-site aggregate counts are slow
        // enough to time out the list on a large instance, so browse must request
        // `brief=true`. This mock matches ONLY when brief=true is present, so a
        // browse that drops it gets no response and the call fails here.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/"))
            .and(query_param("brief", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{
                    "id": 7, "url": "http://nb/api/dcim/sites/7/", "name": "iad1", "slug": "iad1"
                }]
            })))
            .mount(&server)
            .await;

        let results = browse(&client_for(&server), ObjectKind::Site, BROWSE_CAP, None)
            .await
            .expect("browse sites");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display, "iad1");
        // The subtitle is the slug, which brief includes — so the browse column
        // survives the brief switch (no column loss for sites).
        assert_eq!(results[0].subtitle.as_deref(), Some("iad1"));
    }

    #[test]
    fn filter_field_maps_each_kind() {
        assert_eq!(browse_filter_field(ObjectKind::Device), Some("name__ic"));
        assert_eq!(browse_filter_field(ObjectKind::Rack), Some("name__ic"));
        assert_eq!(browse_filter_field(ObjectKind::Circuit), Some("cid__ic"));
        // CIDR/inet-keyed kinds have no NetBox substring lookup → `None` (so `/`
        // routes to search, not a filter that would silently match the whole table).
        assert_eq!(browse_filter_field(ObjectKind::Prefix), None);
        assert_eq!(browse_filter_field(ObjectKind::Aggregate), None);
        assert_eq!(browse_filter_field(ObjectKind::IpAddress), None);
        assert_eq!(browse_filter_field(ObjectKind::Asn), None);
    }

    #[tokio::test]
    async fn device_browse_pushes_the_name_filter() {
        // A browse with a filter sends `name__ic=<value>`. This mock matches ONLY
        // when that param is present, so an unfiltered request would get no reply.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/dcim/devices/"))
            .and(query_param("name__ic", "bfr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{
                    "id": 3, "url": "http://nb/api/dcim/devices/3/", "name": "bfr-core-01"
                }]
            })))
            .mount(&server)
            .await;

        let results = browse(
            &client_for(&server),
            ObjectKind::Device,
            BROWSE_CAP,
            Some("bfr"),
        )
        .await
        .expect("filtered browse");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display, "bfr-core-01");
    }

    #[tokio::test]
    async fn cap_sized_browse_is_one_round_trip() {
        // Locks the cap + perf invariant together: a browse at the cap is a single
        // request sized to the cap, not `ceil(cap / page_size)` sequential small
        // pages. The `limit` matcher is the regression catcher — if `list_all` ever
        // stops growing the page to `max`, the request falls back to `limit=100`
        // (the default page size) and this mock won't match. Assumes `BROWSE_CAP`
        // stays ≤ NetBox's `MAX_PAGE_SIZE` (1000), so a cap-full browse fits one
        // page — see the `BROWSE_CAP` doc comment.
        let server = MockServer::start().await;
        let n = BROWSE_CAP;
        let rows: Vec<serde_json::Value> = (0..n)
            .map(|i| {
                json!({
                    "id": i,
                    "url": format!("http://nb/api/dcim/sites/{}/", i),
                    "name": format!("site{i}"),
                    "slug": format!("site{i}"),
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/"))
            .and(query_param("limit", n.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": n, "next": null, "previous": null, "results": rows
            })))
            .expect(1)
            .mount(&server)
            .await;

        let results = browse(&client_for(&server), ObjectKind::Site, n, None)
            .await
            .expect("cap-sized browse");
        assert_eq!(results.len(), n);
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "a cap-full browse is a single round trip"
        );
    }
}
