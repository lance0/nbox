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
use serde::{Deserialize, Serialize};

use crate::config::ApiSurface;
use crate::domain::aggregate_view::AggregateView;
use crate::domain::asn_view::AsnView;
use crate::domain::circuit_view::{CircuitView, DeviceRef, PathHop, ResolvedTermination};
use crate::domain::cluster_view::ClusterView;
use crate::domain::contact_view::ContactView;
use crate::domain::device_detail::{CableRow, DeviceDetail, IfaceRow, IpRow, VlanRow};
use crate::domain::interface_view::InterfaceView;
use crate::domain::ip_range_view::IpRangeView;
use crate::domain::ip_view::{IpView, assigned_label, most_specific};
use crate::domain::journal_view::{JournalEntryRow, JournalView};
use crate::domain::mac_view::MacView;
use crate::domain::prefix_view::PrefixView;
use crate::domain::provider_view::ProviderView;
use crate::domain::rack_group_view::RackGroupView;
use crate::domain::rack_view::RackView;
use crate::domain::route_target_view::{RouteTargetDetail, RouteTargetView, VrfRef};
use crate::domain::site_view::SiteView;
use crate::domain::tenant_view::TenantView;
use crate::domain::virtual_circuit_view::VirtualCircuitView;
use crate::domain::vlan_view::VlanView;
use crate::domain::vm_type_view::VirtualMachineTypeView;
use crate::domain::vm_view::VmView;
use crate::domain::vrf_view::{VrfAddressRow, VrfDetail, VrfPrefixRow, VrfView};
use crate::error::NboxError;
use crate::netbox::client::NetBoxClient;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::models::circuits::{Circuit, CircuitTermination, Provider, VirtualCircuit};
use crate::netbox::models::common::BriefObject;
use crate::netbox::models::dcim::{Device, Interface, MacAddress, Rack, RackGroup, Site};
use crate::netbox::models::ipam::{
    Aggregate, Asn, AvailableIp, IpAddress, IpRange, Prefix, RouteTarget, Vlan, VlanGroup, Vrf,
};
use crate::netbox::models::tenancy::{Contact, Tenant};
use crate::netbox::models::virtualization::{Cluster, VirtualMachine, VirtualMachineType};
use crate::netbox::prefix_tree::build_nodes;
use crate::netbox::query;
use crate::netbox::search::ObjectKind;
use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::netbox::mutation::{
    self, FieldChange, MutationPlan, MutationReceipt, Operation, PLAN_SCHEMA_VERSION, PlanTarget,
    Precondition,
};
use serde_json::Value;

