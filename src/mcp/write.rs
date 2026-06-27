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

use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;
use rmcp::schemars;
use serde::Deserialize;

use crate::domain::detail;
use crate::netbox::client::NetBoxClient;
use crate::netbox::mutation::{MutationPlan, MutationReceipt};

use super::NboxMcp;

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
    /// The full `MutationPlan` JSON returned by `nbox_plan_write`. The plan's
    /// `confirm_token` is verified against its own contents — a tampered plan
    /// is rejected.
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
        let client = self.bridged_client(caller)?;
        let profile = self.profile.as_str();
        let plan = match args.operation {
            WriteOperation::InterfaceDescription {
                device,
                interface,
                description,
            } => {
                detail::plan_interface_description_update(
                    &client,
                    &device,
                    &interface,
                    &description,
                    None,
                    profile,
                    &not_found,
                )
                .await
            }
            WriteOperation::DeviceStatus { device, status } => {
                detail::plan_device_status_update(
                    &client, &device, &status, None, profile, &not_found,
                )
                .await
            }
            WriteOperation::IpReserve {
                prefix,
                vrf,
                description,
                dns_name,
                count,
            } => {
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
                .await
            }
            WriteOperation::PrefixReserve {
                prefix,
                vrf,
                length,
                description,
            } => {
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
                .await
            }
            WriteOperation::IpRangeReserve {
                range,
                description,
                dns_name,
                count,
            } => {
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
                .await
            }
            WriteOperation::TagAdd {
                object_type,
                object_ref,
                tag,
            } => {
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
                .await
            }
            WriteOperation::TagRemove {
                object_type,
                object_ref,
                tag,
            } => {
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
                .await
            }
        }
        .map_err(super::to_mcp_error)?;
        Ok(Json(plan))
    }

    /// Apply a previously planned write. Verifies the confirm token, then
    /// dispatches to the matching `apply_*` function.
    pub(crate) async fn apply_write_impl(
        &self,
        args: ApplyWriteArgs,
        caller: Option<WriteCaller>,
    ) -> Result<Json<MutationReceipt>, ErrorData> {
        args.plan
            .verify()
            .map_err(|e| super::to_mcp_error(e.into()))?;

        let client = self.bridged_client(caller)?;
        let receipt = match args.plan.operation {
            crate::netbox::mutation::Operation::Update => match args.plan.target.kind.as_str() {
                "interface" => {
                    detail::apply_interface_description_update(&client, &args.plan).await
                }
                "device" => detail::apply_device_status_update(&client, &args.plan).await,
                "tag" => detail::apply_tag_update(&client, &args.plan).await,
                other => {
                    return Err(ErrorData::invalid_params(
                        format!("unknown update target kind \"{other}\""),
                        None,
                    ));
                }
            },
            crate::netbox::mutation::Operation::Allocate => match args.plan.target.kind.as_str() {
                "ip" => detail::apply_ip_reserve(&client, &args.plan).await,
                "prefix" => detail::apply_prefix_reserve(&client, &args.plan).await,
                "ip-range" => detail::apply_ip_range_reserve(&client, &args.plan).await,
                other => {
                    return Err(ErrorData::invalid_params(
                        format!("unknown allocate target kind \"{other}\""),
                        None,
                    ));
                }
            },
        }
        .map_err(super::to_mcp_error)?;
        Ok(Json(receipt))
    }
}
