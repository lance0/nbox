//! REST choice-field validation (ADR-0001 follow-on write surface).
//!
//! A *choice* field (e.g. a device's `status`) takes one value from a small,
//! server-defined set. Before nbox sends a `PATCH` touching one, it asks NetBox
//! which values are allowed and normalizes the operator's input to the canonical
//! value — so an unknown or ambiguous status is a usage error (exit 2) **before**
//! any `PATCH`, naming the input and listing the allowed canonical values.
//!
//! This is deliberately the *smallest* reusable mechanism for that: a pure
//! resolver over an enumerated option set, plus a pure extractor that pulls the
//! choices out of a NetBox `OPTIONS` response. The network (issuing `OPTIONS`)
//! lives in [`NetBoxClient`](super::client::NetBoxClient); this module is the
//! pure contract so the normalization policy is unit-testable with no I/O.
//!
//! It is NOT a generic schema-edit system: it validates one named choice field
//! against the choices NetBox exposes, nothing more. Writable-field discovery,
//! required-field checking, and relation shaping stay out of scope (ROADMAP).

use serde::Deserialize;

use crate::error::NboxError;

/// One enumerated value for a choice field, as NetBox reports it via `OPTIONS`.
/// `value` is the canonical wire value (what nbox sends in the `PATCH` body and
/// compares against the current state); `label` is the human form NetBox shows,
/// when the deployment exposes one (NetBox's `OPTIONS` uses `display`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    /// The canonical wire value (e.g. `active`).
    pub value: String,
    /// The human label (e.g. `Active`), when NetBox exposes one.
    pub label: Option<String>,
}

/// Normalize an operator's input against an enumerated choice set to the
/// canonical wire value. Pure (no I/O) so the policy is unit-tested directly.
///
/// Matching, in order (ADR-0001 §6 — accept canonical values; accept labels
/// case-insensitively when they map unambiguously to one value):
/// 1. an **exact** value match wins (the canonical form);
/// 2. otherwise, a case-insensitive **label** match that maps to exactly one
///    distinct value wins;
/// 3. otherwise the input is rejected — unknown (no match) or ambiguous (a
///    case-insensitive label matched more than one distinct value).
///
/// The error names the field, the input, and lists the allowed canonical values
/// (the `value`s, in the order NetBox returned them) so the message is
/// actionable. `field_label` is the human name of the field for the message
/// (e.g. `"status"`).
pub fn resolve_choice(
    options: &[ChoiceOption],
    field_label: &str,
    input: &str,
) -> Result<String, NboxError> {
    let allowed = allowed_values(options);

    // 1) exact canonical-value match.
    if let Some(opt) = options.iter().find(|o| o.value == input) {
        return Ok(opt.value.clone());
    }

    // 2) case-insensitive label match, collected to distinct values.
    let want = input.to_lowercase();
    let label_values: Vec<&str> = options
        .iter()
        .filter(|o| o.label.as_deref().is_some_and(|l| l.to_lowercase() == want))
        .map(|o| o.value.as_str())
        .collect();
    match label_values.len() {
        0 => Err(unknown_choice_error(field_label, input, &allowed)),
        1 => Ok(label_values[0].to_string()),
        // A case-insensitive label matched more than one distinct value — the
        // input is ambiguous. Treat like an unknown (still names the input +
        // lists allowed values); the message flags the ambiguity.
        _ => Err(NboxError::Usage(format!(
            "ambiguous {field_label} \"{input}\" (matches several values case-insensitively). \
             Allowed: {allowed}"
        ))),
    }
}

