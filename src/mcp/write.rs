//! MCP write tools (Pattern 2, DESIGN §24) — plan-first, per-user identity.
//!
//! Two operation-specific tools mirror the CLI's two-step safe-write flow:
//!
//! 1. `nbox_plan_write` — builds a [`MutationPlan`] (the reviewable diff +
//!    confirm token) without mutating. The agent reviews the plan, then calls
//!    `nbox_apply_write` with it.
//! 2. `nbox_apply_write` — verifies the plan's confirm token and applies it,
//!    returning a [`MutationReceipt`].
//!
//! Per-user identity bridging: the caller's OIDC `sub` is resolved to a
//! per-user NetBox token via [`crate::mcp::vault::CredentialVault`], then
//! bridged into a temporary [`NetBoxClient`] via [`NetBoxClient::with_token`]
//! so the write hits NetBox under the caller's identity.
//!
//! The tools reuse the exact same `plan_*`/`apply_*` engine the CLI uses
//! (ADR-0001) — no separate write path. The vault is the only new layer.

use std::collections::HashMap;

use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;
use rmcp::schemars;
use serde::Deserialize;

use crate::domain::detail;
use crate::netbox::client::NetBoxClient;
use crate::netbox::mutation::{MutationPlan, MutationReceipt};
use crate::netbox::write_audit;

use super::NboxMcp;

// ---------------------------------------------------------------------------
// Server-issued plan store (apply integrity)
// ---------------------------------------------------------------------------

/// Which `apply_*` function realizes a stored plan. Recorded when the plan is
/// issued so apply dispatches on the *operation that produced it*, not on
/// `target.kind`. A tag write's `target.kind` is the tagged object's kind
/// (`device`, `prefix`, …), so dispatching on it would misroute a device/
/// interface tag write to the status/description applier and fail every other
/// kind outright.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApplierKind {
    InterfaceDescription,
    DeviceStatus,
    IpReserve,
    PrefixReserve,
    IpRangeReserve,
    Tag,
}

/// Server-issued write plans awaiting `nbox_apply_write`.
///
/// A plan's `confirm_token` is a non-secret SHA over the plan's own fields
/// ([`crate::netbox::mutation::confirm_token`]), so `plan.verify()` on a
/// caller-supplied plan proves only self-consistency — a write-scoped caller
/// could forge any `target.endpoint` / `patch` and compute a matching token,
/// escaping nbox's narrow write surface. This store closes that hole:
/// `nbox_plan_write` records every plan it issues (keyed by `confirm_token`,
/// with the planner's OIDC `sub` and the applier), and `nbox_apply_write`
/// applies the STORED plan — never the caller's contents, which are used only as
/// the lookup key. Bounded by capacity; plans also carry their own expiry,
/// re-checked at apply.
#[derive(Default)]
pub(crate) struct PlanStore {
    issued: HashMap<String, StoredPlan>,
    seq: u64,
}

struct StoredPlan {
    plan: MutationPlan,
    /// The OIDC `sub` that planned it — apply must come from the same caller.
    sub: String,
    applier: ApplierKind,
    /// Monotonic issue order, for capacity eviction (oldest first).
    seq: u64,
}

/// Why a plan lookup failed (mapped to distinct caller-facing errors).
pub(crate) enum ConsumeError {
    NotFound,
    WrongCaller,
}

/// Upper bound on outstanding (issued-but-unapplied) plans. Generous for real
/// agent use (plan→apply is near-immediate); the oldest is evicted past it so a
/// stream of plan calls can never grow memory unbounded.
const PLAN_STORE_CAP: usize = 256;