/// Cap on the child rows pulled into most detail-view sections: a device's
/// interfaces/IPs/services, a prefix's child prefixes, a VLAN's referencing
/// prefixes, or a VRF's scoped prefixes/addresses. One concept
/// ("rows in one detail section") → one cap, named at the rendering layer the
/// cap operates on, not the dcim/ipam domain layer. Sized generously below
/// NetBox's `MAX_PAGE_SIZE` (1000) so a section-full is a single round trip.
/// NOTE: prefix contained IPs use a targeted higher cap below so a full IPv4
/// `/24` fits without changing every detail tab's fetch budget.
const DETAIL_SECTION_CAP: usize = 200;
/// Prefix detail's contained-address tab needs room for a full IPv4 `/24` while
/// still staying bounded and below NetBox's `MAX_PAGE_SIZE` (1000).
const PREFIX_CONTAINED_IP_CAP: usize = 512;
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
/// interfaces, IPs, and services (cap [`DETAIL_SECTION_CAP`]) and compose the view.
/// Shared by the CLI `device` handler and the MCP `nbox_get` device arm.
async fn build_device_detail(client: &NetBoxClient, device: Device) -> Result<DeviceDetail> {
    let id = device.id;
    let (interfaces, ips, services) = tokio::try_join!(
        client.device_interfaces(id, DETAIL_SECTION_CAP),
        client.device_ips(id, DETAIL_SECTION_CAP),
        client.device_services(id, DETAIL_SECTION_CAP),
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

/// Split an interface reference `device/name` into its two parts. The device is
/// the first segment; the name is EVERYTHING after the first `/` verbatim —
/// interface names may contain slashes (e.g. `xe-0/0/1`, `Ethernet1/49`). A ref
/// with no `/`, or an empty device/name, is a usage error. Shared by the
/// `nbox open`/journal/`nbox_get` interface paths so they all parse identically.
pub(crate) fn split_interface_ref(value: &str) -> Result<(&str, &str)> {
    value
        .split_once('/')
        .filter(|(d, n)| !d.is_empty() && !n.is_empty())
        .ok_or_else(|| {
            NboxError::Usage(format!(
                "interface reference must be `<device>/<name>` (e.g. edge01/xe-0/0/1) — interface names may contain slashes; the part after the device is the name verbatim. Got \"{value}\"."
            ))
            .into()
        })
}

/// Resolve one interface on a device (by the device's user reference + the
/// interface name) to the full [`Interface`] record. Shared by the interface
/// detail view and the journal id-resolution path so they can't drift.
pub(crate) async fn resolve_interface(
    client: &NetBoxClient,
    device: &str,
    interface: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<crate::netbox::models::dcim::Interface> {
    let dev = client
        .device_by_ref(device)
        .await?
        .ok_or_else(|| not_found("device", device))?;
    client
        .device_interface(dev.id, interface)
        .await?
        .ok_or_else(|| not_found("interface", interface))
}

/// `interface <device> <interface>`: resolve one interface on a device and
/// build its view (assigned IPs + cable-path trace). Shared by CLI/MCP.
pub async fn interface_view_by_ref(
    client: &NetBoxClient,
    device: &str,
    interface: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<InterfaceView> {
    let iface = resolve_interface(client, device, interface, not_found).await?;
    let (ips, trace) = tokio::try_join!(
        client.interface_ips(iface.id, DETAIL_SECTION_CAP),
        client.interface_trace(iface.id),
    )?;
    Ok(InterfaceView::build(iface, ips, trace))
}

// ===== Safe write foundation: interface description pilot (ADR-0001) =====
//
// The first write command is operation- and field-specific: update one
// interface's `description`. The planner builds a `MutationPlan` from the live
// object (+ an ETag precondition on 4.6+, else `last_updated` + before-hash);
// `apply_*` sends the minimal `PATCH` with the write-engine semantics from
// ADR-0001 (confirm-token check, no-op short-circuit, stale-precondition
// handling, receipt). No generic `edit`, no free-form patch — see ROADMAP.

/// The only writable field on an interface in v1 (ADR-0001 §6: operation- and
/// field-specific, not a generic editor). A different field fails closed at
/// plan time with an actionable usage error.
pub(crate) const INTERFACE_WRITABLE_FIELD: &str = "description";

/// Plan an interface `description` update: resolve the interface, read the
/// authoritative current state with its `ETag` (4.6+) or `last_updated`
/// (pre-4.6), derive the minimal `PATCH`, and build the [`MutationPlan`].
///
/// `new_description` is the desired value verbatim (empty string clears it). A
/// no-op (current value already matches) yields a plan with `no_op: true` and
/// an empty `patch`; applying it sends no `PATCH` and reports "no change".
/// `changelog_message` is validated against NetBox's length limit here, before
/// any network write, so an over-length message is a usage error (exit 2).
pub(crate) async fn plan_interface_description_update(
    client: &NetBoxClient,
    device: &str,
    interface: &str,
    new_description: &str,
    changelog_message: Option<&str>,
    profile: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<MutationPlan> {
    // The message length is input validation, not a server round-trip — check
    // before resolving so a bad `--message` fails fast with no network use.
    let message = match changelog_message {
        Some(m) if !m.is_empty() => Some(m.to_string()),
        _ => None,
    };
    mutation::validate_changelog_message(&message)?;

    // 1) Resolve the interface by name (device + name → id). Reuses the read
    //    resolution path so ambiguity / not-found / case-insensitive fallback
    //    behave exactly like `nbox interface`.
    let resolved = resolve_interface(client, device, interface, not_found).await?;
    // 2) Fetch the authoritative current state with its `ETag` header. The list
    //    above gave us the id; the detail gives the ETag (4.6+) plus the
    //    canonical object. On pre-4.6 the ETag is `None` and the planner falls
    //    back to `last_updated` + a before-hash (ADR-0001 §3).
    let endpoint = format!("/api/dcim/interfaces/{}/", resolved.id);
    let (current, etag): (Interface, Option<String>) = client.get_with_etag(&endpoint, &[]).await?;

    let target = PlanTarget {
        kind: "interface".to_string(),
        r#ref: format!("{device}/{interface}"),
        id: current.id,
        display: current
            .display
            .clone()
            .unwrap_or_else(|| format!("{device}/{interface}")),
        endpoint,
        profile: profile.to_string(),
    };

    // The before-hash covers the in-scope field's current value, so a concurrent
    // writer that changes `description` (or anything that bumps `last_updated`)
    // is caught at apply even without an ETag.
    let precondition = match etag {
        Some(e) => Precondition::Etag { etag: e },
        None => Precondition::LastUpdated {
            last_updated: current.last_updated.clone(),
            before_hash: interface_before_hash(&current),
        },
    };

    let before = serde_json::to_value(&current.description).unwrap_or(Value::Null);
    let after = serde_json::to_value(new_description).unwrap_or(Value::Null);
    let no_op = current.description.as_deref() == Some(new_description);
    let patch = if no_op {
        serde_json::json!({})
    } else {
        serde_json::json!({ "description": new_description })
    };
    let fields = vec![FieldChange {
        field: INTERFACE_WRITABLE_FIELD.to_string(),
        before: before.clone(),
        after: after.clone(),
    }];

    let expires_epoch = mutation::plan_expiry_epoch(SystemTime::now());
    let confirm_token = mutation::confirm_token(
        &target,
        Operation::Update,
        &precondition,
        &patch,
        &message,
        expires_epoch,
    );

    Ok(MutationPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        operation: Operation::Update,
        target,
        precondition,
        fields,
        patch,
        no_op,
        warnings: Vec::new(),
        errors: Vec::new(),
        changelog_message: message,
        confirm_token,
        expires_at: mutation::format_iso_utc(expires_epoch),
    })
}

/// Apply a planned interface `description` update (ADR-0001 §5 steps 5–7):
/// verify the plan's confirmation token + expiry, short-circuit a no-op, then
/// send the minimal `PATCH` with the recorded precondition. Returns a
/// [`MutationReceipt`] (re-fetch after success is the PATCH response itself —
/// NetBox returns the updated object — plus the new `ETag` when present).
pub(crate) async fn apply_interface_description_update(
    client: &NetBoxClient,
    plan: &MutationPlan,
) -> Result<MutationReceipt> {
    plan.verify()?;

    if plan.no_op {
        return Ok(no_op_receipt(plan));
    }

    // The wire body is the object-field patch plus the opt-in changelog_message
    // (a NetBox write-only request field, recorded in the object-change entry,
    // never stored on the object).
    let mut body = plan.patch.clone();
    if let Some(msg) = &plan.changelog_message {
        body["changelog_message"] = serde_json::json!(msg);
    }

    let (_updated, new_etag, status): (Interface, Option<String>, u16) = match &plan.precondition {
        Precondition::Etag { etag } => {
            client
                .patch(&plan.target.endpoint, &body, Some(etag))
                .await?
        }
        Precondition::LastUpdated {
            last_updated,
            before_hash,
        } => {
            // Read-before-write (pre-4.6 fallback): re-fetch and refuse if the
            // object moved. `last_updated` ticking is the primary signal; the
            // before-hash is a belt-and-suspenders guard for the rare case it
            // didn't.
            let (current, _): (Interface, Option<String>) =
                client.get_with_etag(&plan.target.endpoint, &[]).await?;
            if current.last_updated != *last_updated
                || interface_before_hash(&current) != *before_hash
            {
                return Err(NboxError::StalePrecondition(String::new()).into());
            }
            client.patch(&plan.target.endpoint, &body, None).await?
        }
        // An update plan always carries an Etag or LastUpdated precondition;
        // `None` is the allocate path, which never reaches an update apply.
        Precondition::None => {
            anyhow::bail!("internal error: an update plan carried a None precondition")
        }
    };

    Ok(MutationReceipt {
        schema_version: PLAN_SCHEMA_VERSION,
        operation: plan.operation,
        target: plan.target.clone(),
        fields: plan.fields.clone(),
        applied: true,
        no_op: false,
        status,
        etag: new_etag,
        request_id: None,
        object: None,
        message: format!(
            "applied: {} {} ({})",
            plan.target.kind,
            plan.target.display,
            plan.changed_field_names().join(", ")
        ),
    })
}

/// Build a no-op receipt: the current value already matches, so no `PATCH` is
/// sent. ADR-0001 §8 wording: "no change: current value already matches".
pub(crate) fn no_op_receipt(plan: &MutationPlan) -> MutationReceipt {
    MutationReceipt {
        schema_version: PLAN_SCHEMA_VERSION,
        operation: plan.operation,
        target: plan.target.clone(),
        fields: plan.fields.clone(),
        applied: false,
        no_op: true,
        status: 0,
        etag: None,
        request_id: None,
        object: None,
        message: "no change: current value already matches".to_string(),
    }
}

/// Normalized before-hash over the interface's in-scope writable field
/// (`description`). The apply re-reads and recomputes this; a mismatch vs the
/// plan's recorded `before_hash` means the object changed in NetBox.
fn interface_before_hash(iface: &Interface) -> String {
    let mut m = BTreeMap::new();
    m.insert(
        INTERFACE_WRITABLE_FIELD.to_string(),
        serde_json::to_value(&iface.description).unwrap_or(Value::Null),
    );
    mutation::before_hash(&m)
}

// ===== Safe write follow-on: device status (ADR-0001) =====================
//
// The second write command reuses the same planner/diff/confirm/concurrency/
// audit contracts as the interface-description pilot. The new piece is choice
// validation: `status` is a server-enumerated field, so the planner asks
// NetBox (read-only `OPTIONS`) for the allowed values and normalizes the
// operator's input to the canonical wire value BEFORE building the plan — an
// unknown or ambiguous status is a usage error (exit 2) with no `PATCH`.
// Still operation- and field-specific (one field: `status`); no generic editor.

/// The only writable field on a device in this pilot (ADR-0001 §6).
pub(crate) const DEVICE_WRITABLE_FIELD: &str = "status";

/// Plan a device `status` update: enumerate the allowed status values from
/// NetBox (read-only `OPTIONS`) and normalize the input, resolve the device,
/// read the authoritative current state with its `ETag` (4.6+) or `last_updated`
/// (pre-4.6), derive the minimal `PATCH` (`{"status": "<value>"}`), and build
/// the [`MutationPlan`].
///
/// `new_status` is the operator's input verbatim — a canonical value
/// (`active`) or a label (`Active`) matched case-insensitively when it maps
/// unambiguously to one value. Unknown/ambiguous input is a usage error here,
/// before any `PATCH`, naming the input and listing the allowed canonical
/// values. A no-op (current value already matches) yields `no_op: true` and an
/// empty `patch`. `changelog_message` is validated against NetBox's length
/// limit here, before any network write.
pub(crate) async fn plan_device_status_update(
    client: &NetBoxClient,
    device: &str,
    new_status: &str,
    changelog_message: Option<&str>,
    profile: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<MutationPlan> {
    // Message length is pure input validation — check first, before any network.
    let message = match changelog_message {
        Some(m) if !m.is_empty() => Some(m.to_string()),
        _ => None,
    };
    mutation::validate_changelog_message(&message)?;

    // Enumerate the allowed status values from NetBox (read-only OPTIONS) and
    // normalize the operator's input to the canonical wire value. An
    // unknown/ambiguous input is a usage error (exit 2) BEFORE resolving the
    // device — no PATCH is ever built from an unvalidated value (ADR-0001 §6).
    let choices = client.device_status_choices().await?;
    // Empty metadata means the OPTIONS enumeration came back without choices
    // (an unexpected schema, a permission-stripped `actions`, or a proxy that
    // dropped the body). Fail with a clear cause rather than letting
    // `resolve_choice` report the input as an invalid value against an empty
    // allow-list — and never send an unvalidated write (ADR-0001 §6).
    if choices.is_empty() {
        return Err(NboxError::Usage(format!(
            "could not enumerate allowed values for device {DEVICE_WRITABLE_FIELD} from NetBox \
             OPTIONS; refusing to send an unvalidated write"
        ))
        .into());
    }
    let normalized =
        crate::netbox::choices::resolve_choice(&choices, DEVICE_WRITABLE_FIELD, new_status)?;

    // Resolve the device by reference (reuses the read path so ambiguity /
    // not-found / case-insensitive fallback behave exactly like `nbox device`).
    let dev = client
        .device_by_ref(device)
        .await?
        .ok_or_else(|| not_found("device", device))?;
    // Read the authoritative current state with its `ETag` header (4.6+) or
    // `last_updated` (pre-4.6 fallback). The list above gave us the id; the
    // detail gives the ETag plus the canonical object.
    let endpoint = format!("/api/dcim/devices/{}/", dev.id);
    let (current, etag): (Device, Option<String>) = client.get_with_etag(&endpoint, &[]).await?;

    let target = PlanTarget {
        kind: "device".to_string(),
        r#ref: device.to_string(),
        id: current.id,
        display: current
            .display
            .clone()
            .or_else(|| Some(current.name.clone()))
            .unwrap_or_else(|| device.to_string()),
        endpoint,
        profile: profile.to_string(),
    };

    let precondition = match etag {
        Some(e) => Precondition::Etag { etag: e },
        None => Precondition::LastUpdated {
            last_updated: current.last_updated.clone(),
            before_hash: device_before_hash(&current),
        },
    };

    // The current status value (the canonical wire value, e.g. "active").
    let current_status = current.status.as_ref().map(|c| c.value.clone());
    let before = serde_json::to_value(&current_status).unwrap_or(Value::Null);
    let after = serde_json::to_value(&normalized).unwrap_or(Value::Null);
    let no_op = current_status.as_deref() == Some(normalized.as_str());
    let patch = if no_op {
        serde_json::json!({})
    } else {
        serde_json::json!({ "status": normalized })
    };
    let fields = vec![FieldChange {
        field: DEVICE_WRITABLE_FIELD.to_string(),
        before: before.clone(),
        after: after.clone(),
    }];

    let expires_epoch = mutation::plan_expiry_epoch(SystemTime::now());
    let confirm_token = mutation::confirm_token(
        &target,
        Operation::Update,
        &precondition,
        &patch,
        &message,
        expires_epoch,
    );

    Ok(MutationPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        operation: Operation::Update,
        target,
        precondition,
        fields,
        patch,
        no_op,
        warnings: Vec::new(),
        errors: Vec::new(),
        changelog_message: message,
        confirm_token,
        expires_at: mutation::format_iso_utc(expires_epoch),
    })
}

/// Apply a planned device `status` update (ADR-0001 §5 steps 5–7): verify the
/// plan's confirmation token + expiry, short-circuit a no-op, then send the
/// minimal `PATCH` (`{"status": "<value>"}`) with the recorded precondition.
/// Returns a [`MutationReceipt`] from the PATCH response.
pub(crate) async fn apply_device_status_update(
    client: &NetBoxClient,
    plan: &MutationPlan,
) -> Result<MutationReceipt> {
    plan.verify()?;

    if plan.no_op {
        return Ok(no_op_receipt(plan));
    }

    // The wire body is the object-field patch plus the opt-in changelog_message
    // (a NetBox write-only request field, recorded in the object-change entry,
    // never stored on the object).
    let mut body = plan.patch.clone();
    if let Some(msg) = &plan.changelog_message {
        body["changelog_message"] = serde_json::json!(msg);
    }

    let (_updated, new_etag, status): (Device, Option<String>, u16) = match &plan.precondition {
        Precondition::Etag { etag } => {
            client
                .patch(&plan.target.endpoint, &body, Some(etag))
                .await?
        }
        Precondition::LastUpdated {
            last_updated,
            before_hash,
        } => {
            // Read-before-write (pre-4.6 fallback): re-fetch and refuse if the
            // object moved. `last_updated` ticking is the primary signal; the
            // before-hash is a belt-and-suspenders guard for the rare case it
            // didn't.
            let (current, _): (Device, Option<String>) =
                client.get_with_etag(&plan.target.endpoint, &[]).await?;
            if current.last_updated != *last_updated || device_before_hash(&current) != *before_hash
            {
                return Err(NboxError::StalePrecondition(String::new()).into());
            }
            client.patch(&plan.target.endpoint, &body, None).await?
        }
        Precondition::None => {
            anyhow::bail!("internal error: an update plan carried a None precondition")
        }
    };

    Ok(MutationReceipt {
        schema_version: PLAN_SCHEMA_VERSION,
        operation: plan.operation,
        target: plan.target.clone(),
        fields: plan.fields.clone(),
        applied: true,
        no_op: false,
        status,
        etag: new_etag,
        request_id: None,
        object: None,
        message: format!(
            "applied: {} {} ({})",
            plan.target.kind,
            plan.target.display,
            plan.changed_field_names().join(", ")
        ),
    })
}

/// Normalized before-hash over the device's in-scope writable field (`status`).
/// The apply re-reads and recomputes this; a mismatch vs the plan's recorded
/// `before_hash` means the object changed in NetBox.
fn device_before_hash(dev: &Device) -> String {
    let mut m = BTreeMap::new();
    m.insert(
        DEVICE_WRITABLE_FIELD.to_string(),
        serde_json::to_value(dev.status.as_ref().map(|c| c.value.clone())).unwrap_or(Value::Null),
    );
    mutation::before_hash(&m)
}

/// Plan an `ip reserve <prefix>` allocation (ADR-0001 §5 steps 1–4): validate the
/// optional changelog message, resolve the prefix (scoped by `vrf`), and build an
/// `Allocate` plan whose body POSTs to the prefix's `available-ips` endpoint.
///
/// The endpoint is server-side race-safe (NetBox never hands out the same address
/// twice), so the plan carries [`Precondition::None`] — there is no prior object,
/// ETag, or `last_updated` to bind. A dry-run surfaces the *currently* next
/// address as an advisory warning: NetBox allocates at apply time, so the applied
/// address may differ. Only `description` / `dns_name` may be set (the v1 narrow
/// allow-list — no status/role/tags/assignment).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn plan_ip_reserve(
    client: &NetBoxClient,
    prefix: &str,
    vrf: Option<&str>,
    description: Option<&str>,
    dns_name: Option<&str>,
    changelog_message: Option<&str>,
    profile: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<MutationPlan> {
    // Message length is pure input validation — check first, before any network.
    let message = match changelog_message {
        Some(m) if !m.is_empty() => Some(m.to_string()),
        _ => None,
    };
    mutation::validate_changelog_message(&message)?;

    // Resolve the prefix by CIDR (reuses the read path so ambiguity / not-found /
    // VRF scoping behave exactly like `nbox prefix` / `nbox next-ip`).
    let prefix_obj = resolve_prefix(client, prefix, vrf, not_found).await?;
    let endpoint = format!("/api/ipam/prefixes/{}/available-ips/", prefix_obj.id);

    let target = PlanTarget {
        kind: "ip".to_string(),
        r#ref: prefix.to_string(),
        id: prefix_obj.id,
        display: prefix_obj.prefix.clone(),
        endpoint,
        profile: profile.to_string(),
    };

    // The minimal POST body: only the fields the operator actually set. A bare
    // reserve sends `{}` and takes NetBox's defaults. `fields` is the create diff
    // (`null → value`) for each set field, never a synthetic address.
    let mut body = serde_json::Map::new();
    let mut fields = Vec::new();
    if let Some(d) = description.filter(|s| !s.is_empty()) {
        body.insert("description".to_string(), serde_json::json!(d));
        fields.push(FieldChange {
            field: "description".to_string(),
            before: Value::Null,
            after: serde_json::json!(d),
        });
    }
    if let Some(d) = dns_name.filter(|s| !s.is_empty()) {
        body.insert("dns_name".to_string(), serde_json::json!(d));
        fields.push(FieldChange {
            field: "dns_name".to_string(),
            before: Value::Null,
            after: serde_json::json!(d),
        });
    }
    let patch = Value::Object(body);
    let precondition = Precondition::None;

    // Dry-run advisory: the address NetBox would currently hand out. Read-only,
    // and never part of the body — another client could allocate between this
    // read and the apply POST, so the applied address may differ.
    let mut warnings = Vec::new();
    match client.prefix_available_ips(prefix_obj.id, 1).await {
        Ok(list) => match list.first() {
            Some(next) => warnings.push(format!(
                "currently next: {} — NetBox allocates at apply; the applied address may differ",
                next.address
            )),
            None => warnings.push(
                "no available addresses in this prefix — the reserve will fail at apply"
                    .to_string(),
            ),
        },
        // A failed advisory read must not block planning; the apply POST is the
        // authoritative attempt. Note it and carry on.
        Err(_) => {
            warnings.push("could not read the next available address (advisory only)".to_string());
        }
    }

    let expires_epoch = mutation::plan_expiry_epoch(SystemTime::now());
    let confirm_token = mutation::confirm_token(
        &target,
        Operation::Allocate,
        &precondition,
        &patch,
        &message,
        expires_epoch,
    );

    Ok(MutationPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        operation: Operation::Allocate,
        target,
        precondition,
        fields,
        patch,
        no_op: false,
        warnings,
        errors: Vec::new(),
        changelog_message: message,
        confirm_token,
        expires_at: mutation::format_iso_utc(expires_epoch),
    })
}

/// Apply a planned IP reservation (ADR-0001 §5 steps 5–7): verify the plan's
/// confirmation token + expiry, then POST the minimal body to the prefix's
/// `available-ips` endpoint. NetBox allocates the next free address and returns
/// the created IP object (`201`), which becomes the receipt's `object`.
pub(crate) async fn apply_ip_reserve(
    client: &NetBoxClient,
    plan: &MutationPlan,
) -> Result<MutationReceipt> {
    plan.verify()?;

    // The wire body is the minimal create patch plus the opt-in changelog_message
    // (a NetBox write-only request field, recorded in the object-change entry,
    // never stored on the object).
    let mut body = plan.patch.clone();
    if let Some(msg) = &plan.changelog_message {
        body["changelog_message"] = serde_json::json!(msg);
    }

    let (created, status): (IpAddress, u16) = client.post(&plan.target.endpoint, &body).await?;
    let address = created.address.clone();
    // Build the created IP's view (no parent-prefix enrichment on the write path —
    // the reserve returns the bare object) and stash it as the receipt's `object`.
    let view = IpView::build(created, None);
    let object = serde_json::to_value(&view).ok();

    Ok(MutationReceipt {
        schema_version: PLAN_SCHEMA_VERSION,
        operation: plan.operation,
        target: plan.target.clone(),
        fields: plan.fields.clone(),
        applied: true,
        no_op: false,
        status,
        etag: None,
        request_id: None,
        object,
        message: format!("reserved: {} in {}", address, plan.target.display),
    })
}

/// Plan a `prefix reserve <cidr>` allocation (ADR-0001 §5 steps 1–4): validate
/// the optional changelog message, resolve the parent prefix (scoped by
/// `vrf`), and build an `Allocate` plan whose body POSTs to the parent's
/// `available-prefixes` endpoint.
///
/// The endpoint is server-side race-safe (NetBox never hands out the same
/// block twice), so the plan carries [`Precondition::None`] — there is no prior
/// object, ETag, or `last_updated` to bind. A dry-run surfaces the *currently*
/// next available block as an advisory warning: NetBox allocates at apply time,
/// so the applied block may differ. Only `description` may be set (the v1
/// narrow allow-list — no status/role/tags/vlan).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn plan_prefix_reserve(
    client: &NetBoxClient,
    prefix: &str,
    vrf: Option<&str>,
    length: Option<u8>,
    description: Option<&str>,
    changelog_message: Option<&str>,
    profile: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<MutationPlan> {
    // Message length is pure input validation — check first, before any network.
    let message = match changelog_message {
        Some(m) if !m.is_empty() => Some(m.to_string()),
        _ => None,
    };
    mutation::validate_changelog_message(&message)?;

    // Resolve the parent prefix by CIDR (same resolver as `nbox prefix` /
    // `nbox next-prefix`).
    let prefix_obj = resolve_prefix(client, prefix, vrf, not_found).await?;
    let endpoint = format!("/api/ipam/prefixes/{}/available-prefixes/", prefix_obj.id);

    let target = PlanTarget {
        kind: "prefix".to_string(),
        r#ref: prefix.to_string(),
        id: prefix_obj.id,
        display: prefix_obj.prefix.clone(),
        endpoint,
        profile: profile.to_string(),
    };

    // The minimal POST body: only the fields the operator actually set. A bare
    // reserve sends `{}` and takes NetBox's defaults. `prefix_length` is the
    // NetBox field for the desired child block size.
    let mut body = serde_json::Map::new();
    let mut fields = Vec::new();
    if let Some(len) = length {
        body.insert("prefix_length".to_string(), serde_json::json!(len));
        fields.push(FieldChange {
            field: "prefix_length".to_string(),
            before: Value::Null,
            after: serde_json::json!(len),
        });
    }
    if let Some(d) = description.filter(|s| !s.is_empty()) {
        body.insert("description".to_string(), serde_json::json!(d));
        fields.push(FieldChange {
            field: "description".to_string(),
            before: Value::Null,
            after: serde_json::json!(d),
        });
    }
    let patch = Value::Object(body);
    let precondition = Precondition::None;

    // Dry-run advisory: the block NetBox would currently hand out. Read-only,
    // and never part of the body — another client could allocate between this
    // read and the apply POST, so the applied block may differ.
    let mut warnings = Vec::new();
    match client.prefix_available_prefixes(prefix_obj.id).await {
        Ok(list) if list.is_empty() => {
            warnings.push(
                "no available prefixes in this prefix — the reserve will fail at apply".to_string(),
            );
        }
        Ok(list) => {
            // If a length was requested, filter for the first block that
            // satisfies it (client-side, matching `next-prefix --length`).
            let candidate = if let Some(len) = length {
                list.iter()
                    .find(|p| prefix_satisfies_length(&p.prefix, len))
            } else {
                list.first()
            };
            match candidate {
                Some(c) => warnings.push(format!(
                    "currently next: {} — NetBox allocates at apply; the applied prefix may differ",
                    c.prefix
                )),
                None => {
                    let requested = length.map_or("any".to_string(), |l| format!("/{l}"));
                    warnings.push(format!(
                        "no available block of length {requested} in this prefix — the reserve will fail at apply"
                    ));
                }
            }
        }
        // A failed advisory read must not block planning; the apply POST is the
        // authoritative attempt. Note it and carry on.
        Err(_) => {
            warnings.push("could not read the next available prefix (advisory only)".to_string());
        }
    }

    let expires_epoch = mutation::plan_expiry_epoch(SystemTime::now());
    let confirm_token = mutation::confirm_token(
        &target,
        Operation::Allocate,
        &precondition,
        &patch,
        &message,
        expires_epoch,
    );

    Ok(MutationPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        operation: Operation::Allocate,
        target,
        precondition,
        fields,
        patch,
        no_op: false,
        warnings,
        errors: Vec::new(),
        changelog_message: message,
        confirm_token,
        expires_at: mutation::format_iso_utc(expires_epoch),
    })
}

/// Apply a planned prefix reservation (ADR-0001 §5 steps 5–7): verify the
/// plan's confirmation token + expiry, then POST the minimal body to the
/// parent's `available-prefixes` endpoint. NetBox allocates the next free
/// block and returns the created prefix object (`201`), which becomes the
/// receipt's `object`.
pub(crate) async fn apply_prefix_reserve(
    client: &NetBoxClient,
    plan: &MutationPlan,
) -> Result<MutationReceipt> {
    plan.verify()?;

    // The wire body is the minimal create patch plus the opt-in changelog_message.
    let mut body = plan.patch.clone();
    if let Some(msg) = &plan.changelog_message {
        body["changelog_message"] = serde_json::json!(msg);
    }

    let (created, status): (Prefix, u16) = client.post(&plan.target.endpoint, &body).await?;
    let created_prefix = created.prefix.clone();
    let object = serde_json::to_value(&created).ok();

    Ok(MutationReceipt {
        schema_version: PLAN_SCHEMA_VERSION,
        operation: plan.operation,
        target: plan.target.clone(),
        fields: plan.fields.clone(),
        applied: true,
        no_op: false,
        status,
        etag: None,
        request_id: None,
        object,
        message: format!("reserved: {} in {}", created_prefix, plan.target.display),
    })
}

/// True if `cidr` (e.g. `10.0.0.0/26`) has a prefix length ≤ `target` — a
/// block of size /26 can carve a /28 but not a /24. NetBox's `available-
/// prefixes` returns blocks of any size; the dry-run advisory filters for the
/// first block that can satisfy the requested length.
fn prefix_satisfies_length(cidr: &str, target: u8) -> bool {
    cidr.rsplit_once('/')
        .is_some_and(|(_, len_str)| len_str.parse::<u8>().is_ok_and(|len| len <= target))
}

/// Plan an `ip-range reserve <start|id>` allocation (ADR-0001 §5 steps 1–4):
/// validate the optional changelog message, resolve the IP range by start
/// address or ID, and build an `Allocate` plan whose body POSTs to the range's
/// `available-ips` endpoint.
///
/// The endpoint is server-side race-safe (NetBox never hands out the same
/// address twice), so the plan carries [`Precondition::None`] — there is no
/// prior object, ETag, or `last_updated` to bind. A dry-run surfaces the
/// *currently* next address as an advisory warning: NetBox allocates at apply
/// time, so the applied address may differ. Only `description` / `dns_name`
/// may be set (the v1 narrow allow-list — no status/role/tags/assignment).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn plan_ip_range_reserve(
    client: &NetBoxClient,
    range_ref: &str,
    description: Option<&str>,
    dns_name: Option<&str>,
    changelog_message: Option<&str>,
    profile: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<MutationPlan> {
    // Message length is pure input validation — check first, before any network.
    let message = match changelog_message {
        Some(m) if !m.is_empty() => Some(m.to_string()),
        _ => None,
    };
    mutation::validate_changelog_message(&message)?;

    // Resolve the IP range by start address or ID (same resolver as
    // `nbox ip-range`).
    let range = client
        .ip_range_by_ref(range_ref)
        .await?
        .ok_or_else(|| not_found("IP range", range_ref))?;
    let endpoint = format!("/api/ipam/ip-ranges/{}/available-ips/", range.id);

    let target = PlanTarget {
        kind: "ip".to_string(),
        r#ref: range_ref.to_string(),
        id: range.id,
        display: format!("{} – {}", range.start_address, range.end_address),
        endpoint,
        profile: profile.to_string(),
    };

    // The minimal POST body: only the fields the operator actually set. A bare
    // reserve sends `{}` and takes NetBox's defaults.
    let mut body = serde_json::Map::new();
    let mut fields = Vec::new();
    if let Some(d) = description.filter(|s| !s.is_empty()) {
        body.insert("description".to_string(), serde_json::json!(d));
        fields.push(FieldChange {
            field: "description".to_string(),
            before: Value::Null,
            after: serde_json::json!(d),
        });
    }
    if let Some(d) = dns_name.filter(|s| !s.is_empty()) {
        body.insert("dns_name".to_string(), serde_json::json!(d));
        fields.push(FieldChange {
            field: "dns_name".to_string(),
            before: Value::Null,
            after: serde_json::json!(d),
        });
    }
    let patch = Value::Object(body);
    let precondition = Precondition::None;

    // Dry-run advisory: the address NetBox would currently hand out. Read-only,
    // and never part of the body — another client could allocate between this
    // read and the apply POST, so the applied address may differ.
    let mut warnings = Vec::new();
    // NetBox's IP-range available-ips endpoint returns a bare JSON array.
    match client
        .get::<Vec<AvailableIp>>(
            &format!("/api/ipam/ip-ranges/{}/available-ips/", range.id),
            &[("limit", "1".to_string())],
        )
        .await
    {
        Ok(list) => match list.first() {
            Some(next) => warnings.push(format!(
                "currently next: {} — NetBox allocates at apply; the applied address may differ",
                next.address
            )),
            None => warnings.push(
                "no available addresses in this range — the reserve will fail at apply".to_string(),
            ),
        },
        // A failed advisory read must not block planning; the apply POST is the
        // authoritative attempt. Note it and carry on.
        Err(_) => {
            warnings.push("could not read the next available address (advisory only)".to_string());
        }
    }

    let expires_epoch = mutation::plan_expiry_epoch(SystemTime::now());
    let confirm_token = mutation::confirm_token(
        &target,
        Operation::Allocate,
        &precondition,
        &patch,
        &message,
        expires_epoch,
    );

    Ok(MutationPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        operation: Operation::Allocate,
        target,
        precondition,
        fields,
        patch,
        no_op: false,
        warnings,
        errors: Vec::new(),
        changelog_message: message,
        confirm_token,
        expires_at: mutation::format_iso_utc(expires_epoch),
    })
}

/// Apply a planned IP-range reservation (ADR-0001 §5 steps 5–7): verify the
/// plan's confirmation token + expiry, then POST the minimal body to the
/// range's `available-ips` endpoint. NetBox allocates the next free address
/// and returns the created IP object (`201`), which becomes the receipt's
/// `object`.
pub(crate) async fn apply_ip_range_reserve(
    client: &NetBoxClient,
    plan: &MutationPlan,
) -> Result<MutationReceipt> {
    plan.verify()?;

    // The wire body is the minimal create patch plus the opt-in changelog_message.
    let mut body = plan.patch.clone();
    if let Some(msg) = &plan.changelog_message {
        body["changelog_message"] = serde_json::json!(msg);
    }

    let (created, status): (IpAddress, u16) = client.post(&plan.target.endpoint, &body).await?;
    let address = created.address.clone();
    let view = IpView::build(created, None);
    let object = serde_json::to_value(&view).ok();

    Ok(MutationReceipt {
        schema_version: PLAN_SCHEMA_VERSION,
        operation: plan.operation,
        target: plan.target.clone(),
        fields: plan.fields.clone(),
        applied: true,
        no_op: false,
        status,
        etag: None,
        request_id: None,
        object,
        message: format!("reserved: {} in {}", address, plan.target.display),
    })
}

// ===== Safe write follow-on: tag add/remove (ADR-0001) ===================
//
// Tag writes reuse the same planner/diff/confirm/concurrency/audit contracts
// as the interface/device pilots. Tags are a list field: the plan carries the
// full replacement `{"tags": [slugs]}` (NetBox's PATCH semantics replace the
// whole array), so the before/after diff shows the tag slugs. A no-op (tag
// already present for add, or already absent for remove) sends no PATCH.

/// The writable field on any object for tag writes (ADR-0001 §6).
pub(crate) const TAG_WRITABLE_FIELD: &str = "tags";

/// Whether a tag write adds or removes the tag from the target object's tags
/// array. The planner uses this to compute the replacement slug list and the
/// no-op condition (add: no-op if already present; remove: no-op if already
/// absent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagOperation {
    Add,
    Remove,
}

