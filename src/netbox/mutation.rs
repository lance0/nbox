//! The shared mutation contract (ADR-0001).
//!
//! Every write builds a [`MutationPlan`] first; the plan is the common contract
//! for CLI, TUI, and future MCP writes. Domain view models are NOT write
//! payloads: write code builds an intent, derives a minimal `PATCH` from the
//! live NetBox object, and fails closed for any field it cannot shape safely.
//!
//! What lives here:
//! - the stable JSON surfaces ([`MutationPlan`] / [`MutationReceipt`]) with
//!   `schema_version`;
//! - the target identity ([`PlanTarget`]) and optimistic-concurrency
//!   [`Precondition`] (4.6+ `ETag`/`If-Match`, else `last_updated` + before-hash);
//! - the redacted, stable field diff ([`FieldChange`]) — scoped fields only,
//!   never the full object;
//! - the opaque confirmation token, derived from the target + precondition +
//!   patch + profile + expiry (a guard, NOT an authorization credential);
//! - the [`changelog_message`](CHANGELOG_MESSAGE_MAX) length guard.
//!
//! What does NOT live here: the network. Planning reads the live object via the
//! [`NetBoxClient`](crate::netbox::client::NetBoxClient) (command-specific code),
//! and applying sends the `PATCH` via `NetBoxClient::patch`. This module is the
//! pure contract + the redaction/token helpers so CLI, TUI, and MCP share one
//! shape. Audit redaction (field NAMES only, never values/tokens/objects/the
//! message body) is enforced by [`write_audit`](super::write_audit).

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::error::NboxError;

/// Version of the `MutationPlan` / `MutationReceipt` JSON schema. Bump on
/// incompatible shape changes; golden tests cover it from the first write.
pub const PLAN_SCHEMA_VERSION: u32 = 1;

/// Default plan validity window: a plan may be applied for this long after it
/// is built. Bound into the confirmation token so a plan cannot be silently
/// replayed past its expiry. Generous for a human review pause; short enough
/// that a stale plan (the object likely changed underneath it) is refused on
/// principle. ADR-0001 §5.
pub const DEFAULT_PLAN_TTL: Duration = Duration::from_secs(5 * 60);

/// NetBox's `changelog_message` length limit (a server-side constraint). nbox
/// validates it BEFORE applying so an over-length message is a usage error
/// (exit 2), not a rejected write (exit 1). ADR-0001 §8.
pub const CHANGELOG_MESSAGE_MAX: usize = 200;

/// The operation a plan performs. `update` (`PATCH`) and `allocate` (`POST` to a
/// server allocation endpoint, e.g. a prefix's `available-ips`) are the v1 verbs;
/// create/delete are reserved for later releases (ADR-0001 §1, §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    /// A partial update (`PATCH` against the object detail endpoint).
    Update,
    /// A server-side allocation (`POST` to an allocation endpoint that hands out
    /// the next free resource, e.g. `…/prefixes/{id}/available-ips/`). Creates a
    /// new object; the server picks the value and guards against races.
    Allocate,
}

impl Operation {
    /// The stable string for audit/log wording.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Operation::Update => "update",
            Operation::Allocate => "allocate",
        }
    }

    /// The HTTP method the apply uses for this operation — recorded in the audit
    /// event (`PATCH` for an update, `POST` for an allocation).
    #[must_use]
    pub fn http_method(self) -> &'static str {
        match self {
            Operation::Update => "PATCH",
            Operation::Allocate => "POST",
        }
    }
}

/// Identity of the write target, captured so the plan is self-describing and
/// the audit event can record it without re-deriving.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlanTarget {
    /// Object kind (e.g. `interface`).
    pub kind: String,
    /// The user-facing reference (e.g. `edge01/xe-0/0/1`).
    #[serde(rename = "ref")]
    pub r#ref: String,
    /// The resolved NetBox object id.
    pub id: u64,
    /// A human-friendly label (NetBox `display` when present, else the ref).
    pub display: String,
    /// The REST detail endpoint patched on apply (e.g. `/api/dcim/interfaces/42/`).
    pub endpoint: String,
    /// The active profile name, bound into the confirmation token.
    pub profile: String,
}