impl PlanStore {
    /// Record a freshly issued plan, evicting the oldest when at capacity.
    pub(crate) fn record(&mut self, plan: MutationPlan, sub: String, applier: ApplierKind) {
        if self.issued.len() >= PLAN_STORE_CAP
            && !self.issued.contains_key(&plan.confirm_token)
            && let Some(oldest) = self
                .issued
                .iter()
                .min_by_key(|(_, s)| s.seq)
                .map(|(k, _)| k.clone())
        {
            self.issued.remove(&oldest);
        }
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        self.issued.insert(
            plan.confirm_token.clone(),
            StoredPlan {
                plan,
                sub,
                applier,
                seq,
            },
        );
    }

    /// Consume the plan this server issued for `token`, requiring the same caller
    /// `sub`. One-shot: a matched plan is removed (no replay). The caller's own
    /// plan contents are never trusted — only its token keys the lookup.
    pub(crate) fn consume(
        &mut self,
        token: &str,
        sub: &str,
    ) -> Result<(MutationPlan, ApplierKind), ConsumeError> {
        match self.issued.get(token) {
            None => Err(ConsumeError::NotFound),
            // A mismatched caller must not consume (the rightful caller can still
            // apply) — reject without removing.
            Some(s) if s.sub != sub => Err(ConsumeError::WrongCaller),
            Some(_) => {
                let s = self.issued.remove(token).expect("just checked present");
                Ok((s.plan, s.applier))
            }
        }
    }
}

impl ConsumeError {
    fn into_mcp(self) -> ErrorData {
        match self {
            ConsumeError::NotFound => ErrorData::invalid_params(
                "no write plan matches this confirm_token on this server — it was never issued \
                 here, was already applied, or aged out. Call nbox_plan_write again and apply the \
                 plan it returns.",
                None,
            ),
            ConsumeError::WrongCaller => ErrorData::invalid_params(
                "this write plan was issued for a different caller identity; re-plan with \
                 nbox_plan_write",
                None,
            ),
        }
    }
}

/// Arguments for `nbox_plan_write`. The `operation` field selects the write
/// kind; the remaining fields are the operation's parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PlanWriteArgs {
    /// The write operation to plan.
    pub operation: WriteOperation,
}

/// Which write operation to plan. Each variant carries the operation's
/// parameters — the same parameters the CLI's `--dry-run` path accepts.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WriteOperation {
    /// Set an interface's description (a `PATCH`).
    InterfaceDescription {
        /// The device name or slug.
        device: String,
        /// The interface name (verbatim — names may contain slashes).
        interface: String,
        /// The new description. Empty string clears it.
        description: String,
    },
    /// Set a device's status (a `PATCH`).
    DeviceStatus {
        /// The device name, slug, or ID.
        device: String,
        /// The new status (validated live from NetBox's choices).
        status: String,
    },
    /// Reserve the next available IP in a prefix (an `allocate` POST).
    IpReserve {
        /// The parent prefix CIDR (e.g. `10.0.0.0/24`).
        prefix: String,
        /// Optional VRF reference (name, RD, or ID) to scope the prefix.
        #[serde(default)]
        vrf: Option<String>,
        /// Optional description for the new IP.
        #[serde(default)]
        description: Option<String>,
        /// Optional DNS name for the new IP.
        #[serde(default)]
        dns_name: Option<String>,
        /// How many IPs to reserve (default 1).
        #[serde(default)]
        count: Option<u32>,
    },
    /// Reserve the next available child prefix (an `allocate` POST).
    PrefixReserve {
        /// The parent prefix CIDR.
        prefix: String,
        /// Optional VRF reference.
        #[serde(default)]
        vrf: Option<String>,
        /// Request a specific child prefix length (e.g. 26 for a /26).
        #[serde(default)]
        length: Option<u8>,
        /// Optional description for the new prefix.
        #[serde(default)]
        description: Option<String>,
    },
    /// Reserve the next available IP in an IP range (an `allocate` POST).
    IpRangeReserve {
        /// The IP range start address or ID.
        range: String,
        /// Optional description for the new IP.
        #[serde(default)]
        description: Option<String>,
        /// Optional DNS name for the new IP.
        #[serde(default)]
        dns_name: Option<String>,
        /// How many IPs to reserve (default 1).
        #[serde(default)]
        count: Option<u32>,
    },
    /// Add a tag to an object (a `PATCH` to the `tags` array).
    TagAdd {
        /// The object type (same kinds as `nbox_get`: device, ip, prefix, …).
        object_type: String,
        /// The object reference (name/slug/ID; CIDR for prefix; address for ip).
        object_ref: String,
        /// The tag to add (id, name, or slug).
        tag: String,
    },
    /// Remove a tag from an object (a `PATCH` to the `tags` array).
    TagRemove {
        /// The object type.
        object_type: String,
        /// The object reference.
        object_ref: String,
        /// The tag to remove (id, name, or slug).
        tag: String,
    },
}

