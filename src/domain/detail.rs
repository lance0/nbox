//! Shared single-object fetch + view-build layer.
//!
//! Each `*_by_ref` function resolves one object by its user reference, fans out
//! to any sub-resources, and composes its domain view — the one path the CLI
//! handlers (`run_*`), the MCP tools (`nbox_get`/`nbox_get_interface`), and the
//! TUI all share, so a lookup behaves identically across the three front-ends.
//! Resolution failures stay typed (`NboxError::NotFound`/`Ambiguous`) so each
//! caller keeps mapping them to exit codes / `invalid_params`; the `not_found`
//! closure lets each front-end supply its own actionable message text.
//!
//! The TUI also uses [`load_detail`]/[`load_detail_by_ref`] below to fetch an
//! object by kind + id (or reference) and render it with switchable tabs.

use anyhow::{Context, Result};

use crate::domain::aggregate_view::AggregateView;
use crate::domain::asn_view::AsnView;
use crate::domain::circuit_view::CircuitView;
use crate::domain::cluster_view::ClusterView;
use crate::domain::contact_view::ContactView;
use crate::domain::device_detail::DeviceDetail;
use crate::domain::interface_view::InterfaceView;
use crate::domain::ip_range_view::IpRangeView;
use crate::domain::ip_view::{IpView, most_specific};
use crate::domain::journal_view::{JournalEntryRow, JournalView};
use crate::domain::prefix_view::PrefixView;
use crate::domain::provider_view::ProviderView;
use crate::domain::rack_view::RackView;
use crate::domain::site_view::SiteView;
use crate::domain::tenant_view::TenantView;
use crate::domain::vlan_view::VlanView;
use crate::domain::vm_view::VmView;
use crate::error::NboxError;
use crate::netbox::client::NetBoxClient;
use crate::netbox::models::circuits::{Circuit, Provider};
use crate::netbox::models::common::BriefObject;
use crate::netbox::models::dcim::{Device, Rack, Site};
use crate::netbox::models::ipam::{Aggregate, Asn, IpAddress, IpRange, Prefix, Vlan, VlanGroup};
use crate::netbox::models::tenancy::{Contact, Tenant};
use crate::netbox::models::virtualization::{Cluster, VirtualMachine};
use crate::netbox::query;
use crate::netbox::search::ObjectKind;

/// Cap on interfaces/IPs/services to pull for a device lookup (CLI, MCP, TUI).
const DEVICE_CAP: usize = 200;
/// Cap on child/IP rows pulled into a prefix or VLAN section (CLI, MCP, TUI).
const SECTION_CAP: usize = 50;
/// How many recent journal entries to fold into a detail view with `--journal`.
pub const JOURNAL_INLINE_MAX: usize = 5;

/// Fetch the most recent journal entries for an object (by dotted content type
/// and numeric ID) as display rows, reusing the same query + mapping as the
/// standalone `nbox journal` command. Returns at most `max` entries; callers
/// pass [`JOURNAL_INLINE_MAX`] for the default inline cap or a user override.
pub async fn journal_rows(
    client: &NetBoxClient,
    content_type: &str,
    object_id: u64,
    max: usize,
) -> Result<Vec<JournalEntryRow>> {
    let entries = client.journal_entries(content_type, object_id, max).await?;
    Ok(JournalView::from_models(entries).entries)
}

/// Drop candidates whose scope object doesn't match a user-supplied reference
/// (e.g. `--site`/`--vrf`). A no-op when `query` is `None`. Shared by the CLI
/// handlers and the MCP tools so both filter candidate sets identically.
///
/// An exact match wins: if any candidate's scope matches `query` exactly (by
/// name/slug/rd/id), only those are kept. A `--vrf <rd>` reference now resolves
/// exactly via the VRF brief's dedicated `rd` field. Only when nothing matches
/// exactly do we fall back to the looser [`BriefObject::matches`] (display
/// substring). Without the exact-wins step, `--site ci-site` would also retain
/// `ci-site2` whose display contains the substring `ci-site`.
pub(crate) fn retain_scope<T>(
    items: &mut Vec<T>,
    query: Option<&str>,
    scope: impl Fn(&T) -> Option<&BriefObject>,
) {
    if let Some(q) = query {
        let has_exact = items
            .iter()
            .any(|it| scope(it).is_some_and(|b| b.matches_exact(q)));
        if has_exact {
            items.retain(|it| scope(it).is_some_and(|b| b.matches_exact(q)));
        } else {
            items.retain(|it| scope(it).is_some_and(|b| b.matches(q)));
        }
    }
}