/// The optimistic-concurrency precondition recorded at plan time and checked at
/// apply (ADR-0001 §3). An update on 4.6+ carries an `ETag` (sent as `If-Match`;
/// a stale object yields `412`); older releases fall back to `last_updated` plus
/// a normalized before-hash, checked by a read-before-write at apply time. An
/// allocation has no client precondition — the server endpoint is race-safe.
/// Exactly one variant is populated; it is folded into the confirmation token.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Precondition {
    /// NetBox 4.6+ returns an `ETag` on the detail response; the apply sends
    /// `If-Match: <etag>` and a stale object yields `412 Precondition Failed`.
    Etag { etag: String },
    /// Pre-4.6 fallback: the object's `last_updated` timestamp plus a
    /// before-hash over the in-scope fields. At apply, nbox re-reads the object
    /// and refuses if either changed — a conservative read-before-write check.
    LastUpdated {
        last_updated: Option<String>,
        before_hash: String,
    },
    /// No client-side precondition. Used by `allocate`: the server allocation
    /// endpoint (e.g. `available-ips`) is inherently race-safe — NetBox never
    /// hands out the same resource twice — so there is no prior object to guard.
    None,
}

/// One field in scope for the change, with its before/after values for review.
/// This IS the redacted, stable field diff: only the scoped fields appear, in
/// declaration order — never the full object, never unrelated fields
/// (ADR-0001 §1, §8). Values are the shaped values the operator reviews, not
/// raw wire JSON.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FieldChange {
    pub field: String,
    pub before: Value,
    pub after: Value,
}

/// A validated, ready-to-review write plan (ADR-0001 §1). Built from a live
/// object + an intent; applied only after explicit confirmation.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MutationPlan {
    pub schema_version: u32,
    pub operation: Operation,
    pub target: PlanTarget,
    pub precondition: Precondition,
    /// The scoped field changes (the reviewable diff).
    pub fields: Vec<FieldChange>,
    /// The minimal REST `PATCH` body for the object fields in scope. Empty
    /// (`{}`) when [`no_op`](Self::no_op) — applying a no-op sends no `PATCH`.
    pub patch: Value,
    /// True when the patch body is empty — the current values already match.
    #[serde(default)]
    pub no_op: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Hard validation failures that block the write (the planner fails closed).
    /// A plan with errors must not be applied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    /// Opt-in NetBox changelog message, validated to ≤
    /// [`CHANGELOG_MESSAGE_MAX`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog_message: Option<String>,
    /// How many resources to allocate. `1` for a single allocation (the
    /// default — omitted from JSON so existing plans are byte-identical).
    /// `>1` triggers N sequential POSTs at apply time. Bound into the
    /// confirmation token so a `count=3` plan cannot be replayed as `count=5`.
    #[serde(default = "default_count", skip_serializing_if = "is_default_count")]
    pub count: u32,
    /// Opaque guard that the caller is applying the same scoped plan it
    /// reviewed — NOT an authorization credential (ADR-0001 §5).
    pub confirm_token: String,
    /// When the plan expires (ISO 8601 UTC, `YYYY-MM-DDTHH:MM:SSZ`). Apply
    /// refuses past this time.
    pub expires_at: String,
}

impl MutationPlan {
    /// Verify the plan's confirmation token recomputes from its own fields and
    /// the wall clock is still within the expiry window. A plan whose token does
    /// not match was tampered with or rebuilt; a plan past expiry is stale.
    /// Called at the top of apply so a future multi-step flow (TUI/MCP) that
    /// carries a plan across a boundary cannot apply a different or expired one.
    /// For one-shot `--confirm`, plan and apply share one process so this is
    /// trivially consistent — the check exists so the contract is real, not
    /// ceremonial.
    pub fn verify(&self) -> Result<(), NboxError> {
        let epoch = parse_iso_utc_to_epoch(&self.expires_at).ok_or_else(|| {
            NboxError::Usage(format!(
                "plan has an unparseable expires_at \"{}\"; re-run dry-run",
                self.expires_at
            ))
        })?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if now > epoch {
            // Past the validity window: the plan aged out before apply (only
            // reachable for a future multi-step flow that carries a plan across
            // a boundary — a one-shot `--confirm` plans and applies in one go).
            return Err(NboxError::StalePrecondition(
                " (the plan expired)".to_string(),
            ));
        }
        let expected = confirm_token(
            &self.target,
            self.operation,
            &self.precondition,
            &self.patch,
            &self.changelog_message,
            self.count,
            epoch,
        );
        if expected != self.confirm_token {
            return Err(NboxError::Usage(
                "plan confirmation token does not match its contents; re-run dry-run".to_string(),
            ));
        }
        Ok(())
    }