/// Arguments for `nbox_apply_write`. The agent passes back the exact plan JSON
/// it received from `nbox_plan_write`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ApplyWriteArgs {
    /// The `MutationPlan` returned by `nbox_plan_write`. Only its `confirm_token`
    /// is used — to look up the plan this server issued and stored at plan time;
    /// that stored plan is what executes. The rest of the JSON is not trusted, so
    /// a forged or edited plan has no matching stored entry and is rejected.
    pub plan: MutationPlan,
}

/// A friendly "not found" error for MCP, mirroring the CLI's actionable message.
fn not_found(noun: &str, value: &str) -> anyhow::Error {
    anyhow::anyhow!("no {noun} matched \"{value}\"; use nbox_search to find the right reference")
}

/// The caller authorization facts the write path needs, extracted by the
/// transport from the validated request identity (OIDC `sub` + scopes over the
/// HTTP transport). Kept transport-agnostic — the write engine and its unit
/// tests depend on this, not on the HTTP-only `oidc::Identity` — so the
/// stdio/non-`http` build still compiles (it simply never produces one).
pub(crate) struct WriteCaller {
    /// The caller's OIDC `sub`, resolved to a per-user NetBox token by the vault.
    pub sub: String,
    /// Whether the caller's token carries the `nbox:write` scope (ADR-0001 §7).
    pub has_write_scope: bool,
}

impl NboxMcp {
    /// Resolve the caller's per-user NetBox client via the vault, or reject
    /// with a clear error if writes are disabled or the caller has no vault
    /// entry. Returns a short-lived `NetBoxClient` clone with the per-user
    /// token swapped in.
    /// Resolve the caller's per-user NetBox client, enforcing the full write
    /// authorization ladder — fail-closed at every step (ADR-0001 §7):
    ///
    /// 1. writes enabled at all (`[serve].allow_writes` → a vault is present);
    /// 2. the request carried an authenticated caller (HTTP+OIDC; stdio and
    ///    loopback static-bearer have no per-user identity → rejected);
    /// 3. the caller's token carries the `nbox:write` scope;
    /// 4. the caller's `sub` maps to a provisioned per-user NetBox token.
    ///
    /// The service token is never used for writes. A `None` caller (no identity)
    /// gets a distinct error that never suggests mapping a placeholder `sub`.
    fn bridged_client(&self, caller: Option<WriteCaller>) -> Result<NetBoxClient, ErrorData> {
        let vault = self.vault.as_ref().ok_or_else(|| {
            ErrorData::invalid_params(
                "MCP writes are not enabled on this nbox serve instance; \
                 set [serve].allow_writes = true or pass --allow-writes, \
                 and provision [serve.vault] entries for each caller's OIDC sub",
                None,
            )
        })?;
        let caller = caller.ok_or_else(|| {
            ErrorData::invalid_params(
                "MCP writes require an authenticated OIDC caller identity; this request \
                 carried none. Writes are unavailable over the stdio transport and over \
                 loopback static-bearer auth — use the HTTP transport with OIDC so each \
                 write is attributed to a real NetBox user.",
                None,
            )
        })?;
        if !caller.has_write_scope {
            return Err(ErrorData::invalid_params(
                format!(
                    "the caller's token is missing the required `{}` scope for MCP writes",
                    crate::mcp::SCOPE_WRITE
                ),
                None,
            ));
        }
        let token = vault
            .resolve(&caller.sub)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        Ok((*self.client)
            .clone()
            .with_token(token.as_str().to_string()))
    }