/// One entry in the object's current tags array, as NetBox serializes it on a
/// detail response: `{id, name, slug}`. Used to read the current tag set and
/// build the replacement slug list.
#[derive(Debug, Deserialize)]
struct ObjectTag {
    #[serde(default)]
    #[allow(dead_code)]
    id: Option<u64>,
    #[allow(dead_code)]
    name: Option<String>,
    slug: Option<String>,
}

/// The raw object detail as a `Value`, so the planner can read the `tags`
/// array, `display`, and `last_updated` from any object kind in one path —
/// every NetBox object carries the same `tags` array shape, so no per-kind
/// model is needed for this write.
#[derive(Debug, Deserialize)]
struct TagTargetObject {
    #[serde(default)]
    display: Option<String>,
    #[serde(default)]
    last_updated: Option<String>,
    #[serde(default)]
    tags: Vec<ObjectTag>,
}

/// Resolve a `<kind> <ref>` to the object's REST detail endpoint path (relative
/// to the client base URL) and numeric id. Reuses the same per-kind resolvers
/// as `resolve_object_url`, except for IP addresses: `open ip/<addr>` historically
/// first-picked a candidate, but a write must fail closed when the same address
/// exists in multiple VRFs. Returns the endpoint path (e.g.
/// `/api/dcim/devices/42/`) and the id.
pub(crate) async fn resolve_tag_target_endpoint(
    client: &NetBoxClient,
    kind: &str,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<(String, u64)> {
    let url = match kind {
        "ip" | "ip-address" | "address" => {
            let candidates = client.ip_candidates(value).await?;
            resolve_unique(
                "IP address",
                value,
                candidates,
                query::ip_scope_label,
                not_found,
            )?
            .url
        }
        _ => crate::resolve_object_url(client, kind, value)
            .await?
            .ok_or_else(|| not_found(kind, value))?,
    };
    let parsed = reqwest::Url::parse(&url).context("parsing resolved object URL")?;
    // The object URL is the full REST detail endpoint (e.g.
    // `http://h/netbox/api/dcim/devices/42/`). Strip the client's base URL to
    // get the relative path the PATCH expects. `make_relative` strips scheme +
    // host + the base path; on a plain `http://h/` base it leaves
    // `/api/dcim/devices/42/`.
    let relative = client
        .base_url()
        .make_relative(&parsed)
        .context("stripping base URL from resolved object URL")?;
    // The relative path may carry a leading `./` when the base has a subpath;
    // normalize to the `/api/...` form the client's `url_for` expects.
    let endpoint = relative
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string();
    // Ensure a leading slash so the endpoint matches the canonical `/api/...`
    // form every other planner emits (e.g. `/api/dcim/devices/42/`).
    let endpoint = if endpoint.starts_with('/') {
        endpoint
    } else {
        format!("/{endpoint}")
    };
    // The id is the last path segment before the trailing slash.
    let id = parsed
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .context("extracting object id from resolved URL")?;
    Ok((endpoint, id))
}

/// Plan a tag write (ADR-0001 §5 steps 1–4): resolve the tag, resolve the
/// target object, read its current tags with the `ETag` (4.6+) or
/// `last_updated` (pre-4.6 fallback), and build an `Update` plan whose patch
/// replaces the `tags` array with the computed slug list. A no-op (tag already
/// present for add, or already absent for remove) produces an empty patch.
/// `changelog_message` is validated against NetBox's length limit here, before
/// any network write.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn plan_tag_update(
    client: &NetBoxClient,
    operation: TagOperation,
    object_type: &str,
    object_name: &str,
    tag_ref: &str,
    changelog_message: Option<&str>,
    profile: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<MutationPlan> {
    // Message length is pure input validation — check first, before any network.
    let message = match changelog_message {
        Some(m) if !m.is_empty() => Some(m.to_string()),
        _ => None,
    };
    mutation::validate_changelog_message(&message)?;

    // Resolve the tag by id, exact name, or exact slug (reuse the `nbox tagged`
    // resolver). A missing tag is a not-found (exit 4), not a usage error.
    let tag = client
        .tag_by_ref(tag_ref)
        .await?
        .ok_or_else(|| not_found("tag", tag_ref))?;

    // Resolve the target object to its detail endpoint + id.
    let (endpoint, id) =
        resolve_tag_target_endpoint(client, object_type, object_name, not_found).await?;

    // Read the authoritative current state with its `ETag` header (4.6+) or
    // `last_updated` (pre-4.6 fallback). Reading as a raw value gives us the
    // `tags` array, `display`, and `last_updated` in one fetch for any kind.
    let (current, etag): (TagTargetObject, Option<String>) =
        client.get_with_etag(&endpoint, &[]).await?;

    let display = current
        .display
        .clone()
        .unwrap_or_else(|| object_name.to_string());
    let target = PlanTarget {
        kind: object_type.to_string(),
        r#ref: object_name.to_string(),
        id,
        display,
        endpoint,
        profile: profile.to_string(),
    };

    let precondition = match etag {
        Some(e) => Precondition::Etag { etag: e },
        None => Precondition::LastUpdated {
            last_updated: current.last_updated.clone(),
            before_hash: tags_before_hash(&current.tags),
        },
    };

    // Compute the replacement slug list. NetBox PATCH replaces the whole
    // `tags` array, so the patch carries the full replacement list. A no-op
    // (tag already present for add, or already absent for remove) produces an
    // empty patch (no PATCH).
    let current_slugs: Vec<String> = current.tags.iter().filter_map(|t| t.slug.clone()).collect();
    let tag_present = current_slugs.iter().any(|s| s == &tag.slug);
    let no_op = match operation {
        TagOperation::Add => tag_present,
        TagOperation::Remove => !tag_present,
    };
    let after_slugs: Vec<String> = if no_op {
        current_slugs.clone()
    } else {
        match operation {
            TagOperation::Add => {
                let mut v = current_slugs.clone();
                v.push(tag.slug.clone());
                v
            }
            TagOperation::Remove => current_slugs
                .iter()
                .filter(|s| *s != &tag.slug)
                .cloned()
                .collect(),
        }
    };

    let before = serde_json::to_value(
        current
            .tags
            .iter()
            .filter_map(|t| t.slug.clone())
            .collect::<Vec<_>>(),
    )
    .unwrap_or(Value::Null);
    let after = serde_json::to_value(&after_slugs).unwrap_or(Value::Null);
    let patch = if no_op {
        serde_json::json!({})
    } else {
        serde_json::json!({ TAG_WRITABLE_FIELD: after_slugs })
    };
    let fields = vec![FieldChange {
        field: TAG_WRITABLE_FIELD.to_string(),
        before: before.clone(),
        after: after.clone(),
    }];

    let expires_epoch = mutation::plan_expiry_epoch(SystemTime::now());
    let confirm_token = mutation::confirm_token(
        &target,
        Operation::Update,
        &precondition,
        &patch,
        &message,
        expires_epoch,
    );

    Ok(MutationPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        operation: Operation::Update,
        target,
        precondition,
        fields,
        patch,
        no_op,
        warnings: Vec::new(),
        errors: Vec::new(),
        changelog_message: message,
        confirm_token,
        expires_at: mutation::format_iso_utc(expires_epoch),
    })
}

/// Apply a planned tag write (ADR-0001 §5 steps 5–7): verify the plan's
/// confirmation token + expiry, short-circuit a no-op, then send the minimal
/// `PATCH` (`{"tags": [slugs]}`) with the recorded precondition. Returns a
/// [`MutationReceipt`] from the PATCH response. Shared by add and remove.
pub(crate) async fn apply_tag_update(
    client: &NetBoxClient,
    plan: &MutationPlan,
) -> Result<MutationReceipt> {
    plan.verify()?;

    if plan.no_op {
        return Ok(no_op_receipt(plan));
    }

    // The wire body is the tags replacement plus the opt-in changelog_message.
    let mut body = plan.patch.clone();
    if let Some(msg) = &plan.changelog_message {
        body["changelog_message"] = serde_json::json!(msg);
    }

    let (_updated, new_etag, status): (Value, Option<String>, u16) = match &plan.precondition {
        Precondition::Etag { etag } => {
            client
                .patch(&plan.target.endpoint, &body, Some(etag))
                .await?
        }
        Precondition::LastUpdated {
            last_updated,
            before_hash,
        } => {
            // Read-before-write (pre-4.6 fallback): re-fetch and refuse if
            // the object moved. `last_updated` ticking is the primary
            // signal; the before-hash is a belt-and-suspenders guard.
            let (current, _): (TagTargetObject, Option<String>) =
                client.get_with_etag(&plan.target.endpoint, &[]).await?;
            if current.last_updated != *last_updated
                || tags_before_hash(&current.tags) != *before_hash
            {
                return Err(NboxError::StalePrecondition(String::new()).into());
            }
            client.patch(&plan.target.endpoint, &body, None).await?
        }
        Precondition::None => {
            anyhow::bail!("internal error: a tag plan carried a None precondition")
        }
    };

    Ok(MutationReceipt {
        schema_version: PLAN_SCHEMA_VERSION,
        operation: plan.operation,
        target: plan.target.clone(),
        fields: plan.fields.clone(),
        applied: true,
        no_op: false,
        status,
        etag: new_etag,
        request_id: None,
        object: None,
        message: format!(
            "applied: {} {} ({})",
            plan.target.kind,
            plan.target.display,
            plan.changed_field_names().join(", ")
        ),
    })
}

/// Normalized before-hash over the object's current tags (sorted slugs). The
/// apply re-reads and recomputes this; a mismatch vs the plan's recorded
/// `before_hash` means the object's tags changed in NetBox.
fn tags_before_hash(tags: &[ObjectTag]) -> String {
    let mut slugs: BTreeMap<String, Value> = BTreeMap::new();
    for t in tags {
        if let Some(slug) = &t.slug {
            slugs.insert(slug.clone(), Value::String(slug.clone()));
        }
    }
    mutation::before_hash(&slugs)
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
/// child prefixes and member IPs. Shared by CLI/MCP.
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
    let (children, ips) = prefix_children_and_ips(client, cidr, vrf_id).await?;
    Ok(PrefixView::build(prefix, children, ips))
}

async fn prefix_children_and_ips(
    client: &NetBoxClient,
    cidr: &str,
    vrf_id: Option<u64>,
) -> Result<(Vec<Prefix>, Vec<IpAddress>)> {
    // Children and contained IPs are independent; fetch them concurrently so
    // prefix detail costs one round-trip for the header plus one for both child
    // collections, not two sequential awaits. Only contained IPs get the higher
    // cap; child prefixes keep the shared detail-section budget.
    tokio::try_join!(
        client.prefix_children(cidr, vrf_id, DETAIL_SECTION_CAP),
        client.prefix_ips(cidr, vrf_id, PREFIX_CONTAINED_IP_CAP),
    )
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
/// prefixes that reference it (cap [`DETAIL_SECTION_CAP`]). Shared by CLI/MCP.
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
    let (prefixes, group) = tokio::try_join!(
        client.vlan_prefixes(vlan.id, DETAIL_SECTION_CAP),
        vlan_group_scope(client, &vlan),
    )?;
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

/// `rack-group <slug|name|id>`: resolve a rack group and build its view. Shared by
/// CLI/MCP.
pub async fn rack_group_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<RackGroupView> {
    let rg = client
        .rack_group_by_ref(value)
        .await?
        .ok_or_else(|| not_found("rack group", value))?;
    Ok(RackGroupView::from_model(rg))
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
    let terminations = client.circuit_terminations(circuit.id).await?;
    let resolved = resolve_terminations(client, terminations).await;
    Ok(CircuitView::build(circuit, resolved))
}

/// Resolve every termination's cable path, walking the A and Z sides concurrently
/// (each side's hops are sequential — a hop depends on the previous — but the two
/// sides are independent).
async fn resolve_terminations(
    client: &NetBoxClient,
    terminations: Vec<CircuitTermination>,
) -> Vec<ResolvedTermination> {
    let walks = terminations.into_iter().map(|t| async move {
        let path = resolve_termination_path(client, &t).await;
        ResolvedTermination {
            termination: t,
            path,
        }
    });
    futures::future::join_all(walks).await
}

/// How far to walk a termination's cable chain before giving up (guards against a
/// mis-modeled loop; real circuit→panel→device paths are 1–2 hops).
const CIRCUIT_PATH_CAP: usize = 5;

/// The kind of a cabled object, inferred from its API URL.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PortKind {
    Interface,
    FrontPort,
    RearPort,
    Other,
}

fn port_kind(url: Option<&str>) -> PortKind {
    match url {
        Some(u) if u.contains("/interfaces/") => PortKind::Interface,
        Some(u) if u.contains("/front-ports/") => PortKind::FrontPort,
        Some(u) if u.contains("/rear-ports/") => PortKind::RearPort,
        _ => PortKind::Other,
    }
}

/// The next stop reached by crossing a cable: where it lands and over which cable.
struct NextHop {
    url: Option<String>,
    to: String,
    device: Option<DeviceRef>,
    id: u64,
    kind: PortKind,
    cable: Option<String>,
}