/// Resolve a candidate set to exactly one object: not found when empty (via the
/// caller's `not_found`, so each front-end keeps its own message), ambiguous
/// (with the candidate list) when more than one. The `Ambiguous`/`NotFound`
/// error types are preserved so callers map them to exit codes / invalid_params.
pub(crate) fn resolve_unique<T>(
    noun: &str,
    value: &str,
    mut candidates: Vec<T>,
    label: impl Fn(&T) -> String,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<T> {
    match candidates.len() {
        0 => Err(not_found(noun, value)),
        1 => Ok(candidates.pop().unwrap()),
        _ => {
            let matches = candidates
                .iter()
                .take(8)
                .map(&label)
                .collect::<Vec<_>>()
                .join(", ");
            Err(NboxError::Ambiguous {
                noun: noun.to_string(),
                value: value.to_string(),
                matches,
            }
            .into())
        }
    }
}

/// Build a [`DeviceDetail`] from an already-resolved device: fan out to its
/// interfaces, IPs, and services (cap [`DEVICE_CAP`]) and compose the view.
/// Shared by the CLI `device` handler and the MCP `nbox_get` device arm.
async fn build_device_detail(client: &NetBoxClient, device: Device) -> Result<DeviceDetail> {
    let id = device.id;
    let (interfaces, ips, services) = tokio::try_join!(
        client.device_interfaces(id, DEVICE_CAP),
        client.device_ips(id, DEVICE_CAP),
        client.device_services(id, DEVICE_CAP),
    )?;
    Ok(DeviceDetail::build(device, interfaces, ips, services))
}

/// `device <ref>`: resolve a device by reference and compose its detail view.
/// Reproduces the exact CLI/MCP fetch path; `not_found` supplies the caller's
/// message (and exit-code/invalid_params mapping is preserved via its type).
pub async fn device_detail_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<DeviceDetail> {
    let device = client
        .device_by_ref(value)
        .await?
        .ok_or_else(|| not_found("device", value))?;
    build_device_detail(client, device).await
}

/// `interface <device> <interface>`: resolve one interface on a device and
/// build its view (assigned IPs + cable-path trace). Shared by CLI/MCP.
pub async fn interface_view_by_ref(
    client: &NetBoxClient,
    device: &str,
    interface: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<InterfaceView> {
    let dev = client
        .device_by_ref(device)
        .await?
        .ok_or_else(|| not_found("device", device))?;
    let iface = client
        .device_interface(dev.id, interface)
        .await?
        .ok_or_else(|| not_found("interface", interface))?;
    let (ips, trace) = tokio::try_join!(
        client.interface_ips(iface.id, DEVICE_CAP),
        client.interface_trace(iface.id),
    )?;
    Ok(InterfaceView::build(iface, ips, trace))
}

/// `ip <address>`: resolve an IP (scoped by `vrf`, ambiguity-checked) and
/// enrich with its most-specific parent prefix. Shared by CLI/MCP.
pub async fn ip_view_by_ref(
    client: &NetBoxClient,
    address: &str,
    vrf: Option<&str>,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<IpView> {
    let mut candidates = client.ip_candidates(address).await?;
    retain_scope(&mut candidates, vrf, |ip| ip.vrf.as_ref());
    let ip = resolve_unique(
        "IP address",
        address,
        candidates,
        query::ip_scope_label,
        not_found,
    )?;

    let host = address.split('/').next().unwrap_or(address);
    let vrf_id = ip.vrf.as_ref().map(|v| v.id);
    let parent = most_specific(client.prefixes_containing(host, vrf_id).await?);
    Ok(IpView::build(ip, parent))
}

/// Resolve a CIDR to a single prefix, scoped by an optional VRF reference.
/// Shared by the prefix/next-ip/next-prefix paths in both the CLI and MCP.
pub async fn resolve_prefix(
    client: &NetBoxClient,
    cidr: &str,
    vrf: Option<&str>,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<Prefix> {
    let mut candidates = client.prefix_candidates(cidr).await?;
    retain_scope(&mut candidates, vrf, |p| p.vrf.as_ref());
    resolve_unique(
        "prefix",
        cidr,
        candidates,
        query::prefix_scope_label,
        not_found,
    )
}

/// `prefix <cidr>`: resolve a prefix (scoped by `vrf`) and build its view with
/// children and member IPs (cap [`SECTION_CAP`]). Shared by CLI/MCP.
pub async fn prefix_view_by_ref(
    client: &NetBoxClient,
    cidr: &str,
    vrf: Option<&str>,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<PrefixView> {
    let prefix = resolve_prefix(client, cidr, vrf, not_found).await?;
    // Scope children/member IPs to the resolved prefix's VRF (or the global table
    // when it has none), so a CIDR shared across VRFs can't pull the wrong VRF's.
    let vrf_id = prefix.vrf.as_ref().map(|v| v.id);
    let children = client.prefix_children(cidr, vrf_id, SECTION_CAP).await?;
    let ips = client.prefix_ips(cidr, vrf_id, SECTION_CAP).await?;
    Ok(PrefixView::build(prefix, children, ips))
}

/// Fetch the VLAN's group (for its scope) only when the VLAN actually has one.
/// A VLAN group is polymorphically scoped but the VLAN's nested `group` brief
/// omits that scope, so this does one follow-up GET of the group by id. No group
/// ⇒ no request (`Ok(None)`), keeping the unscoped path's behavior unchanged.
/// A stale/missing group id is tolerated (404 → `None`), so a dangling reference
/// never fails an otherwise-good VLAN lookup.
async fn vlan_group_scope(client: &NetBoxClient, vlan: &Vlan) -> Result<Option<VlanGroup>> {
    match vlan.group.as_ref() {
        Some(g) => client.vlan_group_by_id(g.id).await,
        None => Ok(None),
    }
}

/// `vlan <vid|name>`: resolve a VLAN (a VID present at several sites/groups is
/// scoped by `site`/`group`, ambiguity-checked) and build its view with the
/// prefixes that reference it (cap [`SECTION_CAP`]). Shared by CLI/MCP.
pub async fn vlan_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    site: Option<&str>,
    group: Option<&str>,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<VlanView> {
    let vlan = if let Ok(vid) = value.parse::<u16>() {
        let mut candidates = client.vlan_candidates_by_vid(vid).await?;
        retain_scope(&mut candidates, site, |v| v.site.as_ref());
        retain_scope(&mut candidates, group, |v| v.group.as_ref());
        resolve_unique(
            "VLAN",
            value,
            candidates,
            query::vlan_scope_label,
            not_found,
        )?
    } else {
        client
            .vlan_by_ref(value)
            .await?
            .ok_or_else(|| not_found("VLAN", value))?
    };
    let prefixes = client.vlan_prefixes(vlan.id, SECTION_CAP).await?;
    let group = vlan_group_scope(client, &vlan).await?;
    Ok(VlanView::build(vlan, prefixes, group))
}

/// `site <name|slug>`: resolve a site and build its view. Shared by CLI/MCP.
pub async fn site_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<SiteView> {
    let site = client
        .site_by_ref(value)
        .await?
        .ok_or_else(|| not_found("site", value))?;
    Ok(SiteView::from_model(site))
}

/// `rack <name|id>`: resolve a rack and build its view. Shared by CLI/MCP.
pub async fn rack_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<RackView> {
    let rack = client
        .rack_by_ref(value)
        .await?
        .ok_or_else(|| not_found("rack", value))?;
    Ok(RackView::from_model(rack))
}

/// `circuit <cid|id>`: resolve a circuit and build its view. Shared by CLI/MCP.
pub async fn circuit_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<CircuitView> {
    let circuit = client
        .circuit_by_ref(value)
        .await?
        .ok_or_else(|| not_found("circuit", value))?;
    Ok(CircuitView::from_model(circuit))
}

/// `aggregate <cidr|id>`: resolve an aggregate and build its view. Shared by CLI/MCP.
pub async fn aggregate_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<AggregateView> {
    let aggregate = client
        .aggregate_by_ref(value)
        .await?
        .ok_or_else(|| not_found("aggregate", value))?;
    Ok(AggregateView::from_model(aggregate))
}

/// `asn <asn>`: resolve an ASN (by parsed AS number) and build its view. The
/// `value` is the original text reference, used only for the not-found message.
/// Shared by CLI/MCP; each caller does its own string→u32 parsing first so the
/// CLI (clap-parsed u32) and MCP (free-text) keep their exact parse semantics.
pub async fn asn_view_by_ref(
    client: &NetBoxClient,
    asn: u32,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<AsnView> {
    let asn = client
        .asn_by_ref(asn)
        .await?
        .ok_or_else(|| not_found("ASN", value))?;
    Ok(AsnView::from_model(asn))
}

/// `ip-range <start|id>`: resolve an IP range and build its view. Shared by CLI/MCP.
pub async fn ip_range_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<IpRangeView> {
    let range = client
        .ip_range_by_ref(value)
        .await?
        .ok_or_else(|| not_found("IP range", value))?;
    Ok(IpRangeView::from_model(range))
}

/// `tenant <slug|id>`: resolve a tenant and build its view. Shared by CLI/MCP.
pub async fn tenant_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<TenantView> {
    let tenant = client
        .tenant_by_ref(value)
        .await?
        .ok_or_else(|| not_found("tenant", value))?;
    Ok(TenantView::from_model(tenant))
}

/// `contact <name|id>`: resolve a contact and build its view. Shared by CLI/MCP.
pub async fn contact_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<ContactView> {
    let contact = client
        .contact_by_ref(value)
        .await?
        .ok_or_else(|| not_found("contact", value))?;
    Ok(ContactView::from_model(contact))
}

/// `provider <slug|id>`: resolve a provider and build its view. Shared by CLI/MCP.
pub async fn provider_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<ProviderView> {
    let provider = client
        .provider_by_ref(value)
        .await?
        .ok_or_else(|| not_found("provider", value))?;
    Ok(ProviderView::from_model(provider))
}

/// `vm <name|id>`: resolve a virtual machine and build its view. Shared by CLI/MCP.
pub async fn vm_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<VmView> {
    let vm = client
        .vm_by_ref(value)
        .await?
        .ok_or_else(|| not_found("virtual machine", value))?;
    Ok(VmView::from_model(vm))
}

/// `cluster <name|id>`: resolve a cluster and build its view. Shared by CLI/MCP.
pub async fn cluster_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<ClusterView> {
    let cluster = client
        .cluster_by_ref(value)
        .await?
        .ok_or_else(|| not_found("cluster", value))?;
    Ok(ClusterView::from_model(cluster))
}

/// A switchable section on a detail screen (e.g. a device's interfaces).
#[derive(Debug, Clone)]
pub struct DetailTab {
    pub key: char,
    pub label: String,
    pub body: String,
}

/// A navigable reference from one detail object to a related one — the data
/// behind the TUI's `R` "related objects" jump list. `kind` + `id` address the
/// target (drives a `LoadDetail`); `relation` names the edge ("site", "vlan", …);
/// `label` is the target's display name. Only relations whose target has a detail
/// view are emitted (e.g. a VRF/rack/role has no detail kind, so it's skipped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectLink {
    pub kind: ObjectKind,
    pub id: u64,
    pub relation: &'static str,
    pub label: String,
}

/// Push a link for an optional related [`BriefObject`] (skipped when absent).
fn push_link(
    links: &mut Vec<ObjectLink>,
    relation: &'static str,
    kind: ObjectKind,
    obj: Option<&BriefObject>,
) {
    if let Some(o) = obj {
        links.push(ObjectLink {
            kind,
            id: o.id,
            relation,
            label: o.label(),
        });
    }
}

fn device_links(d: &Device) -> Vec<ObjectLink> {
    let mut l = Vec::new();
    push_link(&mut l, "site", ObjectKind::Site, d.site.as_ref());
    push_link(&mut l, "rack", ObjectKind::Rack, d.rack.as_ref());
    push_link(&mut l, "tenant", ObjectKind::Tenant, d.tenant.as_ref());
    push_link(
        &mut l,
        "primary IPv4",
        ObjectKind::IpAddress,
        d.primary_ip4.as_ref(),
    );
    push_link(
        &mut l,
        "primary IPv6",
        ObjectKind::IpAddress,
        d.primary_ip6.as_ref(),
    );
    l
}

fn site_links(s: &Site) -> Vec<ObjectLink> {
    let mut l = Vec::new();
    push_link(&mut l, "tenant", ObjectKind::Tenant, s.tenant.as_ref());
    l
}

fn rack_links(r: &Rack) -> Vec<ObjectLink> {
    let mut l = Vec::new();
    push_link(&mut l, "site", ObjectKind::Site, r.site.as_ref());
    push_link(&mut l, "tenant", ObjectKind::Tenant, r.tenant.as_ref());
    l
}

fn vlan_links(v: &Vlan) -> Vec<ObjectLink> {
    let mut l = Vec::new();
    push_link(&mut l, "site", ObjectKind::Site, v.site.as_ref());
    push_link(&mut l, "tenant", ObjectKind::Tenant, v.tenant.as_ref());
    l
}

fn prefix_links(p: &Prefix) -> Vec<ObjectLink> {
    let mut l = Vec::new();
    // The polymorphic scope is navigable only when it's a site (the one scope
    // type with a detail view).
    if p.scope_type.as_deref() == Some("dcim.site") {
        push_link(&mut l, "site", ObjectKind::Site, p.scope.as_ref());
    }
    push_link(&mut l, "vlan", ObjectKind::Vlan, p.vlan.as_ref());
    push_link(&mut l, "tenant", ObjectKind::Tenant, p.tenant.as_ref());
    l
}

fn ip_links(ip: &IpAddress, parent: Option<&Prefix>) -> Vec<ObjectLink> {
    let mut l = Vec::new();
    if let Some(pp) = parent {
        l.push(ObjectLink {
            kind: ObjectKind::Prefix,
            id: pp.id,
            relation: "parent prefix",
            label: pp.prefix.clone(),
        });
    }
    push_link(&mut l, "tenant", ObjectKind::Tenant, ip.tenant.as_ref());
    l
}

/// A rendered detail screen: the object's identity, a title, the summary body,
/// any switchable tabs (empty for objects without sub-resources), and the
/// navigable links to related objects (the `R` jump list; empty when none).
#[derive(Debug, Clone)]
pub struct DetailView {
    pub kind: ObjectKind,
    pub id: u64,
    pub title: String,
    pub body: String,
    pub tabs: Vec<DetailTab>,
    pub links: Vec<ObjectLink>,
}

impl DetailView {
    fn new(kind: ObjectKind, id: u64, title: String, body: String) -> Self {
        Self {
            kind,
            id,
            title,
            body,
            tabs: Vec::new(),
            links: Vec::new(),
        }
    }

    fn with_tabs(mut self, tabs: Vec<DetailTab>) -> Self {
        self.tabs = tabs;
        self
    }

    fn with_links(mut self, links: Vec<ObjectLink>) -> Self {
        self.links = links;
        self
    }
}

/// Build a device detail (summary body + i/p/c/v tabs) from its sub-resources.
/// Reuses the same fan-out + compose path as the CLI/MCP device lookup, then
/// derives the TUI's title, summary body, and per-section tabs from it.
async fn load_device_detail(
    client: &NetBoxClient,
    device: Device,
) -> Result<(String, String, Vec<DetailTab>)> {
    let name = device.name.clone();
    let detail = build_device_detail(client, device).await?;
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
    let mut links = Vec::new();
    let (title, body) = match kind {
        ObjectKind::Device => {
            let d: Device = client
                .get(
                    &format!("/api/dcim/devices/{id}/"),
                    &[("exclude", "config_context".to_string())],
                )
                .await?;
            links = device_links(&d);
            let (title, body, device_tabs) = load_device_detail(client, d).await?;
            tabs = device_tabs;
            (title, body)
        }
        ObjectKind::Site => {
            let s: Site = client.get(&format!("/api/dcim/sites/{id}/"), &[]).await?;
            links = site_links(&s);
            let v = SiteView::from_model(s);
            (format!("site {}", v.name), v.to_key_values().render())
        }
        ObjectKind::Rack => {
            let r: Rack = client.get(&format!("/api/dcim/racks/{id}/"), &[]).await?;
            links = rack_links(&r);
            let v = RackView::from_model(r);
            (format!("rack {}", v.name), v.to_key_values().render())
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
            links = ip_links(&ip, parent.as_ref());
            let v = IpView::build(ip, parent);
            (format!("ip {}", v.address), v.to_key_values().render())
        }
        ObjectKind::Prefix => {
            let p: Prefix = client
                .get(&format!("/api/ipam/prefixes/{id}/"), &[])
                .await?;
            links = prefix_links(&p);
            let cidr = p.prefix.clone();
            let vrf_id = p.vrf.as_ref().map(|v| v.id);
            let children = client.prefix_children(&cidr, vrf_id, SECTION_CAP).await?;
            let ips = client.prefix_ips(&cidr, vrf_id, SECTION_CAP).await?;
            let v = PrefixView::build(p, children, ips);
            (format!("prefix {}", v.prefix), v.to_plain())
        }
        ObjectKind::Vlan => {
            let vlan: Vlan = client.get(&format!("/api/ipam/vlans/{id}/"), &[]).await?;
            links = vlan_links(&vlan);
            let prefixes = client.vlan_prefixes(vlan.id, SECTION_CAP).await?;
            let group = vlan_group_scope(client, &vlan).await?;
            let v = VlanView::build(vlan, prefixes, group);
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
        ObjectKind::Tenant => {
            let t: Tenant = client
                .get(&format!("/api/tenancy/tenants/{id}/"), &[])
                .await?;
            let v = TenantView::from_model(t);
            (format!("tenant {}", v.name), v.to_key_values().render())
        }
        ObjectKind::Contact => {
            let c: Contact = client
                .get(&format!("/api/tenancy/contacts/{id}/"), &[])
                .await?;
            let v = ContactView::from_model(c);
            (format!("contact {}", v.name), v.to_key_values().render())
        }
        ObjectKind::Provider => {
            let p: Provider = client
                .get(&format!("/api/circuits/providers/{id}/"), &[])
                .await?;
            let v = ProviderView::from_model(p);
            (format!("provider {}", v.name), v.to_key_values().render())
        }
        ObjectKind::Vm => {
            let vm: VirtualMachine = client
                .get(
                    &format!("/api/virtualization/virtual-machines/{id}/"),
                    &[("exclude", "config_context".to_string())],
                )
                .await?;
            let v = VmView::from_model(vm);
            (format!("vm {}", v.name), v.to_key_values().render())
        }
        ObjectKind::Cluster => {
            let c: Cluster = client
                .get(&format!("/api/virtualization/clusters/{id}/"), &[])
                .await?;
            let v = ClusterView::from_model(c);
            (format!("cluster {}", v.name), v.to_key_values().render())
        }
    };
    Ok(DetailView::new(kind, id, title, body)
        .with_tabs(tabs)
        .with_links(links))
}

/// A `not_found` closure for the TUI palette path: a typed
/// [`NboxError::NotFound`], so an empty candidate set reads the same way an
/// ambiguous one does (an error status), mirroring the CLI/MCP `not_found`
/// shape. Used by the ambiguity-aware IP resolution in [`load_detail_by_ref`].
fn tui_not_found(noun: &str, value: &str) -> anyhow::Error {
    NboxError::NotFound(format!("no {noun} matched \"{value}\"")).into()
}

/// Load and render a detail by user reference (name/slug/cidr/vid/address),
/// used by the command palette.
pub async fn load_detail_by_ref(
    client: &NetBoxClient,
    kind: ObjectKind,
    value: &str,
) -> Result<DetailView> {
    let mut tabs = Vec::new();
    let mut links = Vec::new();
    let (id, title, body) = match kind {
        ObjectKind::Device => {
            let d = client
                .device_by_ref(value)
                .await?
                .with_context(|| format!("no device matched \"{value}\""))?;
            let id = d.id;
            links = device_links(&d);
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
            links = site_links(&s);
            let v = SiteView::from_model(s);
            (id, format!("site {}", v.name), v.to_key_values().render())
        }
        ObjectKind::Rack => {
            let r = client
                .rack_by_ref(value)
                .await?
                .with_context(|| format!("no rack matched \"{value}\""))?;
            let id = r.id;
            links = rack_links(&r);
            let v = RackView::from_model(r);
            (id, format!("rack {}", v.name), v.to_key_values().render())
        }
        ObjectKind::IpAddress => {
            // Route through the SAME ambiguity-aware resolver the CLI/MCP use
            // (see `ip_view_by_ref`): a bare `into_iter().next()` would silently
            // pick the first of several overlapping IPs (e.g. the same address in
            // different VRFs) and show the WRONG object. With no VRF scope to
            // narrow it, more than one candidate is `Ambiguous`, which surfaces in
            // the TUI as an error status (the same way a NotFound load does).
            let candidates = client.ip_candidates(value).await?;
            let ip = resolve_unique(
                "IP address",
                value,
                candidates,
                query::ip_scope_label,
                &tui_not_found,
            )?;
            let id = ip.id;
            let host = ip
                .address
                .split('/')
                .next()
                .unwrap_or(&ip.address)
                .to_string();
            let vrf_id = ip.vrf.as_ref().map(|v| v.id);
            let parent = most_specific(client.prefixes_containing(&host, vrf_id).await?);
            links = ip_links(&ip, parent.as_ref());
            let v = IpView::build(ip, parent);
            (id, format!("ip {}", v.address), v.to_key_values().render())
        }
        ObjectKind::Prefix => {
            let p = client
                .prefix_by_cidr(value)
                .await?
                .with_context(|| format!("no prefix matched \"{value}\""))?;
            let id = p.id;
            links = prefix_links(&p);
            let cidr = p.prefix.clone();
            let vrf_id = p.vrf.as_ref().map(|v| v.id);
            let children = client.prefix_children(&cidr, vrf_id, SECTION_CAP).await?;
            let ips = client.prefix_ips(&cidr, vrf_id, SECTION_CAP).await?;
            let v = PrefixView::build(p, children, ips);
            (id, format!("prefix {}", v.prefix), v.to_plain())
        }
        ObjectKind::Vlan => {
            let vlan = client
                .vlan_by_ref(value)
                .await?
                .with_context(|| format!("no VLAN matched \"{value}\""))?;
            let id = vlan.id;
            links = vlan_links(&vlan);
            let prefixes = client.vlan_prefixes(vlan.id, SECTION_CAP).await?;
            let group = vlan_group_scope(client, &vlan).await?;
            let v = VlanView::build(vlan, prefixes, group);
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
        ObjectKind::Tenant => {
            let t = client
                .tenant_by_ref(value)
                .await?
                .with_context(|| format!("no tenant matched \"{value}\""))?;
            let id = t.id;
            let v = TenantView::from_model(t);
            (id, format!("tenant {}", v.name), v.to_key_values().render())
        }
        ObjectKind::Contact => {
            let c = client
                .contact_by_ref(value)
                .await?
                .with_context(|| format!("no contact matched \"{value}\""))?;
            let id = c.id;
            let v = ContactView::from_model(c);
            (
                id,
                format!("contact {}", v.name),
                v.to_key_values().render(),
            )
        }
        ObjectKind::Provider => {
            let p = client
                .provider_by_ref(value)
                .await?
                .with_context(|| format!("no provider matched \"{value}\""))?;
            let id = p.id;
            let v = ProviderView::from_model(p);
            (
                id,
                format!("provider {}", v.name),
                v.to_key_values().render(),
            )
        }
        ObjectKind::Vm => {
            let vm = client
                .vm_by_ref(value)
                .await?
                .with_context(|| format!("no virtual machine matched \"{value}\""))?;
            let id = vm.id;
            let v = VmView::from_model(vm);
            (id, format!("vm {}", v.name), v.to_key_values().render())
        }
        ObjectKind::Cluster => {
            let c = client
                .cluster_by_ref(value)
                .await?
                .with_context(|| format!("no cluster matched \"{value}\""))?;
            let id = c.id;
            let v = ClusterView::from_model(c);
            (
                id,
                format!("cluster {}", v.name),
                v.to_key_values().render(),
            )
        }
    };
    Ok(DetailView::new(kind, id, title, body)
        .with_tabs(tabs)
        .with_links(links))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netbox::models::ipam::IpAddress;
    use crate::netbox::query;

    fn ip(id: u64, address: &str, vrf: Option<&str>) -> IpAddress {
        IpAddress {
            id,
            url: format!("http://nb/ipam/ip-addresses/{id}/"),
            address: address.to_string(),
            status: None,
            role: None,
            vrf: vrf.map(|name| BriefObject {
                id: id + 100,
                url: None,
                display: Some(name.to_string()),
                name: Some(name.to_string()),
                slug: None,
                rd: None,
            }),
            tenant: None,
            assigned_object_type: None,
            assigned_object_id: None,
            assigned_object: None,
            dns_name: None,
            description: None,
            tags: Vec::new(),
            custom_fields: serde_json::Value::Null,
        }
    }

    /// Bug A: the TUI/palette IP lookup must route through the same
    /// ambiguity-aware resolver the CLI/MCP use — never silently pick the first
    /// of several overlapping candidates. This exercises the exact resolution the
    /// `IpAddress` arm of `load_detail_by_ref` now performs.
    #[test]
    fn palette_ip_resolution_surfaces_ambiguity_not_first_candidate() {
        // Same address present in two VRFs (no scope to narrow it): ambiguous.
        let candidates = vec![
            ip(1, "10.0.0.1/24", Some("vrf-a")),
            ip(2, "10.0.0.1/24", Some("vrf-b")),
        ];
        let err = resolve_unique(
            "IP address",
            "10.0.0.1",
            candidates,
            query::ip_scope_label,
            &tui_not_found,
        )
        .expect_err("overlapping IPs must be ambiguous, not silently the first");
        // The ambiguity is surfaced as the typed error (the TUI renders this as an
        // error status), and it is NOT the silent first-candidate behavior.
        match err.downcast_ref::<NboxError>() {
            Some(NboxError::Ambiguous { noun, value, .. }) => {
                assert_eq!(noun, "IP address");
                assert_eq!(value, "10.0.0.1");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    /// And the unambiguous case still resolves to the one candidate unchanged.
    #[test]
    fn palette_ip_resolution_unambiguous_resolves() {
        let candidates = vec![ip(7, "10.0.0.1/24", Some("vrf-a"))];
        let resolved = resolve_unique(
            "IP address",
            "10.0.0.1",
            candidates,
            query::ip_scope_label,
            &tui_not_found,
        )
        .expect("a single candidate resolves");
        assert_eq!(resolved.id, 7);
    }

    /// An empty candidate set is a typed NotFound (so the TUI surfaces it the same
    /// way as an ambiguous one — an error status), via the `tui_not_found` shape.
    #[test]
    fn palette_ip_resolution_empty_is_not_found() {
        let err = resolve_unique(
            "IP address",
            "10.0.0.99",
            Vec::<IpAddress>::new(),
            query::ip_scope_label,
            &tui_not_found,
        )
        .expect_err("no candidates → not found");
        assert!(matches!(
            err.downcast_ref::<NboxError>(),
            Some(NboxError::NotFound(_))
        ));
    }

    #[test]
    fn device_links_cover_site_rack_tenant_and_primary_ips() {
        use serde_json::json;
        let d: Device = serde_json::from_value(json!({
            "id": 1, "url": "u", "name": "edge01",
            "site": {"id": 5, "name": "iad1", "display": "iad1"},
            "rack": {"id": 7, "name": "R1", "display": "R1"},
            "tenant": {"id": 9, "name": "acme", "display": "acme"},
            "primary_ip4": {"id": 11, "display": "10.0.0.1/24"},
        }))
        .unwrap();
        let got: Vec<(ObjectKind, u64, &str)> = device_links(&d)
            .iter()
            .map(|l| (l.kind, l.id, l.relation))
            .collect();
        assert!(got.contains(&(ObjectKind::Site, 5, "site")));
        assert!(
            got.contains(&(ObjectKind::Rack, 7, "rack")),
            "device→rack link"
        );
        assert!(got.contains(&(ObjectKind::Tenant, 9, "tenant")));
        assert!(got.contains(&(ObjectKind::IpAddress, 11, "primary IPv4")));
        // No primary IPv6 in the fixture → no such link (absent relations skipped).
        assert!(!got.iter().any(|(_, _, r)| *r == "primary IPv6"));
    }

    #[test]
    fn prefix_links_navigate_site_scope_and_vlan_but_not_vrf() {
        use serde_json::json;
        let p: Prefix = serde_json::from_value(json!({
            "id": 2, "url": "u", "prefix": "10.0.0.0/16",
            "scope_type": "dcim.site",
            "scope": {"id": 5, "name": "iad1", "display": "iad1"},
            "vlan": {"id": 8, "display": "vlan 100"},
            "vrf": {"id": 3, "name": "blue", "display": "blue"},
            "tenant": {"id": 9, "name": "acme", "display": "acme"},
        }))
        .unwrap();
        let got: Vec<(ObjectKind, &str)> = prefix_links(&p)
            .iter()
            .map(|l| (l.kind, l.relation))
            .collect();
        assert!(
            got.contains(&(ObjectKind::Site, "site")),
            "site scope navigable"
        );
        assert!(got.contains(&(ObjectKind::Vlan, "vlan")));
        assert!(got.contains(&(ObjectKind::Tenant, "tenant")));
        // A VRF has no detail kind, so it is never emitted as a link.
        assert!(!got.iter().any(|(_, r)| *r == "vrf"));
    }

    #[test]
    fn ip_links_navigate_to_parent_prefix() {
        use serde_json::json;
        let addr = ip(1, "10.0.0.5/24", None);
        let parent: Prefix =
            serde_json::from_value(json!({"id": 42, "url": "u", "prefix": "10.0.0.0/24"})).unwrap();
        let with_parent = ip_links(&addr, Some(&parent));
        assert!(
            with_parent.iter().any(|l| l.kind == ObjectKind::Prefix
                && l.id == 42
                && l.relation == "parent prefix"),
            "an IP links to its most-specific parent prefix"
        );
        // No parent resolved → no parent-prefix link.
        assert!(
            !ip_links(&addr, None)
                .iter()
                .any(|l| l.relation == "parent prefix")
        );
    }
}