    /// Plan a write operation. Builds a `MutationPlan` without mutating.
    pub(crate) async fn plan_write_impl(
        &self,
        args: PlanWriteArgs,
        caller: Option<WriteCaller>,
    ) -> Result<Json<MutationPlan>, ErrorData> {
        // Capture the planner's identity before the caller is consumed by the
        // auth ladder — apply requires the same `sub` to apply this plan.
        let planner_sub = caller.as_ref().map(|c| c.sub.clone());
        let client = self.bridged_client(caller)?;
        let profile = self.profile.as_str();
        let (applier, plan_result) = match args.operation {
            WriteOperation::InterfaceDescription {
                device,
                interface,
                description,
            } => (
                ApplierKind::InterfaceDescription,
                detail::plan_interface_description_update(
                    &client,
                    &device,
                    &interface,
                    &description,
                    None,
                    profile,
                    &not_found,
                )
                .await,
            ),
            WriteOperation::DeviceStatus { device, status } => (
                ApplierKind::DeviceStatus,
                detail::plan_device_status_update(
                    &client, &device, &status, None, profile, &not_found,
                )
                .await,
            ),
            WriteOperation::IpReserve {
                prefix,
                vrf,
                description,
                dns_name,
                count,
            } => (
                ApplierKind::IpReserve,
                detail::plan_ip_reserve(
                    &client,
                    &prefix,
                    vrf.as_deref(),
                    description.as_deref(),
                    dns_name.as_deref(),
                    count.unwrap_or(1),
                    None,
                    profile,
                    &not_found,
                )
                .await,
            ),
            WriteOperation::PrefixReserve {
                prefix,
                vrf,
                length,
                description,
            } => (
                ApplierKind::PrefixReserve,
                detail::plan_prefix_reserve(
                    &client,
                    &prefix,
                    vrf.as_deref(),
                    length,
                    description.as_deref(),
                    None,
                    profile,
                    &not_found,
                )
                .await,
            ),
            WriteOperation::IpRangeReserve {
                range,
                description,
                dns_name,
                count,
            } => (
                ApplierKind::IpRangeReserve,
                detail::plan_ip_range_reserve(
                    &client,
                    &range,
                    description.as_deref(),
                    dns_name.as_deref(),
                    count.unwrap_or(1),
                    None,
                    profile,
                    &not_found,
                )
                .await,
            ),
            WriteOperation::TagAdd {
                object_type,
                object_ref,
                tag,
            } => (
                ApplierKind::Tag,
                detail::plan_tag_update(
                    &client,
                    detail::TagOperation::Add,
                    &object_type,
                    &object_ref,
                    &tag,
                    None,
                    profile,
                    &not_found,
                )
                .await,
            ),
            WriteOperation::TagRemove {
                object_type,
                object_ref,
                tag,
            } => (
                ApplierKind::Tag,
                detail::plan_tag_update(
                    &client,
                    detail::TagOperation::Remove,
                    &object_type,
                    &object_ref,
                    &tag,
                    None,
                    profile,
                    &not_found,
                )
                .await,
            ),
        };
        let plan = plan_result.map_err(super::to_mcp_error)?;
        // Record the server-issued plan so apply can trust it — the caller's
        // submitted plan contents are never used beyond the confirm_token.
        if let Some(sub) = planner_sub {
            self.plans
                .lock()
                .expect("plan store mutex poisoned")
                .record(plan.clone(), sub, applier);
        }
        Ok(Json(plan))
    }