    /// The changed field NAMES (for the audit allow-list — never the values).
    /// ADR-0001 §8: the audit event logs field names, not values.
    #[must_use]
    pub fn changed_field_names(&self) -> Vec<&str> {
        self.fields.iter().map(|f| f.field.as_str()).collect()
    }
}

/// The result of applying a plan (ADR-0001 §5 step 6): re-fetch after success
/// and emit a receipt. Stable JSON surface for scripts (`--json --confirm`).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MutationReceipt {
    pub schema_version: u32,
    pub operation: Operation,
    pub target: PlanTarget,
    /// The field changes that were applied (before/after as reviewed).
    pub fields: Vec<FieldChange>,
    /// True when the `PATCH` was sent and NetBox accepted it.
    pub applied: bool,
    /// True when the plan was a no-op (no `PATCH` sent).
    #[serde(default)]
    pub no_op: bool,
    /// The HTTP status of the `PATCH` (`0` when no request was made — no-op).
    pub status: u16,
    /// The new `ETag` when NetBox returned one on the `PATCH` response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// The NetBox object-change request id, when a receipt lookup finds one
    /// (deferred in v1 — the field exists per the audit allow-list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// The affected object's JSON view, when the operation produced one to return
    /// — i.e. an `allocate` carries the **created** object (the reserved IP's
    /// view) so scripts get its address/id/status without a follow-up read. An
    /// `update` omits it (the field diff is the result), so existing update
    /// receipts are byte-identical and `schema_version` stays 1.
    /// A multi-IP allocation (`count > 1`) carries a JSON **array** of the
    /// created IP views here; a single allocation carries the one created view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<Value>,
    /// The human-readable outcome line per ADR-0001 §8.
    pub message: String,
}

// --- confirmation token -----------------------------------------------------

/// Derive the opaque confirmation token for a plan. Bound to the target,
/// operation, precondition, minimal patch, changelog message, allocation count,
/// profile, and the plan's expiry epoch so the same plan reapplied within its
/// window verifies, but a different plan (or one past expiry) does not. SHA-256
/// over canonical (sorted-key) JSON. This is a guard, not a MAC: it carries no
/// secret — the bound inputs are all already visible in the plan (ADR-0001 §5).
///
/// `operation`, `changelog_message`, and `count` are bound so a future
/// multi-step flow (TUI/MCP) that carries a plan across a boundary cannot
/// apply a plan whose reviewed message, operation, or allocation count was
/// altered between review and apply. In the one-shot `--confirm` CLI flow plan
/// and apply share one process, so this is defense-in-depth, not a live fix.
#[must_use]
pub fn confirm_token(
    target: &PlanTarget,
    operation: Operation,
    precondition: &Precondition,
    patch: &Value,
    changelog_message: &Option<String>,
    count: u32,
    expires_epoch: u64,
) -> String {
    let mut map = BTreeMap::new();
    map.insert("kind".to_string(), Value::String(target.kind.clone()));
    map.insert("id".to_string(), serde_json::json!(target.id));
    map.insert(
        "endpoint".to_string(),
        Value::String(target.endpoint.clone()),
    );
    map.insert("profile".to_string(), Value::String(target.profile.clone()));
    map.insert("operation".to_string(), serde_json::json!(operation));
    map.insert("precondition".to_string(), precondition.to_canonical());
    map.insert("patch".to_string(), patch.clone());
    map.insert(
        "changelog_message".to_string(),
        serde_json::to_value(changelog_message).unwrap_or(Value::Null),
    );
    map.insert("count".to_string(), serde_json::json!(count));
    map.insert(
        "expires_epoch".to_string(),
        serde_json::json!(expires_epoch),
    );
    let canonical = serde_json::to_value(map).unwrap_or(Value::Null);
    hex_sha256(&canonical.to_string())
}