/// Walk a circuit termination's cable chain: from its immediate link peer, through
/// any patch panels (rear↔front), to the device interface it lands on. Returns the
/// hops **device-first** (the resolved endpoint leads; the circuit-adjacent panel
/// is last). Stops at an interface (the resolved endpoint), a dead-end (e.g. an
/// unwired panel — common), the hop cap, or a cycle. Best-effort and tolerant of
/// the polymorphic JSON: a fetch error simply ends the chain where it is.
async fn resolve_termination_path(client: &NetBoxClient, t: &CircuitTermination) -> Vec<PathHop> {
    let mut hops = Vec::new();
    let Some(peer) = t.link_peers.first() else {
        return hops;
    };
    let mut cur = NextHop {
        url: peer.url.clone(),
        to: peer.endpoint_label(),
        device: peer.device.as_deref().map(DeviceRef::from_brief),
        id: peer.id,
        kind: port_kind(peer.url.as_deref()),
        // The first cable crossed is the termination's own.
        cable: t.cable.as_ref().map(BriefObject::label),
    };
    let mut seen = std::collections::HashSet::new();
    for _ in 0..CIRCUIT_PATH_CAP {
        let endpoint = cur.kind == PortKind::Interface;
        hops.push(PathHop {
            to: cur.to.clone(),
            cable: cur.cable.clone(),
            endpoint,
            device: cur.device.clone(),
        });
        if endpoint {
            break;
        }
        if let Some(u) = &cur.url
            && !seen.insert(u.clone())
        {
            break; // cycle guard
        }
        match panel_onward(client, &cur).await {
            Some(next) => cur = next,
            None => break, // dead-end (unwired panel) or unsupported object
        }
    }
    // Built from the circuit outward; present device-first (the resolved endpoint
    // leads, the circuit-adjacent panel comes last) so the path reads from the
    // equipment toward the circuit.
    hops.reverse();
    hops
}

/// From a panel face, cross to its opposite face and follow that face's cable to
/// the next stop. `None` when the panel isn't internally wired (no rear↔front
/// mapping) or the opposite face isn't cabled onward.
async fn panel_onward(client: &NetBoxClient, cur: &NextHop) -> Option<NextHop> {
    match cur.kind {
        PortKind::RearPort => front_for_rear(client, cur.device.as_ref()?.id, cur.id).await,
        PortKind::FrontPort => rear_for_front(client, cur.id).await,
        _ => None,
    }
}

/// The rear-port ids a front-port maps to. Tolerates both the standard singular
/// `rear_port` field and the `rear_ports` array form some instances serialize
/// (`[{rear_port: <id>, rear_port_position, position}]`), where `rear_port` is a
/// bare id (or, defensively, a nested `{id}`).
fn front_rear_ids(fp: &serde_json::Value) -> Vec<u64> {
    let mut ids = Vec::new();
    if let Some(id) = fp
        .get("rear_port")
        .and_then(|r| r.get("id"))
        .and_then(serde_json::Value::as_u64)
    {
        ids.push(id);
    }
    if let Some(arr) = fp.get("rear_ports").and_then(serde_json::Value::as_array) {
        for e in arr {
            let rp = e.get("rear_port");
            if let Some(id) = rp.and_then(serde_json::Value::as_u64) {
                ids.push(id);
            } else if let Some(id) = rp
                .and_then(|r| r.get("id"))
                .and_then(serde_json::Value::as_u64)
            {
                ids.push(id);
            }
        }
    }
    ids
}

/// How many of a panel device's front-ports to page through looking for the
/// rear↔front mapping. A high-density panel can have hundreds; cap it so a
/// mis-modeled device can't make the walk unbounded.
const PANEL_FRONT_PORT_CAP: usize = 1000;

/// Rear → front: find the panel's front-port mapped to this rear-port, then the
/// stop across that front-port's cable. Pages through the device's front-ports
/// (a big panel exceeds one page) so the mapping isn't missed past the first page.
async fn front_for_rear(client: &NetBoxClient, device_id: u64, rear_id: u64) -> Option<NextHop> {
    let fronts: Vec<serde_json::Value> = client
        .list_all(
            Endpoint::FrontPorts,
            vec![("device_id", device_id.to_string())],
            PANEL_FRONT_PORT_CAP,
        )
        .await
        .ok()?;
    // Takes the first front-port that maps to this rear-port. Correct for the
    // common single-position panel; a multi-position rear (an MPO trunk) is
    // referenced by several front-ports at different positions, so a position-aware
    // match would be a refinement (see ROADMAP).
    for fp in &fronts {
        if !front_rear_ids(fp).contains(&rear_id) {
            continue;
        }
        let cable = fp.get("cable").and_then(cable_label_value);
        if let Some(peer) = fp
            .get("link_peers")
            .and_then(serde_json::Value::as_array)
            .and_then(|a| a.first())
        {
            return next_hop_from_peer(peer, cable);
        }
    }
    None
}

/// Front → rear: read the front-port's paired rear-port, then the stop across the
/// rear-port's cable.
async fn rear_for_front(client: &NetBoxClient, front_id: u64) -> Option<NextHop> {
    let fp: serde_json::Value = client
        .get(&format!("/api/dcim/front-ports/{front_id}/"), &[])
        .await
        .ok()?;
    let rear_id = front_rear_ids(&fp).into_iter().next()?;
    let rp: serde_json::Value = client
        .get(&format!("/api/dcim/rear-ports/{rear_id}/"), &[])
        .await
        .ok()?;
    let cable = rp.get("cable").and_then(cable_label_value);
    let peer = rp
        .get("link_peers")
        .and_then(serde_json::Value::as_array)
        .and_then(|a| a.first())?;
    next_hop_from_peer(peer, cable)
}

/// Build a [`NextHop`] from a link-peer JSON object (a port) and the cable crossed.
fn next_hop_from_peer(peer: &serde_json::Value, cable: Option<String>) -> Option<NextHop> {
    let id = peer.get("id").and_then(serde_json::Value::as_u64)?;
    let url = peer.get("url").and_then(|v| v.as_str()).map(str::to_string);
    let port = peer
        .get("display")
        .or_else(|| peer.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let device = peer.get("device").and_then(device_ref_from_value);
    let to = match &device {
        Some(d) => format!("{} {port}", d.name),
        None => port,
    };
    Some(NextHop {
        kind: port_kind(url.as_deref()),
        url,
        to,
        device,
        id,
        cable,
    })
}

/// A [`DeviceRef`] from a port's nested `device` JSON object (id + bare name).
fn device_ref_from_value(d: &serde_json::Value) -> Option<DeviceRef> {
    let id = d.get("id").and_then(serde_json::Value::as_u64)?;
    let name = d
        .get("name")
        .or_else(|| d.get("display"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some(DeviceRef { id, name })
}

/// A cable's label from a JSON object: its display/label, else NetBox's `#<id>`.
fn cable_label_value(v: &serde_json::Value) -> Option<String> {
    v.get("display")
        .or_else(|| v.get("label"))
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            v.get("id")
                .and_then(serde_json::Value::as_u64)
                .map(|id| format!("#{id}"))
        })
}

/// Navigable links for a circuit + its resolved terminations: the provider/tenant,
/// each side's site endpoint, and the devices its path traverses (panel + the
/// device it lands on). Provider networks have no detail kind, so they're skipped.
fn circuit_links(c: &Circuit, terminations: &[ResolvedTermination]) -> Vec<ObjectLink> {
    let mut l = Vec::new();
    push_link(
        &mut l,
        "provider",
        ObjectKind::Provider,
        c.provider.as_ref(),
    );
    push_link(&mut l, "tenant", ObjectKind::Tenant, c.tenant.as_ref());
    for rt in terminations {
        let side = rt.termination.term_side.as_deref().unwrap_or("?");
        if rt.termination.termination_type.as_deref() == Some("dcim.site")
            && let Some(site) = &rt.termination.termination
        {
            l.push(ObjectLink {
                kind: ObjectKind::Site,
                id: site.id,
                relation: format!("{side}-side site"),
                label: site.label(),
            });
        }
        for hop in &rt.path {
            if let Some(dev) = &hop.device
                && !l
                    .iter()
                    .any(|x| x.kind == ObjectKind::Device && x.id == dev.id)
            {
                l.push(ObjectLink {
                    kind: ObjectKind::Device,
                    id: dev.id,
                    relation: format!("{side}-side device"),
                    label: dev.name.clone(),
                });
            }
        }
    }
    l
}

/// Build a circuit's [`DetailView`] (TUI): its attributes as the body, the A↔Z
/// path as a dedicated scrollable tab, and navigable links to the provider, sites,
/// and patched devices. Shared by `load_detail` (by id) and `load_detail_by_ref`.
async fn load_circuit_detail_view(client: &NetBoxClient, circuit: Circuit) -> Result<DetailView> {
    let id = circuit.id;
    let terminations = client.circuit_terminations(id).await?;
    let resolved = resolve_terminations(client, terminations).await;
    let links = circuit_links(&circuit, &resolved);
    let view = CircuitView::build(circuit, resolved);
    let title = format!("circuit {}", view.cid);
    let mut tabs = Vec::new();
    if !view.diagram.is_empty() {
        tabs.push(DetailTab {
            key: 'p',
            label: "path".to_string(),
            body: view.diagram.join("\n"),
            rows: Vec::new(),
        });
    }
    Ok(DetailView::new(
        ObjectKind::Circuit,
        id,
        title,
        view.to_key_values().render(),
    )
    .with_tabs(tabs)
    .with_links(links))
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

/// `mac <addr>`: reverse-resolve a MAC to its assignment. MACs aren't enforced
/// globally unique (the same MAC can appear on several interfaces), so >1
/// candidate surfaces as `Ambiguous` (exit 5) with the candidate list — the
/// caller normalizes the MAC before passing it in. Shared by CLI/MCP.
pub async fn mac_view_by_ref(
    client: &NetBoxClient,
    mac: &str,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<MacView> {
    Ok(MacView::from_model(
        resolve_mac(client, mac, value, not_found).await?,
    ))
}

/// Resolve a (normalized) MAC to a single [`MacAddress`]. MACs aren't enforced
/// globally unique, so >1 candidate is `Ambiguous` (exit 5) and 0 is not-found
/// (exit 4) — never a silent first-pick. Shared by `nbox mac`, `nbox open
/// mac/<addr>`, and `nbox journal mac <addr>` so all three honor the same
/// exit-code contract.
pub(crate) async fn resolve_mac(
    client: &NetBoxClient,
    mac: &str,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<MacAddress> {
    let candidates = client.mac_candidates(mac).await?;
    resolve_unique("MAC", value, candidates, label_mac, not_found)
}

/// A MAC candidate's one-line label for the ambiguous-match list: the MAC plus
/// its assignment (e.g. `aa:…:ff → edge01 xe-0/0/1`), so an ambiguous MAC names
/// the competing interfaces.
fn label_mac(m: &MacAddress) -> String {
    let assigned = m
        .assigned_object
        .as_ref()
        .and_then(crate::domain::mac_view::assigned_label)
        .unwrap_or_else(|| "unassigned".to_string());
    format!("{} → {}", m.mac_address, assigned)
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

/// `virtual-circuit <cid|id>`: resolve a virtual circuit, fetch its terminations,
/// and build the view. Shared by CLI/MCP.
pub async fn virtual_circuit_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<VirtualCircuitView> {
    let vc = client
        .virtual_circuit_by_ref(value)
        .await?
        .ok_or_else(|| not_found("virtual circuit", value))?;
    let terminations = client.virtual_circuit_terminations(vc.id).await?;
    Ok(VirtualCircuitView::build(vc, terminations))
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

/// `vm-type <slug|name|id>`: resolve a virtual machine type and build its view.
/// Shared by CLI/MCP.
pub async fn vm_type_view_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<VirtualMachineTypeView> {
    let t = client
        .vm_type_by_ref(value)
        .await?
        .ok_or_else(|| not_found("virtual machine type", value))?;
    Ok(VirtualMachineTypeView::from_model(t))
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

/// One navigable row within a detail section: the display `text` and, when the
/// row addresses an openable object, the `target` that `Enter` jumps to (the same
/// `LoadDetail` jump the `R` modal uses). Rows with no target (headings, footers,
/// "(none)" placeholders) still render but aren't selectable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetailRow {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<(ObjectKind, u64)>,
}

impl DetailRow {
    /// A selectable row that opens `kind`/`id` on `Enter`.
    pub fn link(text: String, kind: ObjectKind, id: u64) -> Self {
        Self {
            text,
            target: Some((kind, id)),
        }
    }

    /// A plain, non-selectable row (heading, footer, placeholder).
    pub fn plain(text: String) -> Self {
        Self { text, target: None }
    }
}

/// A switchable section on a detail screen (e.g. a device's interfaces). A
/// section is rendered as scrollable text from `body`, unless `rows` is non-empty,
/// in which case it's an interactive list (`j`/`k` move, `Enter` opens the row's
/// target) — `body` is then the same content flattened to text for plain output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetailTab {
    pub key: char,
    pub label: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<DetailRow>,
}

/// A navigable reference from one detail object to a related one — the data
/// behind the TUI's `R` "related objects" jump list. `kind` + `id` address the
/// target (drives a `LoadDetail`); `relation` names the edge ("site", "vlan", …);
/// `label` is the target's display name. Only relations whose target has a detail
/// view are emitted (e.g. a VRF/rack/role has no detail kind, so it's skipped).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectLink {
    pub kind: ObjectKind,
    pub id: u64,
    pub relation: String,
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
            relation: relation.to_string(),
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
            relation: "parent prefix".to_string(),
            label: pp.prefix.clone(),
        });
    }
    push_link(&mut l, "tenant", ObjectKind::Tenant, ip.tenant.as_ref());
    l
}

/// A rendered detail screen: the object's identity, a title, the summary body,
/// any switchable tabs (empty for objects without sub-resources), and the
/// navigable links to related objects (the `R` jump list; empty when none).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetailView {
    pub kind: ObjectKind,
    pub id: u64,
    pub title: String,
    pub body: String,
    pub tabs: Vec<DetailTab>,
    pub links: Vec<ObjectLink>,
    /// A persistent header card rendered above the tab bar (fixed, not scrolled).
    /// Empty for objects that don't use one — they render exactly as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header: Vec<String>,
    /// Label for the summary slot (`detail_tab == 0`) in the tab bar. Empty means
    /// the default "summary"; a routing-context view sets e.g. "prefixes·12".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary_label: String,
    /// Navigable rows for the summary slot. When non-empty the summary renders as
    /// an interactive list (like a [`DetailTab`] with rows) instead of `body` text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary_rows: Vec<DetailRow>,
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
            header: Vec::new(),
            summary_label: String::new(),
            summary_rows: Vec::new(),
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

    fn with_header(mut self, header: Vec<String>) -> Self {
        self.header = header;
        self
    }

    /// Set the summary slot's tab label and its navigable rows in one call.
    fn with_summary(mut self, label: String, rows: Vec<DetailRow>) -> Self {
        self.summary_label = label;
        self.summary_rows = rows;
        self
    }
}

/// Navigable rows for a device's assigned IP addresses — each opens that IP on
/// Enter. The interface trails the address, matching the plain `ip_lines` body.
fn device_ip_rows(ips: &[IpRow]) -> Vec<DetailRow> {
    ips.iter()
        .map(|ip| {
            let text = match &ip.interface {
                Some(name) => format!("{}  {name}", ip.address),
                None => ip.address.clone(),
            };
            DetailRow::link(text, ObjectKind::IpAddress, ip.id)
        })
        .collect()
}

/// Navigable rows for the VLANs seen on a device's interfaces — each opens that
/// VLAN on Enter.
fn device_vlan_rows(vlans: &[VlanRow]) -> Vec<DetailRow> {
    vlans
        .iter()
        .map(|v| DetailRow::link(v.vlan.clone(), ObjectKind::Vlan, v.id))
        .collect()
}

/// Navigable rows for a device's interfaces — each opens that interface's detail
/// (its attributes + cable-path trace) on Enter. Text mirrors the `iface_lines`
/// body (name, type, a `(disabled)` marker) minus the leading indent the list
/// renderer supplies.
fn device_interface_rows(ifaces: &[IfaceRow]) -> Vec<DetailRow> {
    use std::fmt::Write as _;
    ifaces
        .iter()
        .map(|i| {
            let mut text = i.name.clone();
            if let Some(t) = &i.type_ {
                let _ = write!(text, "  {t}");
            }
            if i.enabled == Some(false) {
                text.push_str("  (disabled)");
            }
            DetailRow::link(text, ObjectKind::Interface, i.id)
        })
        .collect()
}

/// Navigable rows for a device's cabled interfaces — each opens the *local*
/// interface's detail (where the full cable-path trace lives) on Enter. Text
/// mirrors the `cable_lines` body minus the leading indent.
fn device_cable_rows(cables: &[CableRow]) -> Vec<DetailRow> {
    cables
        .iter()
        .map(|c| {
            let text = if c.connected_to.is_empty() {
                format!("{}  {}", c.interface, c.cable.as_deref().unwrap_or(""))
            } else {
                format!("{} → {}", c.interface, c.connected_to.join(", "))
            };
            DetailRow::link(text, ObjectKind::Interface, c.id)
        })
        .collect()
}

/// Navigable rows for a list of devices — each opens that device on Enter. Used by
/// the site and rack `devices` tabs.
fn device_link_rows(devices: &[Device]) -> Vec<DetailRow> {
    if devices.is_empty() {
        return vec![DetailRow::plain("(no devices)".to_string())];
    }
    devices
        .iter()
        .map(|d| DetailRow::link(d.name.clone(), ObjectKind::Device, d.id))
        .collect()
}

/// Navigable rows for a list of racks — each opens that rack on Enter. Used by the
/// site `racks` tab.
fn rack_link_rows(racks: &[Rack]) -> Vec<DetailRow> {
    if racks.is_empty() {
        return vec![DetailRow::plain("(no racks)".to_string())];
    }
    racks
        .iter()
        .map(|r| DetailRow::link(r.name.clone(), ObjectKind::Rack, r.id))
        .collect()
}