    /// Apply a previously planned write. Looks up the plan THIS server issued for
    /// the submitted `confirm_token` (bound to the same caller) and applies that
    /// stored plan — the caller-supplied plan contents are never trusted, since
    /// the token is a non-secret hash a write-scoped caller could forge.
    pub(crate) async fn apply_write_impl(
        &self,
        args: ApplyWriteArgs,
        caller: Option<WriteCaller>,
    ) -> Result<Json<MutationReceipt>, ErrorData> {
        // Capture the caller's identity, then enforce the full write
        // authorization ladder (writes enabled, authenticated caller, write
        // scope, vault entry) and bind the per-user NetBox token.
        let caller_sub = caller.as_ref().map(|c| c.sub.clone());
        let client = self.bridged_client(caller)?;
        let sub = caller_sub.expect("bridged_client succeeded ⇒ caller present");

        // Apply the plan this server issued for the token — never the caller's
        // contents. A forged/tampered plan has no matching server-stored entry.
        let (plan, applier) = self
            .plans
            .lock()
            .expect("plan store mutex poisoned")
            .consume(&args.plan.confirm_token, &sub)
            .map_err(ConsumeError::into_mcp)?;

        // Defense in depth: the stored plan must still pass its own integrity +
        // expiry check.
        plan.verify().map_err(|e| super::to_mcp_error(e.into()))?;

        let started = write_audit::Started::now();
        let result = match applier {
            ApplierKind::InterfaceDescription => {
                detail::apply_interface_description_update(&client, &plan).await
            }
            ApplierKind::DeviceStatus => detail::apply_device_status_update(&client, &plan).await,
            ApplierKind::Tag => detail::apply_tag_update(&client, &plan).await,
            ApplierKind::IpReserve => detail::apply_ip_reserve(&client, &plan).await,
            ApplierKind::PrefixReserve => detail::apply_prefix_reserve(&client, &plan).await,
            ApplierKind::IpRangeReserve => detail::apply_ip_range_reserve(&client, &plan).await,
        };

        // ADR-0001 §8 write audit, attributed to this MCP caller — same
        // allow-list shape as the CLI (field names only, never values/token).
        self.emit_write_audit(&client, &plan, &result, started.elapsed_ms());

        let receipt = result.map_err(super::to_mcp_error)?;
        Ok(Json(receipt))
    }

    /// Emit the single ADR-0001 §8 write-audit event for an MCP apply (success
    /// or failure), reusing the CLI's allow-list shape with `surface = mcp`.
    fn emit_write_audit(
        &self,
        client: &NetBoxClient,
        plan: &MutationPlan,
        result: &anyhow::Result<MutationReceipt>,
        latency_ms: u128,
    ) {
        use write_audit::{Outcome, Surface};

        let host = crate::audit_origin(client.base_url());
        let (outcome, status) = match result {
            Ok(r) if r.no_op => (Outcome::NoOp, 0),
            Ok(r) => (Outcome::Applied, r.status),
            Err(e) => crate::classify_apply_error(e),
        };
        // A no-op makes no request — blank the method/path/status like the CLI.
        let (http_method, http_path) = if matches!(outcome, Outcome::NoOp) {
            ("", "")
        } else {
            (plan.operation.http_method(), plan.target.endpoint.as_str())
        };
        let field_names = plan.changed_field_names();
        write_audit::WriteAuditEvent {
            surface: Surface::Mcp,
            profile: self.profile.as_str(),
            host: &host,
            operation: plan.operation,
            target_kind: &plan.target.kind,
            target_id: plan.target.id,
            target_display: &plan.target.display,
            fields: &field_names,
            outcome,
            http_method,
            http_path,
            status,
            latency_ms,
            request_id: None,
            message_present: plan.changelog_message.is_some(),
            message_len: crate::message_audit_len(plan.changelog_message.as_deref()),
        }
        .emit();
    }
}