// --- before-hash (pre-4.6 precondition) ------------------------------------

/// Normalized before-hash over a map of in-scope field → current value:
/// SHA-256 of the canonical (sorted-key) JSON. Used as the pre-4.6
/// optimistic-concurrency precondition alongside `last_updated` (ADR-0001 §3).
/// The apply step rebuilds the same map from a fresh read and refuses on
/// mismatch — so a concurrent writer is caught even in the absence of `ETag`.
#[must_use]
pub fn before_hash(fields: &BTreeMap<String, Value>) -> String {
    let canonical = serde_json::to_value(fields.clone()).unwrap_or(Value::Null);
    hex_sha256(&canonical.to_string())
}

// --- changelog_message guard -----------------------------------------------

/// Validate an opt-in `changelog_message` against NetBox's length limit before
/// applying. Over-length is a usage error (exit 2), not a rejected write
/// (exit 1) — the caller should not reach NetBox with input the server will
/// reject. ADR-0001 §8.
pub fn validate_changelog_message(msg: &Option<String>) -> Result<(), NboxError> {
    if let Some(m) = msg {
        let len = m.chars().count();
        if len > CHANGELOG_MESSAGE_MAX {
            return Err(NboxError::Usage(format!(
                "changelog_message exceeds NetBox's {CHANGELOG_MESSAGE_MAX}-character limit (got {len} chars)"
            )));
        }
    }
    Ok(())
}

// --- count helpers (multi-IP allocation) -----------------------------------

/// Default allocation count: 1 (a single allocation). Plans with `count == 1`
/// omit the field from JSON so existing single-IP plans are byte-identical.
#[must_use]
pub fn default_count() -> u32 {
    1
}

/// True when the count is the default (1) — used by `skip_serializing_if` so
/// single-allocation plans omit the field entirely.
#[must_use]
pub fn is_default_count(count: &u32) -> bool {
    *count == 1
}

// --- time helpers ----------------------------------------------------------

/// The expiry epoch (Unix seconds) for a plan built `now`, with the default TTL.
#[must_use]
pub fn plan_expiry_epoch(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() + DEFAULT_PLAN_TTL.as_secs())
}

/// Format a Unix epoch (seconds, UTC) as the canonical ISO 8601 string
/// `YYYY-MM-DDTHH:MM:SSZ` used for `expires_at`. The format is fixed and
/// round-trips through [`parse_iso_utc_to_epoch`].
#[must_use]
pub fn format_iso_utc(epoch: u64) -> String {
    let epoch = i64::try_from(epoch).expect("plan expiry epoch fits in i64");
    OffsetDateTime::from_unix_timestamp(epoch)
        .expect("plan expiry epoch is within OffsetDateTime range")
        .format(&Rfc3339)
        .expect("RFC3339 formatting succeeds for OffsetDateTime")
}

/// Parse the canonical ISO 8601 UTC form `YYYY-MM-DDTHH:MM:SSZ` (the only format
/// [`format_iso_utc`] emits) back to Unix epoch seconds. Returns `None` for any
/// other shape — the apply treats that as a usage error. A cheap fixed-shape
/// check preserves the plan contract while the `time` crate handles calendar
/// validation and epoch conversion.
#[must_use]
pub fn parse_iso_utc_to_epoch(s: &str) -> Option<u64> {
    if !is_canonical_iso_utc_shape(s) {
        return None;
    }
    let epoch = OffsetDateTime::parse(s, &Rfc3339).ok()?.unix_timestamp();
    u64::try_from(epoch).ok()
}

fn is_canonical_iso_utc_shape(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z'
        && bytes
            .iter()
            .enumerate()
            .all(|(idx, b)| matches!(idx, 4 | 7 | 10 | 13 | 16 | 19) || b.is_ascii_digit())
}

// --- hashing ---------------------------------------------------------------