/// A best-effort navigable `devices` tab: the devices scoped by `param`
/// (`site_id`/`rack_id`) `= id`, each opening on Enter. A fetch error surfaces in
/// the tab body instead of failing the whole detail (the summary still loads),
/// mirroring [`rack_elevation_tab`].
async fn contained_devices_tab(client: &NetBoxClient, param: &'static str, id: u64) -> DetailTab {
    match client
        .list_all::<Device>(
            Endpoint::Devices,
            vec![(param, id.to_string())],
            DETAIL_SECTION_CAP,
        )
        .await
    {
        Ok(devices) => {
            let rows = device_link_rows(&devices);
            DetailTab {
                key: 'd',
                label: format!("devices·{}", devices.len()),
                body: rows_to_text(&rows),
                rows,
            }
        }
        Err(e) => DetailTab {
            key: 'd',
            label: "devices".to_string(),
            body: format!("(devices unavailable: {e:#})"),
            rows: Vec::new(),
        },
    }
}

/// A best-effort navigable `racks` tab for a site: the racks in the site, each
/// opening on Enter. A fetch error surfaces in the tab body (the summary loads).
async fn site_racks_tab(client: &NetBoxClient, site_id: u64) -> DetailTab {
    match client
        .list_all::<Rack>(
            Endpoint::Racks,
            vec![("site_id", site_id.to_string())],
            DETAIL_SECTION_CAP,
        )
        .await
    {
        Ok(racks) => {
            let rows = rack_link_rows(&racks);
            DetailTab {
                key: 'r',
                label: format!("racks·{}", racks.len()),
                body: rows_to_text(&rows),
                rows,
            }
        }
        Err(e) => DetailTab {
            key: 'r',
            label: "racks".to_string(),
            body: format!("(racks unavailable: {e:#})"),
            rows: Vec::new(),
        },
    }
}

/// Build a device detail (summary body + i/p/c/v/s tabs) from its sub-resources.
/// Reuses the same fan-out + compose path as the CLI/MCP device lookup, then
/// derives the TUI's title, summary body, and per-section tabs from it. The
/// interfaces (`i`), IP (`p`), cables (`c`), and VLAN (`v`) tabs get navigable
/// rows so Enter drills in — interfaces/cables open the interface detail, IPs the
/// IP, VLANs the VLAN. Services (`s`) stay plain text (no detail to open).
async fn load_device_detail(
    client: &NetBoxClient,
    device: Device,
) -> Result<(String, String, Vec<DetailTab>)> {
    let name = device.name.clone();
    let detail = build_device_detail(client, device).await?;
    let tabs = detail
        .sections()
        .into_iter()
        .map(|(key, label, body)| {
            let rows = match key {
                'i' => device_interface_rows(&detail.interfaces),
                'p' => device_ip_rows(&detail.ip_addresses),
                'c' => device_cable_rows(&detail.cables),
                'v' => device_vlan_rows(&detail.vlans),
                _ => Vec::new(),
            };
            DetailTab {
                key,
                label: label.to_string(),
                body,
                rows,
            }
        })
        .collect();
    Ok((format!("device {name}"), detail.summary_plain(), tabs))
}

/// Load and render the detail for a search result (`kind` + `id`).
/// Build a rack's `e` (elevation) detail tab — the framed front elevation.
/// Best-effort: a fetch error surfaces in the tab body instead of failing the
/// whole rack detail (the summary still loads).
async fn rack_elevation_tab(client: &NetBoxClient, rack_id: u64, u_height: u32) -> DetailTab {
    let body =
        match crate::netbox::rack_elevation::load_rack_elevation(client, rack_id, u_height).await {
            Ok(elevation) => elevation.render(),
            Err(e) => format!("(elevation unavailable: {e:#})"),
        };
    DetailTab {
        key: 'e',
        label: "elevation".to_string(),
        body,
        rows: Vec::new(),
    }
}

/// The tenant cross-link for a VRF (the one related object with a detail view).
fn vrf_links(v: &Vrf) -> Vec<ObjectLink> {
    let mut l = Vec::new();
    push_link(&mut l, "tenant", ObjectKind::Tenant, v.tenant.as_ref());
    l
}

/// The compact VRF header card: identity + routing metadata as fixed lines (RD,
/// tenant, route-target counts, enforce-unique; then the description). The full
/// route targets live in the `targets` tab, not here. Rendered above the tab bar.
fn vrf_header_lines(v: &VrfView) -> Vec<String> {
    let mut top: Vec<String> = vec![format!("RD {}", v.rd.as_deref().unwrap_or("—"))];
    if let Some(t) = &v.tenant {
        top.push(format!("Tenant {t}"));
    }
    if !v.import_targets.is_empty() || !v.export_targets.is_empty() {
        top.push(format!(
            "RT ↓{} ↑{}",
            v.import_targets.len(),
            v.export_targets.len()
        ));
    }
    if let Some(eu) = v.enforce_unique {
        top.push(format!("enforce-uniq {}", if eu { "✓" } else { "✗" }));
    }
    let mut lines = vec![top.join("   ")];
    if let Some(d) = &v.description {
        lines.push(d.clone());
    }
    lines
}

/// Navigable rows for the VRF's import/export route targets — each route target
/// opens its detail (the VRFs that import/export it), so the `targets` tab
/// navigates like the prefix/address sections.
fn vrf_target_detail_rows(v: &VrfView) -> Vec<DetailRow> {
    if v.import_targets.is_empty() && v.export_targets.is_empty() {
        return vec![DetailRow::plain("(no route targets)".to_string())];
    }
    let mut rows = vec![DetailRow::plain(format!(
        "Import ({})",
        v.import_targets.len()
    ))];
    for rt in &v.import_targets {
        rows.push(DetailRow::link(
            format!("  {}", rt.name),
            ObjectKind::RouteTarget,
            rt.id,
        ));
    }
    rows.push(DetailRow::plain(format!(
        "Export ({})",
        v.export_targets.len()
    )));
    for rt in &v.export_targets {
        rows.push(DetailRow::link(
            format!("  {}", rt.name),
            ObjectKind::RouteTarget,
            rt.id,
        ));
    }
    rows
}

/// Flatten navigable rows to a plain-text body (one row per line) — the text
/// fallback that non-interactive renderers and serialized views read.
fn rows_to_text(rows: &[DetailRow]) -> String {
    rows.iter()
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Navigable rows for a list of prefixes — each opens that prefix on Enter. Used
/// for a prefix's child prefixes and for the prefixes that reference a VLAN; the
/// `empty` placeholder names the absent case for each.
fn prefix_link_rows(prefixes: &[Prefix], empty: &str) -> Vec<DetailRow> {
    if prefixes.is_empty() {
        return vec![DetailRow::plain(empty.to_string())];
    }
    prefixes
        .iter()
        .map(|p| DetailRow::link(p.prefix.clone(), ObjectKind::Prefix, p.id))
        .collect()
}

/// Navigable rows for a prefix's contained IP addresses — each opens that IP on
/// Enter. The assignment (device/interface) trails the address, matching the
/// CLI's inline "IP Addresses" section.
fn prefix_ip_rows(ips: &[IpAddress]) -> Vec<DetailRow> {
    if ips.is_empty() {
        return vec![DetailRow::plain("(no addresses)".to_string())];
    }
    ips.iter()
        .map(|ip| {
            let text = match ip.assigned_object.as_ref().and_then(assigned_label) {
                Some(a) => format!("{}  {a}", ip.address),
                None => ip.address.clone(),
            };
            DetailRow::link(text, ObjectKind::IpAddress, ip.id)
        })
        .collect()
}

/// Assemble the prefix detail's title, header body, and navigable child/address
/// tabs. The child-prefix and contained-IP lists become navigable tabs (Enter
/// drills into that prefix/IP); the body is the header key-values only, since the
/// lists now live in the tabs rather than inline. The id-bearing fetches are
/// consumed here, then folded into the JSON [`PrefixView`] (whose serialized shape
/// — and the CLI's inline-section output — is unchanged).
fn prefix_detail_parts(
    p: Prefix,
    children: Vec<Prefix>,
    ips: Vec<IpAddress>,
) -> (String, String, Vec<DetailTab>) {
    let child_rows = prefix_link_rows(&children, "(no child prefixes)");
    let ip_rows = prefix_ip_rows(&ips);
    let tabs = vec![
        DetailTab {
            key: 'c',
            label: format!("children·{}", children.len()),
            body: rows_to_text(&child_rows),
            rows: child_rows,
        },
        DetailTab {
            key: 'a',
            label: format!("addresses·{}", ips.len()),
            body: rows_to_text(&ip_rows),
            rows: ip_rows,
        },
    ];
    let v = PrefixView::build(p, children, ips);
    (format!("prefix {}", v.prefix), v.to_detail_header(), tabs)
}

/// The navigable `prefixes` tab for a VLAN — each referencing prefix opens on Enter.
fn vlan_prefixes_tab(prefixes: &[Prefix]) -> DetailTab {
    let rows = prefix_link_rows(prefixes, "(no prefixes)");
    DetailTab {
        key: 'p',
        label: format!("prefixes·{}", prefixes.len()),
        body: rows_to_text(&rows),
        rows,
    }
}

/// Sort key for tree order: address family, network address, then prefix length —
/// reproduces NetBox's tree ordering (a container before its children) so the
/// depth-based indentation is correct regardless of which backend supplied the
/// rows.
fn prefix_sort_key(cidr: &str) -> (u8, u128, u8) {
    let (addr, len) = cidr.split_once('/').unwrap_or((cidr, ""));
    let len: u8 = len.parse().unwrap_or(0);
    match addr.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(a)) => (0, u128::from(u32::from(a)), len),
        Ok(std::net::IpAddr::V6(a)) => (1, u128::from(a), len),
        Err(_) => (2, u128::MAX, len),
    }
}

async fn rest_vrf_children(
    client: &NetBoxClient,
    vrf_id: u64,
) -> Result<(Vec<Prefix>, Vec<IpAddress>)> {
    // REST: fetch the two child collections concurrently - they're independent
    // and this halves the detail's latency on a high-RTT link.
    Ok(tokio::try_join!(
        client.list_all(
            Endpoint::Prefixes,
            vec![("vrf_id", vrf_id.to_string())],
            DETAIL_SECTION_CAP,
        ),
        client.list_all(
            Endpoint::IpAddresses,
            vec![("vrf_id", vrf_id.to_string())],
            DETAIL_SECTION_CAP,
        ),
    )?)
}

/// Build the backend-neutral [`VrfDetail`]: the VRF summary plus its scoped
/// prefixes (as a tree) and addresses. Children come from a single bundled
/// GraphQL query when the VRF surface resolves to GraphQL; unsupported schemas
/// route to REST before runtime, and runtime GraphQL bundle failures retry REST.
async fn build_vrf_detail(client: &NetBoxClient, vrf: Vrf) -> Result<VrfDetail> {
    let id = vrf.id;
    let summary = VrfView::from_model(vrf);

    let (mut prefixes, addresses): (Vec<Prefix>, Vec<IpAddress>) = if client
        .effective_backend(ApiSurface::Vrf)
        .await
        .uses_graphql()
    {
        match client.graphql_vrf_bundle(id, DETAIL_SECTION_CAP).await {
            Ok(bundle) => bundle,
            Err(err) => {
                let graphql_error = format!("{err:#}");
                tracing::warn!(
                    vrf_id = id,
                    error = %graphql_error,
                    "GraphQL VRF bundle failed; retrying REST"
                );
                rest_vrf_children(client, id).await.with_context(|| {
                    format!(
                        "GraphQL VRF bundle failed ({graphql_error}); REST fallback also failed"
                    )
                })?
            }
        }
    } else {
        rest_vrf_children(client, id).await?
    };

    let prefix_total = summary.prefix_count.unwrap_or(prefixes.len() as u64);
    let address_total = summary.ipaddress_count.unwrap_or(addresses.len() as u64);

    // Sort into tree order (container before children) so depth indentation is
    // correct regardless of backend, then derive per-node depth + container
    // utilization via the shared tree builder.
    prefixes.sort_by_key(|p| prefix_sort_key(&p.prefix));
    let prefixes = build_nodes(prefixes)
        .into_iter()
        .map(|n| VrfPrefixRow {
            id: n.id,
            prefix: n.prefix,
            depth: n.depth,
            status: n.status,
            description: n.description,
            utilization: n.utilization,
        })
        .collect();

    let addresses = addresses
        .into_iter()
        .map(|ip| VrfAddressRow {
            id: ip.id,
            address: ip.address,
            status: ip.status.map(|c| c.value),
            dns_name: ip.dns_name.filter(|s| !s.is_empty()),
        })
        .collect();

    Ok(VrfDetail {
        summary,
        prefixes,
        addresses,
        prefix_total,
        address_total,
    })
}

/// `vrf <name|rd|id>`: resolve a VRF and build its routing-context detail. Shared
/// by CLI/MCP. Identity (and not-found/ambiguous typed errors) stays REST.
pub async fn vrf_detail_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<VrfDetail> {
    let vrf = client
        .vrf_by_ref(value)
        .await?
        .ok_or_else(|| not_found("vrf", value))?;
    build_vrf_detail(client, vrf).await
}

fn route_target_links(rt: &RouteTarget) -> Vec<ObjectLink> {
    let mut l = Vec::new();
    push_link(&mut l, "tenant", ObjectKind::Tenant, rt.tenant.as_ref());
    l
}

/// Sort a route target's VRF references into NetBox's VRF model order — by
/// `(name, rd)` — so the importing/exporting lists are deterministic. The REST
/// `/api/ipam/vrfs/` list already returns name order; the GraphQL bundle sorts to
/// match. Applied to both backends so their output is byte-identical.
fn sort_vrf_refs(refs: &mut [VrfRef]) {
    refs.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.rd.cmp(&b.rd)));
}

async fn rest_route_target_relations(
    client: &NetBoxClient,
    route_target_id: u64,
) -> Result<(Vec<VrfRef>, Vec<VrfRef>)> {
    // REST: fetch the two VRF collections concurrently - they're independent and
    // this halves the detail's latency on a high-RTT link.
    let (importing, exporting): (Vec<Vrf>, Vec<Vrf>) = tokio::try_join!(
        client.list_all(
            Endpoint::Vrfs,
            vec![("import_target_id", route_target_id.to_string())],
            DETAIL_SECTION_CAP,
        ),
        client.list_all(
            Endpoint::Vrfs,
            vec![("export_target_id", route_target_id.to_string())],
            DETAIL_SECTION_CAP,
        ),
    )?;
    Ok((
        importing.iter().map(VrfRef::from_model).collect(),
        exporting.iter().map(VrfRef::from_model).collect(),
    ))
}

/// Build a route target's relation graph: the target's header plus the VRFs that
/// import and export it. A route target carries the relation on the VRF side, so
/// when the route-target surface resolves to GraphQL one filtered
/// `route_target_list` query returns both directions; otherwise canonical REST
/// fans out two `/api/ipam/vrfs/` list calls concurrently. The resulting
/// [`RouteTargetDetail`] is byte-identical between the two paths. Unsupported
/// schemas route to REST before runtime, and runtime GraphQL bundle failures
/// retry REST.
async fn build_route_target_detail(
    client: &NetBoxClient,
    rt: RouteTarget,
) -> Result<RouteTargetDetail> {
    let id = rt.id;
    let summary = RouteTargetView::from_model(rt);

    let (mut importing_vrfs, mut exporting_vrfs): (Vec<VrfRef>, Vec<VrfRef>) = if client
        .effective_backend(ApiSurface::RouteTarget)
        .await
        .uses_graphql()
    {
        match client.graphql_route_target_bundle(id).await {
            Ok(bundle) => bundle,
            Err(err) => {
                let graphql_error = format!("{err:#}");
                tracing::warn!(
                    route_target_id = id,
                    error = %graphql_error,
                    "GraphQL route-target bundle failed; retrying REST"
                );
                rest_route_target_relations(client, id)
                    .await
                    .with_context(|| {
                        format!(
                            "GraphQL route-target bundle failed ({graphql_error}); REST fallback also failed"
                        )
                    })?
            }
        }
    } else {
        rest_route_target_relations(client, id).await?
    };

    sort_vrf_refs(&mut importing_vrfs);
    sort_vrf_refs(&mut exporting_vrfs);

    Ok(RouteTargetDetail {
        summary,
        importing_vrfs,
        exporting_vrfs,
    })
}

/// `route-target <name|id>`: resolve a route target and build its relation graph.
/// Shared by CLI/MCP. Identity (and not-found/ambiguous typed errors) stays REST.
pub async fn route_target_detail_by_ref(
    client: &NetBoxClient,
    value: &str,
    not_found: &(dyn Fn(&str, &str) -> anyhow::Error + Send + Sync),
) -> Result<RouteTargetDetail> {
    let rt = client
        .route_target_by_ref(value)
        .await?
        .ok_or_else(|| not_found("route target", value))?;
    build_route_target_detail(client, rt).await
}

/// The route target's compact header card: tenant then description.
fn route_target_header_lines(v: &RouteTargetView) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(t) = &v.tenant {
        lines.push(format!("Tenant {t}"));
    }
    if let Some(d) = &v.description {
        lines.push(d.clone());
    }
    lines
}

/// Navigable rows from a route target's VRF list — each opens that VRF.
fn route_target_vrf_rows(vrfs: &[VrfRef], empty: &str) -> Vec<DetailRow> {
    if vrfs.is_empty() {
        return vec![DetailRow::plain(empty.to_string())];
    }
    vrfs.iter()
        .map(|v| DetailRow::link(v.display_line(), ObjectKind::Vrf, v.id))
        .collect()
}