/// The allowed canonical values as a comma-separated list, in NetBox's order,
/// for the usage-error message.
fn allowed_values(options: &[ChoiceOption]) -> String {
    options
        .iter()
        .map(|o| o.value.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The unknown-choice usage error: names the field, the input, and the allowed
/// canonical values. Exit 2 (a usage error, before any `PATCH`).
fn unknown_choice_error(field_label: &str, input: &str, allowed: &str) -> NboxError {
    NboxError::Usage(format!(
        "invalid {field_label} \"{input}\"; allowed values: {allowed}"
    ))
}

/// One choice as NetBox serializes it in the `OPTIONS` choices array. The
/// canonical shape is `{"value": "active", "display": "Active"}`; `display` is
/// tolerated as absent (some fields/deployments expose value only) and `label`
/// is accepted as a fallback for the human form.
#[derive(Debug, Deserialize)]
struct WireChoice {
    value: String,
    #[serde(default)]
    display: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

/// Pull a named field's choices out of a NetBox `OPTIONS` response body. Pure
/// (operates on the already-fetched JSON) so it is unit-tested against canned
/// payloads. Returns the choices in NetBox's order; an empty `Vec` means the
/// field wasn't found or carried no choices (the caller treats that as a
/// "couldn't enumerate" usage error so no unvalidated value is ever sent).
///
/// DRF's `OPTIONS` metadata nests writable-field schemas under
/// `actions.<POST|PUT|PATCH>.body.<field>`. NetBox exposes the create (`POST`)
/// body on the list endpoint; `PUT`/`PATCH` appear on the detail endpoint. nbox
/// issues `OPTIONS` on the **list** endpoint and reads `POST` first (the
/// canonical writable-action body), falling back to `PUT` then `PATCH` so a
/// deployment that surfaces the field under a different verb still validates.
pub fn extract_choices(body: &serde_json::Value, field: &str) -> Vec<ChoiceOption> {
    let Some(actions) = body.get("actions") else {
        return Vec::new();
    };
    for verb in ["POST", "PUT", "PATCH"] {
        let Some(field_meta) = actions
            .get(verb)
            .and_then(|a| a.get("body"))
            .and_then(|b| b.get(field))
        else {
            continue;
        };
        let Some(choices) = field_meta.get("choices").and_then(|c| c.as_array()) else {
            continue;
        };
        let opts: Vec<ChoiceOption> = choices
            .iter()
            .filter_map(|c| {
                // Tolerate the wire shape (`{value, display}`) and a bare
                // `{"value": "…"}`; skip anything without a value.
                serde_json::from_value::<WireChoice>(c.clone())
                    .ok()
                    .map(|w| ChoiceOption {
                        value: w.value,
                        // NetBox's key is `display`; `label` is a fallback.
                        label: w.display.or(w.label),
                    })
            })
            .collect();
        if !opts.is_empty() {
            return opts;
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn opts() -> Vec<ChoiceOption> {
        vec![
            ChoiceOption {
                value: "active".into(),
                label: Some("Active".into()),
            },
            ChoiceOption {
                value: "planned".into(),
                label: Some("Planned".into()),
            },
            ChoiceOption {
                value: "offline".into(),
                label: Some("Offline".into()),
            },
            ChoiceOption {
                value: "failed".into(),
                label: Some("Failed".into()),
            },
            ChoiceOption {
                value: "decommissioning".into(),
                label: Some("Decommissioning".into()),
            },
        ]
    }

    #[test]
    fn exact_canonical_value_matches() {
        assert_eq!(
            resolve_choice(&opts(), "status", "active").unwrap(),
            "active"
        );
        assert_eq!(
            resolve_choice(&opts(), "status", "decommissioning").unwrap(),
            "decommissioning"
        );
    }

    #[test]
    fn label_matches_case_insensitively_to_one_value() {
        // NetBox's label is "Active"; the operator typed the label, or a
        // differently-cased form of it.
        assert_eq!(
            resolve_choice(&opts(), "status", "Active").unwrap(),
            "active"
        );
        assert_eq!(
            resolve_choice(&opts(), "status", "ACTIVE").unwrap(),
            "active"
        );
        assert_eq!(
            resolve_choice(&opts(), "status", "Planned").unwrap(),
            "planned"
        );
    }

    #[test]
    fn unknown_value_is_a_usage_error_listing_allowed_values() {
        let err = resolve_choice(&opts(), "status", "bogus").unwrap_err();
        assert_eq!(err.exit_code(), 2, "unknown choice is a usage error");
        let msg = format!("{err}");
        assert!(msg.contains("invalid status \"bogus\""), "{msg}");
        assert!(
            msg.contains("active, planned, offline, failed, decommissioning"),
            "{msg}"
        );
    }

    #[test]
    fn ambiguous_label_is_a_usage_error() {
        // Two choices whose labels collide case-insensitively → ambiguous.
        let ambiguous = vec![
            ChoiceOption {
                value: "active".into(),
                label: Some("Up".into()),
            },
            ChoiceOption {
                value: "online".into(),
                label: Some("up".into()),
            },
        ];
        let err = resolve_choice(&ambiguous, "status", "UP").unwrap_err();
        assert_eq!(err.exit_code(), 2);
        let msg = format!("{err}");
        assert!(msg.contains("ambiguous"), "{msg}");
        assert!(msg.contains("Allowed:"), "{msg}");
    }

    #[test]
    fn empty_option_set_is_a_usage_error() {
        // No choices enumerated → the input can't match anything.
        let err = resolve_choice(&[], "status", "active").unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(format!("{err}").contains("invalid status \"active\""));
    }

    #[test]
    fn choices_without_labels_still_match_exact_value() {
        // A deployment that exposes values but no labels: only canonical values
        // match (no case-insensitive label path).
        let value_only = vec![
            ChoiceOption {
                value: "active".into(),
                label: None,
            },
            ChoiceOption {
                value: "offline".into(),
                label: None,
            },
        ];
        assert_eq!(
            resolve_choice(&value_only, "status", "active").unwrap(),
            "active"
        );
        // A cased form that isn't an exact value and has no label to fall back
        // to is rejected (labels aren't exposed).
        let err = resolve_choice(&value_only, "status", "Active").unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn extract_choices_reads_post_body_under_actions() {
        let body = json!({
            "name": "Device",
            "actions": {
                "POST": {
                    "body": {
                        "status": {
                            "type": "choice",
                            "label": "Status",
                            "choices": [
                                {"value": "active", "display": "Active"},
                                {"value": "offline", "display": "Offline"}
                            ]
                        },
                        "name": {"type": "string", "label": "Name"}
                    }
                }
            }
        });
        let opts = extract_choices(&body, "status");
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].value, "active");
        assert_eq!(opts[0].label.as_deref(), Some("Active"));
        assert_eq!(opts[1].value, "offline");
    }

    #[test]
    fn extract_choices_falls_back_to_put_then_patch() {
        // A detail-endpoint-style OPTIONS surfaces the writable field under
        // PUT/PATCH, not POST. The extractor still finds it.
        let body = json!({
            "actions": {
                "PUT": {"body": {}},
                "PATCH": {
                    "body": {
                        "status": {"choices": [{"value": "planned", "display": "Planned"}]}
                    }
                }
            }
        });
        let opts = extract_choices(&body, "status");
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].value, "planned");
    }

    #[test]
    fn extract_choices_returns_empty_when_field_or_choices_absent() {
        let body = json!({"actions": {"POST": {"body": {"name": {"type": "string"}}}}});
        assert!(extract_choices(&body, "status").is_empty());
        // Field present but no `choices` array.
        let body = json!({"actions": {"POST": {"body": {"status": {"type": "choice"}}}}});
        assert!(extract_choices(&body, "status").is_empty());
        // No `actions` key at all.
        assert!(extract_choices(&json!({"renders": []}), "status").is_empty());
    }

    #[test]
    fn extract_choices_tolerates_bare_value_and_label_fallback() {
        // A choice with only `value`, and one exposing `label` instead of
        // NetBox's `display`.
        let body = json!({
            "actions": {"POST": {"body": {"status": {"choices": [
                {"value": "active"},
                {"value": "offline", "label": "Offline"}
            ]}}}}
        });
        let opts = extract_choices(&body, "status");
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].value, "active");
        assert!(opts[0].label.is_none());
        assert_eq!(opts[1].label.as_deref(), Some("Offline"));
    }
}