fn hex_sha256(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

impl Precondition {
    /// Canonical JSON for the confirmation token. Deterministic for a given
    /// variant (serde's tag + field order is stable), and the token wraps the
    /// whole input in sorted keys anyway.
    fn to_canonical(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn target() -> PlanTarget {
        PlanTarget {
            kind: "interface".into(),
            r#ref: "edge01/xe-0/0/1".into(),
            id: 42,
            display: "xe-0/0/1".into(),
            endpoint: "/api/dcim/interfaces/42/".into(),
            profile: "default".into(),
        }
    }

    #[test]
    fn confirm_token_is_stable_for_same_inputs() {
        let t = target();
        let p = Precondition::Etag {
            etag: "\"abc\"".into(),
        };
        let patch = json!({"description": "up"});
        let a = confirm_token(&t, Operation::Update, &p, &patch, &None, 1, 1_000);
        let b = confirm_token(&t, Operation::Update, &p, &patch, &None, 1, 1_000);
        assert_eq!(a, b, "same inputs → same token");
        assert_eq!(a.len(), 64, "full SHA-256 hex");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn confirm_token_changes_with_any_bound_input() {
        let t = target();
        let p = Precondition::Etag {
            etag: "\"abc\"".into(),
        };
        let patch = json!({"description": "up"});
        let base = confirm_token(&t, Operation::Update, &p, &patch, &None, 1, 1_000);
        // Different patch → different token.
        assert_ne!(
            base,
            confirm_token(
                &t,
                Operation::Update,
                &p,
                &json!({"description": "down"}),
                &None,
                1,
                1_000
            )
        );
        // Different id (target) → different token.
        let mut t2 = t.clone();
        t2.id = 43;
        assert_ne!(
            base,
            confirm_token(&t2, Operation::Update, &p, &patch, &None, 1, 1_000)
        );
        // Different precondition → different token.
        assert_ne!(
            base,
            confirm_token(
                &t,
                Operation::Update,
                &Precondition::Etag {
                    etag: "\"xyz\"".into()
                },
                &patch,
                &None,
                1,
                1_000
            )
        );
        // Different expiry → different token.
        assert_ne!(
            base,
            confirm_token(&t, Operation::Update, &p, &patch, &None, 1, 2_000)
        );
        // Different profile → different token.
        let mut t3 = t.clone();
        t3.profile = "work".into();
        assert_ne!(
            base,
            confirm_token(&t3, Operation::Update, &p, &patch, &None, 1, 1_000)
        );
        // Different operation → different token.
        assert_ne!(
            base,
            confirm_token(&t, Operation::Allocate, &p, &patch, &None, 1, 1_000)
        );
        // Different changelog message → different token.
        assert_ne!(
            base,
            confirm_token(
                &t,
                Operation::Update,
                &p,
                &patch,
                &Some("reviewed change".into()),
                1,
                1_000
            )
        );
    }

    #[test]
    fn before_hash_is_stable_and_order_independent() {
        let mut a = BTreeMap::new();
        a.insert("description".into(), json!("old"));
        let mut b = BTreeMap::new();
        b.insert("description".into(), json!("old"));
        assert_eq!(before_hash(&a), before_hash(&b));
        // A different value changes the hash.
        let mut c = BTreeMap::new();
        c.insert("description".into(), json!("new"));
        assert_ne!(before_hash(&a), before_hash(&c));
    }

    #[test]
    fn changelog_message_limit_is_char_counted() {
        validate_changelog_message(&None).expect("None is fine");
        validate_changelog_message(&Some("ok".into())).expect("short is fine");
        let exactly = "x".repeat(CHANGELOG_MESSAGE_MAX);
        validate_changelog_message(&Some(exactly)).expect("exactly the limit is fine");
        let over = "x".repeat(CHANGELOG_MESSAGE_MAX + 1);
        let err = validate_changelog_message(&Some(over)).unwrap_err();
        assert_eq!(err.exit_code(), 2, "over-length is a usage error");
        assert!(format!("{err}").contains("200-character limit"));
    }

    #[test]
    fn operation_allocate_strings_and_http_method() {
        assert_eq!(Operation::Allocate.as_str(), "allocate");
        assert_eq!(Operation::Update.as_str(), "update");
        assert_eq!(Operation::Allocate.http_method(), "POST");
        assert_eq!(Operation::Update.http_method(), "PATCH");
        // The wire form (the stable JSON in plans/receipts) is lowercase.
        assert_eq!(
            serde_json::to_value(Operation::Allocate).unwrap(),
            json!("allocate")
        );
        assert_eq!(
            serde_json::from_value::<Operation>(json!("allocate")).unwrap(),
            Operation::Allocate
        );
    }

    #[test]
    fn precondition_none_round_trips_and_binds_token() {
        // `none` serializes as a tagged variant and reads back.
        let v = serde_json::to_value(Precondition::None).unwrap();
        assert_eq!(v, json!({"type": "none"}));
        assert!(matches!(
            serde_json::from_value::<Precondition>(json!({"type": "none"})).unwrap(),
            Precondition::None
        ));
        // It folds into the confirmation token distinctly from the other variants,
        // so an allocate plan's token can't collide with an update plan's.
        let t = target();
        let patch = json!({});
        let none_tok = confirm_token(
            &t,
            Operation::Update,
            &Precondition::None,
            &patch,
            &None,
            1,
            1_000,
        );
        let etag_tok = confirm_token(
            &t,
            Operation::Update,
            &Precondition::Etag {
                etag: "\"abc\"".into(),
            },
            &patch,
            &None,
            1,
            1_000,
        );
        assert_ne!(none_tok, etag_tok);
        // Stable for the same inputs.
        assert_eq!(
            none_tok,
            confirm_token(
                &t,
                Operation::Update,
                &Precondition::None,
                &patch,
                &None,
                1,
                1_000
            )
        );
    }

    #[test]
    fn receipt_object_is_omitted_for_update_present_for_allocate() {
        let base = MutationReceipt {
            schema_version: PLAN_SCHEMA_VERSION,
            operation: Operation::Update,
            target: target(),
            fields: vec![],
            applied: true,
            no_op: false,
            status: 200,
            etag: None,
            request_id: None,
            object: None,
            message: "applied".into(),
        };
        // An update receipt has no `object` key (byte-identical to pre-allocate).
        let v = serde_json::to_value(&base).unwrap();
        assert!(
            v.get("object").is_none(),
            "update receipt omits `object`: {v}"
        );
        // An allocate receipt carries the created object.
        let alloc = MutationReceipt {
            operation: Operation::Allocate,
            status: 201,
            object: Some(json!({"address": "203.0.113.7/24", "status": "active"})),
            message: "reserved".into(),
            ..base
        };
        let v = serde_json::to_value(&alloc).unwrap();
        assert_eq!(v["object"]["address"], json!("203.0.113.7/24"));
        // `schema_version` stays 1 across both shapes (purely additive field).
        assert_eq!(v["schema_version"], json!(1));
    }

    #[test]
    fn iso_format_round_trips() {
        // The formatter emits canonical ISO; the parser reads exactly that back.
        for epoch in [
            0u64,
            1,
            1_735_689_600,
            1_900_000_000,
            86_400,
            1_483_228_800,
            1_709_164_800,
            1_750_945_509,
        ] {
            let s = format_iso_utc(epoch);
            assert_eq!(
                parse_iso_utc_to_epoch(&s),
                Some(epoch),
                "round-trip {epoch}: {s}"
            );
        }
    }

    #[test]
    fn iso_parser_rejects_other_shapes() {
        assert_eq!(parse_iso_utc_to_epoch("not a date"), None);
        assert_eq!(parse_iso_utc_to_epoch("2026-06-26"), None);
        assert_eq!(parse_iso_utc_to_epoch("2026-06-26T12:00:00"), None); // missing Z
        assert_eq!(parse_iso_utc_to_epoch("2026-06-26T12:00:00.123Z"), None); // fractional
        assert_eq!(parse_iso_utc_to_epoch("2026-13-26T12:00:00Z"), None); // bad month
        assert_eq!(parse_iso_utc_to_epoch("2026-06-31T12:00:00Z"), None); // bad day (30 in june)
        assert_eq!(parse_iso_utc_to_epoch("2025-02-29T12:00:00Z"), None); // bad day (28 in non-leap feb)
        assert_eq!(parse_iso_utc_to_epoch("2024-02-30T12:00:00Z"), None); // bad day (29 in leap feb)
        // Leap day itself is valid and round-trips.
        assert_eq!(
            parse_iso_utc_to_epoch("2024-02-29T00:00:00Z"),
            Some(1_709_164_800)
        );
        assert_eq!(parse_iso_utc_to_epoch("2026-06-26T25:00:00Z"), None); // bad hour
    }

    #[test]
    fn iso_format_known_values() {
        assert_eq!(format_iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_iso_utc(1_483_228_800), "2017-01-01T00:00:00Z");
        assert_eq!(format_iso_utc(1_735_689_600), "2025-01-01T00:00:00Z");
        assert_eq!(format_iso_utc(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(format_iso_utc(1_750_945_509), "2025-06-26T13:45:09Z");
    }

    #[test]
    fn plan_verify_accepts_a_fresh_consistent_plan() {
        let t = target();
        let p = Precondition::Etag {
            etag: "\"abc\"".into(),
        };
        let patch = json!({"description": "up"});
        let now = SystemTime::now();
        let exp = plan_expiry_epoch(now);
        let plan = MutationPlan {
            schema_version: PLAN_SCHEMA_VERSION,
            operation: Operation::Update,
            target: t,
            precondition: p,
            fields: vec![FieldChange {
                field: "description".into(),
                before: json!("old"),
                after: json!("up"),
            }],
            patch,
            no_op: false,
            warnings: vec![],
            errors: vec![],
            changelog_message: None,
            count: 1,
            confirm_token: String::new(), // filled below
            expires_at: format_iso_utc(exp),
        };
        let token = confirm_token(
            &plan.target,
            Operation::Update,
            &plan.precondition,
            &plan.patch,
            &None,
            1,
            exp,
        );
        let mut plan = plan;
        plan.confirm_token = token;
        plan.verify().expect("fresh consistent plan verifies");
    }

    #[test]
    fn plan_verify_rejects_a_tampered_token() {
        let mut plan = plan_for_verify();
        plan.confirm_token = "00".repeat(32); // wrong token
        let err = plan.verify().unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(format!("{err}").contains("confirmation token does not match"));
    }

    #[test]
    fn plan_verify_rejects_an_expired_plan() {
        // Expiry in the past (epoch 1 → 1970).
        let mut plan = plan_for_verify_with_epoch(1);
        plan.confirm_token = confirm_token(
            &plan.target,
            Operation::Update,
            &plan.precondition,
            &plan.patch,
            &None,
            1,
            1,
        );
        let err = plan.verify().unwrap_err();
        assert_eq!(
            err.exit_code(),
            1,
            "expired is a stale-precondition refusal"
        );
    }

    #[test]
    fn changed_field_names_exposes_names_not_values() {
        let plan = plan_for_verify();
        assert_eq!(plan.changed_field_names(), vec!["description"]);
    }

    fn plan_for_verify() -> MutationPlan {
        plan_for_verify_with_epoch(plan_expiry_epoch(SystemTime::now()))
    }

    fn plan_for_verify_with_epoch(epoch: u64) -> MutationPlan {
        let t = target();
        let p = Precondition::Etag {
            etag: "\"abc\"".into(),
        };
        let patch = json!({"description": "up"});
        MutationPlan {
            schema_version: PLAN_SCHEMA_VERSION,
            operation: Operation::Update,
            target: t,
            precondition: p,
            fields: vec![FieldChange {
                field: "description".into(),
                before: json!("old"),
                after: json!("up"),
            }],
            patch,
            no_op: false,
            warnings: vec![],
            errors: vec![],
            changelog_message: None,
            count: 1,
            confirm_token: confirm_token(
                &target(),
                Operation::Update,
                &Precondition::Etag {
                    etag: "\"abc\"".into(),
                },
                &json!({"description": "up"}),
                &None,
                1,
                epoch,
            ),
            expires_at: format_iso_utc(epoch),
        }
    }
}