/// Map a [`RouteTargetDetail`] to the TUI [`DetailView`]: a header card over the
/// importing VRFs (navigable summary slot), with the exporting VRFs as a tab.
fn route_target_detail_view(links: Vec<ObjectLink>, detail: RouteTargetDetail) -> DetailView {
    let id = detail.summary.id;
    let name = detail.summary.name.clone();
    let header = route_target_header_lines(&detail.summary);
    let importing = route_target_vrf_rows(&detail.importing_vrfs, "(no VRFs import this target)");
    let exporting = route_target_vrf_rows(&detail.exporting_vrfs, "(no VRFs export this target)");
    let importing_len = detail.importing_vrfs.len();

    let tabs = vec![DetailTab {
        key: 'e',
        label: format!("exporting·{}", detail.exporting_vrfs.len()),
        body: rows_to_text(&exporting),
        rows: exporting,
    }];

    DetailView::new(
        ObjectKind::RouteTarget,
        id,
        format!("route-target {name}"),
        rows_to_text(&importing),
    )
    .with_tabs(tabs)
    .with_links(links)
    .with_header(header)
    .with_summary(format!("importing·{importing_len}"), importing)
}

/// Build a route target's relation-graph [`DetailView`] (TUI): identity + links
/// (REST) then the importing/exporting VRFs.
async fn load_route_target_detail_view(
    client: &NetBoxClient,
    rt: RouteTarget,
) -> Result<DetailView> {
    let links = route_target_links(&rt);
    let detail = build_route_target_detail(client, rt).await?;
    Ok(route_target_detail_view(links, detail))
}

/// Navigable rows from a VRF detail's prefix tree — each opens its prefix; a
/// footer notes any capped overflow.
fn vrf_prefix_detail_rows(detail: &VrfDetail) -> Vec<DetailRow> {
    if detail.prefixes.is_empty() {
        return vec![DetailRow::plain("(no prefixes in this VRF)".to_string())];
    }
    let mut rows: Vec<DetailRow> = detail
        .prefixes
        .iter()
        .map(|p| DetailRow::link(p.display_line(), ObjectKind::Prefix, p.id))
        .collect();
    if detail.prefix_total as usize > detail.prefixes.len() {
        rows.push(DetailRow::plain(format!(
            "… {} more (showing {})",
            detail.prefix_total as usize - detail.prefixes.len(),
            detail.prefixes.len()
        )));
    }
    rows
}

/// Navigable rows from a VRF detail's addresses.
fn vrf_address_detail_rows(detail: &VrfDetail) -> Vec<DetailRow> {
    if detail.addresses.is_empty() {
        return vec![DetailRow::plain("(no addresses in this VRF)".to_string())];
    }
    let mut rows: Vec<DetailRow> = detail
        .addresses
        .iter()
        .map(|a| DetailRow::link(a.display_line(), ObjectKind::IpAddress, a.id))
        .collect();
    if detail.address_total as usize > detail.addresses.len() {
        rows.push(DetailRow::plain(format!(
            "… {} more",
            detail.address_total as usize - detail.addresses.len()
        )));
    }
    rows
}

/// Map a backend-neutral [`VrfDetail`] to the TUI [`DetailView`]: a fixed header
/// card over the prefix tree (navigable summary slot), with navigable `addresses`
/// and a `targets` tab.
fn vrf_detail_view(links: Vec<ObjectLink>, detail: VrfDetail) -> DetailView {
    let id = detail.summary.id;
    let name = detail.summary.name.clone();
    let header = vrf_header_lines(&detail.summary);
    let target_count = detail.summary.import_targets.len() + detail.summary.export_targets.len();
    let target_rows = vrf_target_detail_rows(&detail.summary);
    let prefix_rows = vrf_prefix_detail_rows(&detail);
    let address_rows = vrf_address_detail_rows(&detail);

    let tabs = vec![
        DetailTab {
            key: 'a',
            label: format!("addresses·{}", detail.address_total),
            body: rows_to_text(&address_rows),
            rows: address_rows,
        },
        DetailTab {
            key: 't',
            label: format!("targets·{target_count}"),
            body: rows_to_text(&target_rows),
            rows: target_rows,
        },
    ];

    DetailView::new(
        ObjectKind::Vrf,
        id,
        format!("vrf {name}"),
        rows_to_text(&prefix_rows),
    )
    .with_tabs(tabs)
    .with_links(links)
    .with_header(header)
    .with_summary(format!("prefixes·{}", detail.prefix_total), prefix_rows)
}

/// Build a VRF's routing-context [`DetailView`] (TUI): resolve identity + links
/// (REST) then the backend-neutral detail bundle.
async fn load_vrf_detail_view(client: &NetBoxClient, vrf: Vrf) -> Result<DetailView> {
    let links = vrf_links(&vrf);
    let detail = build_vrf_detail(client, vrf).await?;
    Ok(vrf_detail_view(links, detail))
}

/// Build an interface's [`DetailView`] (TUI), addressed by numeric id: its
/// attributes + cable-path trace (the same [`InterfaceView`] the CLI renders) as
/// the body, with a navigable link back to its device. This is the target the
/// device interfaces/cables tabs open; interfaces aren't part of the global search
/// fan-out (they need a device for context), so there's no by-ref resolution.
async fn load_interface_detail_view(client: &NetBoxClient, id: u64) -> Result<DetailView> {
    let iface: Interface = client
        .get(&format!("/api/dcim/interfaces/{id}/"), &[])
        .await?;
    let mut links = Vec::new();
    push_link(
        &mut links,
        "device",
        ObjectKind::Device,
        iface.device.as_ref(),
    );
    let device_label = iface.device.as_ref().map(BriefObject::name_label);
    let title = match &device_label {
        Some(d) => format!("interface {d} {}", iface.name),
        None => format!("interface {}", iface.name),
    };
    // Assigned IPs and the cable trace are independent fetches — run concurrently.
    let (ips, trace) = tokio::try_join!(
        client.interface_ips(iface.id, DETAIL_SECTION_CAP),
        client.interface_trace(iface.id),
    )?;
    let view = InterfaceView::build(iface, ips, trace);
    // The cable path is a dedicated, scrollable Path tab (the diagram); the body
    // is the interface's attributes + VLAN/connection/IP sections without it.
    let mut tabs = Vec::new();
    if !view.diagram.is_empty() {
        tabs.push(DetailTab {
            key: 'c',
            label: "cable path".to_string(),
            body: view.diagram.join("\n"),
            rows: Vec::new(),
        });
    }
    Ok(
        DetailView::new(ObjectKind::Interface, id, title, view.to_summary_plain())
            .with_tabs(tabs)
            .with_links(links),
    )
}

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
            // Devices and racks are independent fetches — run them concurrently.
            let (devices, racks) = tokio::join!(
                contained_devices_tab(client, "site_id", id),
                site_racks_tab(client, id),
            );
            tabs.push(devices);
            tabs.push(racks);
            (format!("site {}", v.name), v.to_key_values().render())
        }
        ObjectKind::Rack => {
            let r: Rack = client.get(&format!("/api/dcim/racks/{id}/"), &[]).await?;
            links = rack_links(&r);
            let u_height = r.u_height;
            let v = RackView::from_model(r);
            // Devices and the elevation are independent — fetch concurrently when
            // the rack has a height; otherwise just the devices tab.
            match u_height.filter(|h| *h > 0) {
                Some(uh) => {
                    let (devices, elevation) = tokio::join!(
                        contained_devices_tab(client, "rack_id", id),
                        rack_elevation_tab(client, id, uh),
                    );
                    tabs.push(devices);
                    tabs.push(elevation);
                }
                None => tabs.push(contained_devices_tab(client, "rack_id", id).await),
            }
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
            let (children, ips) = prefix_children_and_ips(client, &cidr, vrf_id).await?;
            let (title, prefix_body, prefix_tabs) = prefix_detail_parts(p, children, ips);
            tabs = prefix_tabs;
            (title, prefix_body)
        }
        ObjectKind::Vlan => {
            let vlan: Vlan = client.get(&format!("/api/ipam/vlans/{id}/"), &[]).await?;
            links = vlan_links(&vlan);
            let (prefixes, group) = tokio::try_join!(
                client.vlan_prefixes(vlan.id, DETAIL_SECTION_CAP),
                vlan_group_scope(client, &vlan),
            )?;
            tabs = vec![vlan_prefixes_tab(&prefixes)];
            let v = VlanView::build(vlan, prefixes, group);
            (format!("vlan {}", v.vid), v.to_detail_header())
        }
        ObjectKind::Circuit => {
            let c: Circuit = client
                .get(&format!("/api/circuits/circuits/{id}/"), &[])
                .await?;
            // The circuit view builds its own DetailView (attributes body + A↔Z
            // path tab + provider/site/device links).
            return load_circuit_detail_view(client, c).await;
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
        ObjectKind::Vrf => {
            let vrf: Vrf = client.get(&format!("/api/ipam/vrfs/{id}/"), &[]).await?;
            // The VRF view sets a header card + navigable summary rows, so it
            // builds the whole DetailView itself rather than the (title, body) pair.
            return load_vrf_detail_view(client, vrf).await;
        }
        ObjectKind::RouteTarget => {
            let rt: RouteTarget = client
                .get(&format!("/api/ipam/route-targets/{id}/"), &[])
                .await?;
            // Like the VRF view, the route target builds its own header + rows.
            return load_route_target_detail_view(client, rt).await;
        }
        ObjectKind::Interface => {
            // The interface view builds its own DetailView (title + body + device
            // link); it's reached only by id, from a device's interfaces/cables tab.
            return load_interface_detail_view(client, id).await;
        }
        ObjectKind::Mac => {
            let m: MacAddress = client
                .get(&format!("/api/dcim/mac-addresses/{id}/"), &[])
                .await?;
            let v = MacView::from_model(m);
            (format!("mac {}", v.mac_address), v.to_key_values().render())
        }
        ObjectKind::VirtualCircuit => {
            let vc: VirtualCircuit = client
                .get(&format!("/api/circuits/virtual-circuits/{id}/"), &[])
                .await?;
            let terminations = client.virtual_circuit_terminations(vc.id).await?;
            let v = VirtualCircuitView::build(vc, terminations);
            (format!("virtual circuit {}", v.cid), v.to_plain())
        }
        ObjectKind::RackGroup => {
            let rg: RackGroup = client
                .get(&format!("/api/dcim/rack-groups/{id}/"), &[])
                .await?;
            let v = RackGroupView::from_model(rg);
            (format!("rack group {}", v.name), v.to_key_values().render())
        }
        ObjectKind::VmType => {
            let t: VirtualMachineType = client
                .get(
                    &format!("/api/virtualization/virtual-machine-types/{id}/"),
                    &[],
                )
                .await?;
            let v = VirtualMachineTypeView::from_model(t);
            (format!("vm type {}", v.name), v.to_key_values().render())
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
            // Devices and racks are independent fetches — run them concurrently.
            let (devices, racks) = tokio::join!(
                contained_devices_tab(client, "site_id", id),
                site_racks_tab(client, id),
            );
            tabs.push(devices);
            tabs.push(racks);
            (id, format!("site {}", v.name), v.to_key_values().render())
        }
        ObjectKind::Rack => {
            let r = client
                .rack_by_ref(value)
                .await?
                .with_context(|| format!("no rack matched \"{value}\""))?;
            let id = r.id;
            links = rack_links(&r);
            let u_height = r.u_height;
            let v = RackView::from_model(r);
            // Devices and the elevation are independent — fetch concurrently when
            // the rack has a height; otherwise just the devices tab.
            match u_height.filter(|h| *h > 0) {
                Some(uh) => {
                    let (devices, elevation) = tokio::join!(
                        contained_devices_tab(client, "rack_id", id),
                        rack_elevation_tab(client, id, uh),
                    );
                    tabs.push(devices);
                    tabs.push(elevation);
                }
                None => tabs.push(contained_devices_tab(client, "rack_id", id).await),
            }
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
            let (children, ips) = prefix_children_and_ips(client, &cidr, vrf_id).await?;
            let (title, prefix_body, prefix_tabs) = prefix_detail_parts(p, children, ips);
            tabs = prefix_tabs;
            (id, title, prefix_body)
        }
        ObjectKind::Vlan => {
            let vlan = client
                .vlan_by_ref(value)
                .await?
                .with_context(|| format!("no VLAN matched \"{value}\""))?;
            let id = vlan.id;
            links = vlan_links(&vlan);
            let (prefixes, group) = tokio::try_join!(
                client.vlan_prefixes(vlan.id, DETAIL_SECTION_CAP),
                vlan_group_scope(client, &vlan),
            )?;
            tabs = vec![vlan_prefixes_tab(&prefixes)];
            let v = VlanView::build(vlan, prefixes, group);
            (id, format!("vlan {}", v.vid), v.to_detail_header())
        }
        ObjectKind::Circuit => {
            let c = client
                .circuit_by_ref(value)
                .await?
                .with_context(|| format!("no circuit matched \"{value}\""))?;
            // The circuit view builds its own DetailView (path tab + links).
            return load_circuit_detail_view(client, c).await;
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
        ObjectKind::Vrf => {
            let vrf = client
                .vrf_by_ref(value)
                .await?
                .with_context(|| format!("no vrf matched \"{value}\""))?;
            // The VRF view builds the whole DetailView itself (header + rows).
            return load_vrf_detail_view(client, vrf).await;
        }
        ObjectKind::RouteTarget => {
            let rt = client
                .route_target_by_ref(value)
                .await?
                .with_context(|| format!("no route target matched \"{value}\""))?;
            return load_route_target_detail_view(client, rt).await;
        }
        ObjectKind::Interface => {
            // Interfaces have no single-string reference (they need a device for
            // context), so the palette can't name one. A numeric id still resolves
            // (e.g. a future `nbox://interface/<id>`); anything else is not found.
            let id: u64 = value
                .trim()
                .parse()
                .map_err(|_| tui_not_found("interface", value))?;
            return load_interface_detail_view(client, id).await;
        }
        ObjectKind::Mac => {
            // A MAC is normalized before lookup; a non-MAC input is a usage error
            // (not a NetBox round-trip). Resolve → Ambiguous (exit 5) if several
            // interfaces carry it, else the flat MAC view.
            let mac = crate::mac::normalize(value).ok_or_else(|| tui_not_found("MAC", value))?;
            let candidates = client.mac_candidates(&mac).await?;
            let m = resolve_unique("MAC", value, candidates, label_mac, &tui_not_found)?;
            let id = m.id;
            let v = MacView::from_model(m);
            (
                id,
                format!("mac {}", v.mac_address),
                v.to_key_values().render(),
            )
        }
        ObjectKind::VirtualCircuit => {
            let vc = client
                .virtual_circuit_by_ref(value)
                .await?
                .with_context(|| format!("no virtual circuit matched \"{value}\""))?;
            let id = vc.id;
            let terminations = client.virtual_circuit_terminations(id).await?;
            let v = VirtualCircuitView::build(vc, terminations);
            (id, format!("virtual circuit {}", v.cid), v.to_plain())
        }
        ObjectKind::RackGroup => {
            let rg = client
                .rack_group_by_ref(value)
                .await?
                .with_context(|| format!("no rack group matched \"{value}\""))?;
            let id = rg.id;
            let v = RackGroupView::from_model(rg);
            (
                id,
                format!("rack group {}", v.name),
                v.to_key_values().render(),
            )
        }
        ObjectKind::VmType => {
            let t = client
                .vm_type_by_ref(value)
                .await?
                .with_context(|| format!("no virtual machine type matched \"{value}\""))?;
            let id = t.id;
            let v = VirtualMachineTypeView::from_model(t);
            (
                id,
                format!("vm type {}", v.name),
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
                device: None,
            }),
            tenant: None,
            assigned_object_type: None,
            assigned_object_id: None,
            assigned_object: None,
            dns_name: None,
            description: None,
            nat_inside: None,
            nat_outside: Vec::new(),
            owner: None,
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
        let links = device_links(&d);
        let got: Vec<(ObjectKind, u64, &str)> = links
            .iter()
            .map(|l| (l.kind, l.id, l.relation.as_str()))
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
        let links = prefix_links(&p);
        let got: Vec<(ObjectKind, &str)> = links
            .iter()
            .map(|l| (l.kind, l.relation.as_str()))
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
    fn circuit_links_cover_provider_site_and_patched_device_not_provider_network() {
        use serde_json::json;
        let c: Circuit = serde_json::from_value(json!({
            "id": 3, "url": "u", "cid": "ACME-1001",
            "provider": {"id": 1, "display": "ACME"},
            "tenant": {"id": 9, "name": "acme", "display": "acme"},
        }))
        .unwrap();
        let term_a: CircuitTermination = serde_json::from_value(json!({
            "id": 10, "term_side": "A",
            "termination": {"id": 1, "display": "DC1", "name": "DC1"},
            "termination_type": "dcim.site"
        }))
        .unwrap();
        let term_z: CircuitTermination = serde_json::from_value(json!({
            "id": 11, "term_side": "Z",
            "termination": {"id": 2, "display": "ACME Cloud"},
            "termination_type": "circuits.providernetwork"
        }))
        .unwrap();
        let resolved = vec![
            ResolvedTermination {
                termination: term_a,
                path: vec![PathHop {
                    to: "panel-1 7".to_string(),
                    cable: Some("#100".to_string()),
                    endpoint: false,
                    device: Some(DeviceRef {
                        id: 9,
                        name: "panel-1".to_string(),
                    }),
                }],
            },
            ResolvedTermination {
                termination: term_z,
                path: Vec::new(),
            },
        ];
        let links = circuit_links(&c, &resolved);
        let got: Vec<(ObjectKind, &str)> = links
            .iter()
            .map(|l| (l.kind, l.relation.as_str()))
            .collect();
        assert!(got.contains(&(ObjectKind::Provider, "provider")));
        assert!(got.contains(&(ObjectKind::Tenant, "tenant")));
        // The A-side site and the device along its path are navigable.
        assert!(got.contains(&(ObjectKind::Site, "A-side site")));
        assert!(got.contains(&(ObjectKind::Device, "A-side device")));
        // A provider network has no detail kind, so the Z-side emits no site link.
        assert!(!got.iter().any(|(_, r)| *r == "Z-side site"));
    }

    #[test]
    fn prefix_detail_parts_builds_navigable_children_and_address_tabs() {
        use serde_json::json;
        let p: Prefix =
            serde_json::from_value(json!({"id": 2, "url": "u", "prefix": "10.0.0.0/16"})).unwrap();
        let children: Vec<Prefix> = vec![
            serde_json::from_value(json!({"id": 6, "url": "u", "prefix": "10.0.0.0/24"})).unwrap(),
            serde_json::from_value(json!({"id": 7, "url": "u", "prefix": "10.0.1.0/24"})).unwrap(),
        ];
        let ips: Vec<IpAddress> = vec![
            serde_json::from_value(json!({
                "id": 11, "url": "u", "address": "10.0.0.1/24",
                "assigned_object": {"display": "eth0", "device": {"display": "edge01"}}
            }))
            .unwrap(),
        ];

        let (title, body, tabs) = prefix_detail_parts(p, children, ips);
        assert_eq!(title, "prefix 10.0.0.0/16");
        // Body is the header only — the child/IP lists moved to navigable tabs, so
        // they must not also appear inline (that would double them).
        assert!(body.contains("prefix: 10.0.0.0/16"));
        assert!(!body.contains("Child Prefixes"));
        assert!(!body.contains("IP Addresses"));

        // children tab: each child prefix opens that prefix on Enter.
        let children_tab = &tabs[0];
        assert_eq!(children_tab.key, 'c');
        assert_eq!(children_tab.label, "children·2");
        assert_eq!(children_tab.rows[0].target, Some((ObjectKind::Prefix, 6)));
        assert_eq!(children_tab.rows[1].target, Some((ObjectKind::Prefix, 7)));

        // addresses tab: each IP opens that IP; the assignment trails the address.
        let addr_tab = &tabs[1];
        assert_eq!(addr_tab.key, 'a');
        assert_eq!(addr_tab.label, "addresses·1");
        assert_eq!(addr_tab.rows[0].target, Some((ObjectKind::IpAddress, 11)));
        assert!(addr_tab.rows[0].text.contains("10.0.0.1/24"));
        assert!(addr_tab.rows[0].text.contains("edge01"));
    }

    #[test]
    fn device_ip_and_vlan_rows_are_navigable() {
        let ips = vec![
            IpRow {
                id: 11,
                address: "10.0.0.1/24".to_string(),
                interface: Some("eth0".to_string()),
            },
            IpRow {
                id: 12,
                address: "10.0.0.2/24".to_string(),
                interface: None,
            },
        ];
        let ip_rows = device_ip_rows(&ips);
        assert_eq!(ip_rows[0].target, Some((ObjectKind::IpAddress, 11)));
        assert!(ip_rows[0].text.contains("10.0.0.1/24"));
        assert!(ip_rows[0].text.contains("eth0"));
        assert_eq!(ip_rows[1].target, Some((ObjectKind::IpAddress, 12)));

        let vlans = vec![VlanRow {
            id: 20,
            vlan: "20 (prod)".to_string(),
        }];
        let vlan_rows = device_vlan_rows(&vlans);
        assert_eq!(vlan_rows[0].target, Some((ObjectKind::Vlan, 20)));
        assert!(vlan_rows[0].text.contains("20 (prod)"));
    }

    #[test]
    fn device_interface_rows_open_the_interface() {
        let ifaces = vec![
            IfaceRow {
                id: 101,
                name: "xe-0/0/0".to_string(),
                enabled: Some(true),
                type_: Some("SFP+".to_string()),
                description: None,
            },
            IfaceRow {
                id: 102,
                name: "xe-0/0/1".to_string(),
                enabled: Some(false),
                type_: None,
                description: None,
            },
        ];
        let rows = device_interface_rows(&ifaces);
        // Each interface opens its own detail on Enter.
        assert_eq!(rows[0].target, Some((ObjectKind::Interface, 101)));
        assert!(rows[0].text.contains("xe-0/0/0"));
        assert!(rows[0].text.contains("SFP+"));
        // A disabled interface is marked, and still navigable.
        assert_eq!(rows[1].target, Some((ObjectKind::Interface, 102)));
        assert!(rows[1].text.contains("(disabled)"));
    }

    #[test]
    fn device_cable_rows_open_the_local_interface() {
        let cables = vec![
            CableRow {
                id: 101,
                interface: "xe-0/0/0".to_string(),
                cable: Some("#3".to_string()),
                connected_to: vec!["core01 xe-1/0/0".to_string()],
            },
            CableRow {
                id: 103,
                interface: "xe-0/0/2".to_string(),
                cable: Some("#5".to_string()),
                connected_to: Vec::new(),
            },
        ];
        let rows = device_cable_rows(&cables);
        // A cable row opens the LOCAL interface's detail (where the trace lives).
        assert_eq!(rows[0].target, Some((ObjectKind::Interface, 101)));
        assert!(rows[0].text.contains("xe-0/0/0"));
        assert!(rows[0].text.contains("core01 xe-1/0/0"));
        // No far end → the cable label trails instead of a "->" connection.
        assert_eq!(rows[1].target, Some((ObjectKind::Interface, 103)));
        assert!(rows[1].text.contains("#5"));
    }

    #[test]
    fn vlan_prefixes_tab_is_navigable() {
        use serde_json::json;
        let prefixes: Vec<Prefix> = vec![
            serde_json::from_value(json!({"id": 21, "url": "u", "prefix": "10.44.208.0/24"}))
                .unwrap(),
            serde_json::from_value(json!({"id": 22, "url": "u", "prefix": "10.45.208.0/24"}))
                .unwrap(),
        ];
        let tab = vlan_prefixes_tab(&prefixes);
        assert_eq!(tab.key, 'p');
        assert_eq!(tab.label, "prefixes·2");
        assert_eq!(tab.rows[0].target, Some((ObjectKind::Prefix, 21)));
        assert_eq!(tab.rows[1].target, Some((ObjectKind::Prefix, 22)));
        // Empty → one non-navigable placeholder row.
        let empty = vlan_prefixes_tab(&[]);
        assert_eq!(empty.label, "prefixes·0");
        assert_eq!(empty.rows.len(), 1);
        assert_eq!(empty.rows[0].target, None);
    }

    #[test]
    fn site_and_rack_contained_rows_are_navigable() {
        use serde_json::json;
        let devices: Vec<crate::netbox::models::dcim::Device> = vec![
            serde_json::from_value(
                json!({"id": 31, "url": "u", "name": "edge01", "custom_fields": {}}),
            )
            .unwrap(),
            serde_json::from_value(
                json!({"id": 32, "url": "u", "name": "edge02", "custom_fields": {}}),
            )
            .unwrap(),
        ];
        let drows = device_link_rows(&devices);
        assert_eq!(drows[0].target, Some((ObjectKind::Device, 31)));
        assert_eq!(drows[1].target, Some((ObjectKind::Device, 32)));
        assert!(drows[0].text.contains("edge01"));

        let racks: Vec<crate::netbox::models::dcim::Rack> =
            vec![serde_json::from_value(json!({"id": 7, "url": "u", "name": "R12"})).unwrap()];
        let rrows = rack_link_rows(&racks);
        assert_eq!(rrows[0].target, Some((ObjectKind::Rack, 7)));
        assert!(rrows[0].text.contains("R12"));

        // Empty → a single non-navigable placeholder row.
        assert_eq!(device_link_rows(&[])[0].target, None);
        assert_eq!(rack_link_rows(&[])[0].target, None);
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

    /// The cache serializes assembled detail views to JSON bytes, so a view must
    /// survive a round-trip with its tabs and links intact.
    #[test]
    fn detail_view_json_roundtrips_for_caching() {
        let view = DetailView::new(
            ObjectKind::Device,
            7,
            "device edge01".into(),
            "summary".into(),
        )
        .with_tabs(vec![DetailTab {
            key: 'i',
            label: "interfaces".into(),
            body: "eth0".into(),
            rows: Vec::new(),
        }])
        .with_links(vec![ObjectLink {
            kind: ObjectKind::Site,
            id: 5,
            relation: "site".into(),
            label: "iad1".into(),
        }]);

        let bytes = serde_json::to_vec(&view).expect("serialize");
        let back: DetailView = serde_json::from_slice(&bytes).expect("deserialize");

        assert_eq!(back.kind, ObjectKind::Device);
        assert_eq!(back.id, 7);
        assert_eq!(back.title, "device edge01");
        assert_eq!(back.tabs.len(), 1);
        assert_eq!(back.tabs[0].key, 'i');
        assert_eq!(back.links[0].relation, "site");
        assert_eq!(back.links[0].kind, ObjectKind::Site);
    }

    // --- VRF detail: GraphQL accelerator vs REST ---

    mod vrf_backends {
        use super::super::*;
        use crate::config::{ApiConfig, BackendPreference, ProfileConfig};
        use serde_json::json;
        use wiremock::matchers::{body_string_contains, method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn not_found(noun: &str, value: &str) -> anyhow::Error {
            NboxError::NotFound(format!("no {noun} matched \"{value}\"")).into()
        }

        async fn mount_vrf_identity(server: &MockServer) {
            Mock::given(method("GET"))
                .and(path("/api/ipam/vrfs/42/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": 42, "url": "http://nb/api/ipam/vrfs/42/",
                    "name": "customer-prod", "rd": "65000:42",
                    "tenant": {"id": 1, "display": "Acme"},
                    "prefix_count": 2,
                    "ipaddress_count": 1
                })))
                .mount(server)
                .await;
        }

        async fn mount_rest_children(server: &MockServer) {
            Mock::given(method("GET"))
                .and(path("/api/ipam/prefixes/"))
                .and(query_param("vrf_id", "42"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 2, "next": null, "previous": null,
                    "results": [
                        {
                            "id": 1, "url": "u", "prefix": "10.50.0.0/16",
                            "_depth": 0,
                            "status": {"value": "container", "label": "container"},
                            "description": "supernet"
                        },
                        {
                            "id": 2, "url": "u", "prefix": "10.50.1.0/24",
                            "_depth": 1,
                            "status": {"value": "active", "label": "active"},
                            "description": ""
                        }
                    ]
                })))
                .mount(server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/ipam/ip-addresses/"))
                .and(query_param("vrf_id", "42"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 1, "next": null, "previous": null,
                    "results": [
                        {
                            "id": 9, "url": "u", "address": "10.50.1.1/24",
                            "status": {"value": "active", "label": "active"},
                            "dns_name": "gw.customer",
                            "description": ""
                        }
                    ]
                })))
                .mount(server)
                .await;
        }

        fn rest_client(server: &MockServer) -> NetBoxClient {
            NetBoxClient::new(
                &ProfileConfig {
                    url: server.uri(),
                    ..Default::default()
                },
                None,
            )
            .unwrap()
        }

        fn graphql_client(server: &MockServer) -> NetBoxClient {
            NetBoxClient::new(
                &ProfileConfig {
                    url: server.uri(),
                    api: Some(ApiConfig {
                        search: None,
                        vrf: Some(BackendPreference::Graphql),
                        route_target: None,
                    }),
                    ..Default::default()
                },
                None,
            )
            .unwrap()
        }

        async fn mount_graphql_probe(server: &MockServer) {
            let list_field = |name: &str, filter: &str| {
                json!({
                    "name": name,
                    "args": [
                        {"name": "filters", "type": {"kind": "INPUT_OBJECT", "name": filter}},
                        {"name": "pagination", "type": {"kind": "INPUT_OBJECT", "name": "PaginationInput"}}
                    ]
                })
            };
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .and(body_string_contains("__schema"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {"__schema": {"queryType": {"fields": [
                        list_field("prefix_list", "PrefixFilter"),
                        list_field("ip_address_list", "IPAddressFilter"),
                    ]}}}
                })))
                .mount(server)
                .await;
            let vrf_id_field = json!({"name": "vrf_id", "type": {"kind": "INPUT_OBJECT", "name": "IntegerLookup"}});
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .and(body_string_contains("DeviceFilter"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {
                        "prefix": {"inputFields": [vrf_id_field.clone()]},
                        "ip": {"inputFields": [vrf_id_field]}
                    }
                })))
                .mount(server)
                .await;
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .and(body_string_contains("ASNFilter"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {}})))
                .mount(server)
                .await;
        }

        async fn mount_graphql_bundle(server: &MockServer, response: ResponseTemplate) {
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .and(body_string_contains("ip_address_list(filters"))
                .respond_with(response)
                .mount(server)
                .await;
        }

        fn graphql_bundle_body() -> serde_json::Value {
            json!({
                "data": {
                    "prefix_list": [
                        {"id": "1", "prefix": "10.50.0.0/16", "_depth": 0, "status": "container", "description": "supernet"},
                        {"id": "2", "prefix": "10.50.1.0/24", "_depth": 1, "status": "active", "description": ""}
                    ],
                    "ip_address_list": [
                        {"id": "9", "address": "10.50.1.1/24", "status": "active", "dns_name": "gw.customer", "description": ""}
                    ]
                }
            })
        }

        #[tokio::test]
        async fn rest_and_graphql_vrf_detail_are_byte_identical() {
            let rest = MockServer::start().await;
            mount_vrf_identity(&rest).await;
            mount_rest_children(&rest).await;

            let gql = MockServer::start().await;
            mount_vrf_identity(&gql).await;
            mount_graphql_probe(&gql).await;
            mount_graphql_bundle(
                &gql,
                ResponseTemplate::new(200).set_body_json(graphql_bundle_body()),
            )
            .await;

            let rest_detail = vrf_detail_by_ref(&rest_client(&rest), "42", &not_found)
                .await
                .expect("rest detail");
            let gql_detail = vrf_detail_by_ref(&graphql_client(&gql), "42", &not_found)
                .await
                .expect("graphql detail");

            assert_eq!(
                rest_detail.to_plain(),
                gql_detail.to_plain(),
                "plain output must be byte-identical across backends"
            );
            assert_eq!(
                serde_json::to_string(&rest_detail).unwrap(),
                serde_json::to_string(&gql_detail).unwrap(),
                "serialized JSON must be byte-identical across backends"
            );
        }

        #[tokio::test]
        async fn graphql_preference_with_support_uses_graphql_path() {
            let server = MockServer::start().await;
            mount_vrf_identity(&server).await;
            mount_graphql_probe(&server).await;
            mount_graphql_bundle(
                &server,
                ResponseTemplate::new(200).set_body_json(graphql_bundle_body()),
            )
            .await;
            Mock::given(method("GET"))
                .and(path("/api/ipam/prefixes/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 0, "next": null, "previous": null, "results": []
                })))
                .expect(0)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/ipam/ip-addresses/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 0, "next": null, "previous": null, "results": []
                })))
                .expect(0)
                .mount(&server)
                .await;

            let client = graphql_client(&server);
            assert!(
                client
                    .effective_backend(ApiSurface::Vrf)
                    .await
                    .uses_graphql()
            );
            let detail = vrf_detail_by_ref(&client, "42", &not_found)
                .await
                .expect("graphql detail");
            assert_eq!(detail.prefixes.len(), 2);
            assert_eq!(detail.addresses.len(), 1);
        }

        #[tokio::test]
        async fn graphql_preference_without_support_falls_back_to_rest() {
            let server = MockServer::start().await;
            mount_vrf_identity(&server).await;
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .and(body_string_contains("__schema"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {"__schema": {"queryType": {"fields": []}}}
                })))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {}})))
                .mount(&server)
                .await;
            mount_rest_children(&server).await;

            let client = graphql_client(&server);
            let effective = client.effective_backend(ApiSurface::Vrf).await;
            assert!(!effective.uses_graphql(), "missing schema -> REST fallback");
            assert!(
                effective
                    .reason()
                    .is_some_and(|r| r.contains("prefix_list.vrf_id")),
                "fallback reason names the missing schema piece"
            );

            let detail = vrf_detail_by_ref(&client, "42", &not_found)
                .await
                .expect("rest fallback detail");
            assert_eq!(detail.summary.name, "customer-prod");

            let requests = server.received_requests().await.unwrap();
            assert!(
                !requests
                    .iter()
                    .any(|r| String::from_utf8_lossy(&r.body).contains("ip_address_list(filters")),
                "a capability fallback must not issue the GraphQL bundle query"
            );
        }

        #[tokio::test]
        async fn runtime_graphql_bundle_failure_retries_rest() {
            let server = MockServer::start().await;
            mount_vrf_identity(&server).await;
            mount_graphql_probe(&server).await;
            mount_graphql_bundle(
                &server,
                ResponseTemplate::new(200).set_body_json(json!({
                    "errors": [{"message": "maximum query depth exceeded"}]
                })),
            )
            .await;
            mount_rest_children(&server).await;

            let detail = vrf_detail_by_ref(&graphql_client(&server), "42", &not_found)
                .await
                .expect("REST fallback detail");
            assert_eq!(detail.summary.name, "customer-prod");
            assert_eq!(detail.prefixes.len(), 2);
            assert_eq!(detail.addresses.len(), 1);

            let requests = server.received_requests().await.unwrap();
            assert!(
                requests
                    .iter()
                    .any(|r| String::from_utf8_lossy(&r.body).contains("ip_address_list(filters")),
                "runtime fallback first attempts the GraphQL bundle"
            );
            assert!(
                requests
                    .iter()
                    .any(|r| r.url.path() == "/api/ipam/prefixes/"),
                "runtime GraphQL failure retries REST children"
            );
        }
    }

    // --- Route-target relation graph: GraphQL accelerator vs REST ---

    mod route_target_backends {
        use super::super::*;
        use crate::config::{ApiConfig, BackendPreference, ProfileConfig};
        use serde_json::json;
        use wiremock::matchers::{body_string_contains, method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn not_found(noun: &str, value: &str) -> anyhow::Error {
            NboxError::NotFound(format!("no {noun} matched \"{value}\"")).into()
        }

        /// The route-target identity GET (id fast-path) the REST resolver hits
        /// first on both backends — identity stays canonical REST.
        async fn mount_rt_identity(server: &MockServer) {
            Mock::given(method("GET"))
                .and(path("/api/ipam/route-targets/5/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": 5, "url": "http://nb/api/ipam/route-targets/5/",
                    "name": "65000:100", "tenant": {"id": 1, "display": "Acme"}
                })))
                .mount(server)
                .await;
        }

        fn rest_client(server: &MockServer) -> NetBoxClient {
            NetBoxClient::new(
                &ProfileConfig {
                    url: server.uri(),
                    ..Default::default()
                },
                None,
            )
            .unwrap()
        }

        fn graphql_client(server: &MockServer) -> NetBoxClient {
            NetBoxClient::new(
                &ProfileConfig {
                    url: server.uri(),
                    api: Some(ApiConfig {
                        search: None,
                        vrf: None,
                        route_target: Some(BackendPreference::Graphql),
                    }),
                    ..Default::default()
                },
                None,
            )
            .unwrap()
        }

        /// Mount the GraphQL schema + filter probes that expose route_target_list
        /// with an `id` lookup (so the bundle scopes to one target).
        async fn mount_graphql_probe(server: &MockServer) {
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .and(body_string_contains("__schema"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {"__schema": {"queryType": {"fields": [
                        {"name": "route_target_list", "args": [
                            {"name": "filters", "type": {"kind": "INPUT_OBJECT", "name": "RouteTargetFilter"}}
                        ]}
                    ]}}}
                })))
                .mount(server)
                .await;
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .and(body_string_contains("DeviceFilter"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {}})))
                .mount(server)
                .await;
            let id_field =
                json!({"name": "id", "type": {"kind": "INPUT_OBJECT", "name": "IDFilterLookup"}});
            // Match the filter probe on its batch marker ("ASNFilter"), not
            // "RouteTargetFilter" — the bundle query's `$rt: RouteTargetFilter`
            // variable would otherwise be captured by this probe mock.
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .and(body_string_contains("ASNFilter"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {"routeTarget": {"inputFields": [id_field]}}
                })))
                .mount(server)
                .await;
        }

        #[tokio::test]
        async fn graphql_bundle_assembles_route_target_detail() {
            let server = MockServer::start().await;
            mount_rt_identity(&server).await;
            mount_graphql_probe(&server).await;
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .and(body_string_contains("route_target_list(filters"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {"route_target_list": [{
                        "importing_vrfs": [
                            {"id": "1", "name": "customer-prod", "rd": "65000:100"},
                            {"id": "2", "name": "customer-dev", "rd": null}
                        ],
                        "exporting_vrfs": [
                            {"id": "1", "name": "customer-prod", "rd": "65000:100"}
                        ]
                    }]}
                })))
                .mount(&server)
                .await;

            let detail = route_target_detail_by_ref(&graphql_client(&server), "5", &not_found)
                .await
                .expect("graphql detail");

            assert_eq!(detail.summary.name, "65000:100");
            assert_eq!(detail.summary.tenant.as_deref(), Some("Acme"));
            // Sorted by (name, rd): customer-dev before customer-prod.
            assert_eq!(
                detail
                    .importing_vrfs
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["customer-dev", "customer-prod"]
            );
            assert_eq!(detail.exporting_vrfs.len(), 1);
            assert_eq!(detail.exporting_vrfs[0].id, 1);
        }

        /// The REST-built and GraphQL-built `RouteTargetDetail` are byte-identical
        /// for the same data: identical `to_plain()` and identical serialized JSON.
        #[tokio::test]
        async fn rest_and_graphql_detail_are_byte_identical() {
            // REST server: identity GET + two vrfs list calls. NetBox returns the
            // vrfs name-ordered already; supply them so the result matches GraphQL.
            let rest = MockServer::start().await;
            mount_rt_identity(&rest).await;
            Mock::given(method("GET"))
                .and(path("/api/ipam/vrfs/"))
                .and(query_param("import_target_id", "5"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 2, "next": null, "previous": null,
                    "results": [
                        {"id": 2, "url": "u", "name": "customer-dev"},
                        {"id": 1, "url": "u", "name": "customer-prod", "rd": "65000:100"}
                    ]
                })))
                .mount(&rest)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/ipam/vrfs/"))
                .and(query_param("export_target_id", "5"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 1, "next": null, "previous": null,
                    "results": [
                        {"id": 1, "url": "u", "name": "customer-prod", "rd": "65000:100"}
                    ]
                })))
                .mount(&rest)
                .await;

            // GraphQL server: identity GET + probes + bundle. The nested rows are
            // deliberately given in a DIFFERENT order than REST to prove the sort
            // makes the two paths converge.
            let gql = MockServer::start().await;
            mount_rt_identity(&gql).await;
            mount_graphql_probe(&gql).await;
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .and(body_string_contains("route_target_list(filters"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {"route_target_list": [{
                        "importing_vrfs": [
                            {"id": "1", "name": "customer-prod", "rd": "65000:100"},
                            {"id": "2", "name": "customer-dev", "rd": ""}
                        ],
                        "exporting_vrfs": [
                            {"id": "1", "name": "customer-prod", "rd": "65000:100"}
                        ]
                    }]}
                })))
                .mount(&gql)
                .await;

            let rest_detail = route_target_detail_by_ref(&rest_client(&rest), "5", &not_found)
                .await
                .expect("rest detail");
            let gql_detail = route_target_detail_by_ref(&graphql_client(&gql), "5", &not_found)
                .await
                .expect("graphql detail");

            assert_eq!(
                rest_detail.to_plain(),
                gql_detail.to_plain(),
                "plain output must be byte-identical across backends"
            );
            assert_eq!(
                serde_json::to_string(&rest_detail).unwrap(),
                serde_json::to_string(&gql_detail).unwrap(),
                "serialized JSON must be byte-identical across backends"
            );
        }

        /// Capability gating: `route_target = "graphql"` with a supporting probe
        /// routes through GraphQL (the REST vrfs list calls are NEVER made).
        #[tokio::test]
        async fn graphql_preference_with_support_uses_graphql_path() {
            let server = MockServer::start().await;
            mount_rt_identity(&server).await;
            mount_graphql_probe(&server).await;
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .and(body_string_contains("route_target_list(filters"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {"route_target_list": [{"importing_vrfs": [], "exporting_vrfs": []}]}
                })))
                .mount(&server)
                .await;
            // The REST relation calls must NOT happen on the GraphQL path.
            Mock::given(method("GET"))
                .and(path("/api/ipam/vrfs/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 0, "next": null, "previous": null, "results": []
                })))
                .expect(0)
                .mount(&server)
                .await;

            let client = graphql_client(&server);
            assert!(
                client
                    .effective_backend(ApiSurface::RouteTarget)
                    .await
                    .uses_graphql()
            );
            let detail = route_target_detail_by_ref(&client, "5", &not_found)
                .await
                .expect("graphql detail");
            assert!(detail.importing_vrfs.is_empty());
            assert!(detail.exporting_vrfs.is_empty());
        }

        #[tokio::test]
        async fn runtime_graphql_bundle_failure_retries_rest() {
            let server = MockServer::start().await;
            mount_rt_identity(&server).await;
            mount_graphql_probe(&server).await;
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .and(body_string_contains("route_target_list(filters"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "errors": [{"message": "maximum query depth exceeded"}]
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/ipam/vrfs/"))
                .and(query_param("import_target_id", "5"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 1, "next": null, "previous": null,
                    "results": [
                        {"id": 1, "url": "u", "name": "customer-prod", "rd": "65000:100"}
                    ]
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/ipam/vrfs/"))
                .and(query_param("export_target_id", "5"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 0, "next": null, "previous": null, "results": []
                })))
                .mount(&server)
                .await;

            let detail = route_target_detail_by_ref(&graphql_client(&server), "5", &not_found)
                .await
                .expect("REST fallback detail");
            assert_eq!(detail.importing_vrfs.len(), 1);
            assert_eq!(detail.importing_vrfs[0].name, "customer-prod");
            assert!(detail.exporting_vrfs.is_empty());

            let requests = server.received_requests().await.unwrap();
            assert!(
                requests
                    .iter()
                    .any(|r| String::from_utf8_lossy(&r.body).contains("route_target_list(filters")),
                "runtime fallback first attempts the GraphQL bundle"
            );
            assert!(
                requests.iter().any(|r| r.url.path() == "/api/ipam/vrfs/"),
                "runtime GraphQL failure retries REST relations"
            );
        }

        /// Capability gating: `route_target = "graphql"` but the schema lacks
        /// route_target_list → REST fallback, with the reason surfaced. The REST
        /// vrfs list calls are made; no GraphQL bundle query is issued.
        #[tokio::test]
        async fn graphql_preference_without_support_falls_back_to_rest() {
            let server = MockServer::start().await;
            mount_rt_identity(&server).await;
            // Schema probe exposes no route_target_list at all.
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .and(body_string_contains("__schema"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {"__schema": {"queryType": {"fields": []}}}
                })))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/graphql/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {}})))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/ipam/vrfs/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count": 0, "next": null, "previous": null, "results": []
                })))
                .mount(&server)
                .await;

            let client = graphql_client(&server);
            let effective = client.effective_backend(ApiSurface::RouteTarget).await;
            assert!(!effective.uses_graphql(), "missing schema → REST fallback");
            assert!(
                effective
                    .reason()
                    .is_some_and(|r| r.contains("route_target_list")),
                "fallback reason names the missing schema piece"
            );

            // The REST path still assembles the detail (empty relations here).
            let detail = route_target_detail_by_ref(&client, "5", &not_found)
                .await
                .expect("rest fallback detail");
            assert!(detail.importing_vrfs.is_empty());

            // No GraphQL bundle query was issued (only schema/filter probes).
            let requests = server.received_requests().await.unwrap();
            assert!(
                !requests
                    .iter()
                    .any(|r| String::from_utf8_lossy(&r.body).contains("route_target_list(filters")),
                "a fallback must not issue the GraphQL bundle query"
            );
        }
    }

    // ===== Safe-write planner/apply unit tests (ADR-0001) =====
    //
    // Direct unit-level coverage of `plan_interface_description_update` +
    // `apply_interface_description_update`: precondition selection (ETag vs
    // last_updated), no-op, minimal patch, and the two stale-precondition paths
    // (412 + pre-4.6 before-hash mismatch). The binary `tests/write_tests.rs`
    // pins the same contracts at the process boundary.
    mod write_planner {
        use super::super::{apply_interface_description_update, plan_interface_description_update};
        use crate::error::NboxError;
        use crate::netbox::client::NetBoxClient;
        use crate::netbox::mutation::Precondition;
        use serde_json::json;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn client(server: &MockServer) -> NetBoxClient {
            NetBoxClient::new(
                &crate::config::ProfileConfig {
                    url: server.uri(),
                    ..Default::default()
                },
                None,
            )
            .expect("client")
        }

        async fn mount_resolution(server: &MockServer) {
            Mock::given(method("GET"))
                .and(path("/api/dcim/devices/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count":1,"next":null,"previous":null,
                    "results":[{"id":7,"url":"u","name":"edge01"}]
                })))
                .mount(server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/dcim/interfaces/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "count":1,"next":null,"previous":null,
                    "results":[{
                        "id":42,"url":"u","name":"xe-0/0/1",
                        "description":"old","last_updated":"2026-06-26T10:00:00Z"
                    }]
                })))
                .mount(server)
                .await;
        }

        async fn mount_detail(server: &MockServer, etag: Option<&str>, last_updated: &str) {
            let mut r = ResponseTemplate::new(200).set_body_json(json!({
                "id":42,"url":"u","name":"xe-0/0/1",
                "description":"old","last_updated":last_updated
            }));
            if let Some(e) = etag {
                r = r.insert_header("ETag", e);
            }
            Mock::given(method("GET"))
                .and(path("/api/dcim/interfaces/42/"))
                .respond_with(r)
                .mount(server)
                .await;
        }

        #[tokio::test]
        async fn plan_uses_etag_precondition_when_present() {
            let server = MockServer::start().await;
            mount_resolution(&server).await;
            mount_detail(&server, Some("\"v1\""), "2026-06-26T10:00:00Z").await;
            let plan = plan_interface_description_update(
                &client(&server),
                "edge01",
                "xe-0/0/1",
                "new",
                None,
                "default",
                &not_found,
            )
            .await
            .expect("plan");
            assert!(matches!(plan.precondition, Precondition::Etag { .. }));
            // Minimal patch — only the scoped field, never the full object.
            assert_eq!(plan.patch, json!({"description": "new"}));
            assert!(!plan.no_op);
        }

        #[tokio::test]
        async fn plan_uses_last_updated_precondition_when_no_etag() {
            let server = MockServer::start().await;
            mount_resolution(&server).await;
            mount_detail(&server, None, "2026-06-26T10:00:00Z").await;
            let plan = plan_interface_description_update(
                &client(&server),
                "edge01",
                "xe-0/0/1",
                "new",
                None,
                "default",
                &not_found,
            )
            .await
            .expect("plan");
            assert!(matches!(
                plan.precondition,
                Precondition::LastUpdated { .. }
            ));
        }

        #[tokio::test]
        async fn plan_detects_noop_when_value_unchanged() {
            let server = MockServer::start().await;
            mount_resolution(&server).await;
            mount_detail(&server, None, "2026-06-26T10:00:00Z").await;
            let plan = plan_interface_description_update(
                &client(&server),
                "edge01",
                "xe-0/0/1",
                "old", // == current
                None,
                "default",
                &not_found,
            )
            .await
            .expect("plan");
            assert!(plan.no_op);
            assert_eq!(plan.patch, json!({}));
        }

        #[tokio::test]
        async fn apply_sends_patch_and_returns_receipt() {
            let server = MockServer::start().await;
            mount_resolution(&server).await;
            mount_detail(&server, Some("\"v1\""), "2026-06-26T10:00:00Z").await;
            Mock::given(method("PATCH"))
                .and(path("/api/dcim/interfaces/42/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id":42,"url":"u","name":"xe-0/0/1","description":"new"
                })))
                .mount(&server)
                .await;
            let client = client(&server);
            let plan = plan_interface_description_update(
                &client, "edge01", "xe-0/0/1", "new", None, "default", &not_found,
            )
            .await
            .expect("plan");
            let receipt = apply_interface_description_update(&client, &plan)
                .await
                .expect("apply");
            assert!(receipt.applied);
            assert!(!receipt.no_op);
            assert_eq!(receipt.status, 200);
            assert!(receipt.message.starts_with("applied: interface"));
        }

        #[tokio::test]
        async fn apply_412_maps_to_stale_precondition() {
            let server = MockServer::start().await;
            mount_resolution(&server).await;
            mount_detail(&server, Some("\"v1\""), "2026-06-26T10:00:00Z").await;
            Mock::given(method("PATCH"))
                .and(path("/api/dcim/interfaces/42/"))
                .respond_with(ResponseTemplate::new(412).set_body_string("precondition"))
                .mount(&server)
                .await;
            let client = client(&server);
            let plan = plan_interface_description_update(
                &client, "edge01", "xe-0/0/1", "new", None, "default", &not_found,
            )
            .await
            .expect("plan");
            let err = apply_interface_description_update(&client, &plan)
                .await
                .unwrap_err();
            assert!(
                err.chain().any(|c| c
                    .downcast_ref::<crate::error::NboxError>()
                    .is_some_and(|n| matches!(n, crate::error::NboxError::StalePrecondition(_)))),
                "412 → StalePrecondition: {err:#}"
            );
        }

        #[tokio::test]
        async fn apply_pre46_refuses_when_last_updated_changed_between_plan_and_apply() {
            let server = MockServer::start().await;
            mount_resolution(&server).await;
            // Plan reads T1; the apply re-read returns T2 (no ETag → pre-4.6).
            Mock::given(method("GET"))
                .and(path("/api/dcim/interfaces/42/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id":42,"url":"u","name":"xe-0/0/1",
                    "description":"old","last_updated":"2026-06-26T10:00:00Z"
                })))
                .up_to_n_times(1)
                .with_priority(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/dcim/interfaces/42/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id":42,"url":"u","name":"xe-0/0/1",
                    "description":"old","last_updated":"2026-06-26T11:00:00Z"
                })))
                .mount(&server)
                .await;
            let client = client(&server);
            let plan = plan_interface_description_update(
                &client, "edge01", "xe-0/0/1", "new", None, "default", &not_found,
            )
            .await
            .expect("plan");
            let err = apply_interface_description_update(&client, &plan)
                .await
                .unwrap_err();
            assert!(
                err.chain().any(|c| c
                    .downcast_ref::<crate::error::NboxError>()
                    .is_some_and(|n| matches!(n, crate::error::NboxError::StalePrecondition(_)))),
                "pre-4.6 mismatch → StalePrecondition: {err:#}"
            );
            // No PATCH was attempted — the read-before-write caught the change.
            let patched = server
                .received_requests()
                .await
                .unwrap()
                .into_iter()
                .filter(|r| r.method.as_str() == "PATCH")
                .count();
            assert_eq!(patched, 0);
        }

        // A not-found closure matching the CLI's shape (exit-4 NboxError::NotFound),
        // so resolution failures here behave like the real `nbox interface` path.
        fn not_found(noun: &str, value: &str) -> anyhow::Error {
            NboxError::NotFound(format!("no {noun} matched \"{value}\"")).into()
        }
    }
}
