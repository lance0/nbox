//! MCP prompt catalog — curated read-only investigation prompts.
//!
//! An MCP server can advertise *prompts*: parameterized templates an agent
//! discovers via `prompts/list` and expands via `prompts/get`. nbox's prompts
//! are curated investigation plans — a structured set of steps naming the nbox
//! tools to call — for common NetBox operator questions (IP utilization audit,
//! cable path trace, stale-prefix sweep, object change review). They are
//! *read-only* and reference only existing tools; `prompts/get` returns a single
//! user-role message with the plan, tailored to the supplied arguments.
//!
//! This is the agent-wedge differentiator (ROADMAP "MCP prompts catalog"):
//! discoverability + curated expertise, with zero live dependency (no NetBox
//! round-trip — the prompt is a plan, not data; the agent runs the plan against
//! the tools).
//!
//! Wiring mirrors the manual `list_resources`/`list_resource_templates` path
//! (the `#[tool_handler]` macro only emits tool methods, so hand-written prompt
//! methods sit alongside it without conflict).

use rmcp::ErrorData;
use rmcp::model::{
    GetPromptRequestParams, GetPromptResult, JsonObject, Prompt, PromptArgument, PromptMessage,
    PromptMessageRole,
};

/// The argument name → string value extractor for prompt arguments. MCP prompt
/// arguments arrive as a JSON object (`Option<JsonObject>`); values are strings
/// (the spec models prompt arguments as simple string slots, not typed schemas).
/// Returns `None` for a missing key or a non-string value.
fn arg_str(args: Option<&JsonObject>, key: &str) -> Option<String> {
    args.and_then(|m| m.get(key))?.as_str().map(str::to_string)
}

/// A required prompt argument: the prompt won't be useful without it.
fn arg_required(name: &str, desc: &str) -> PromptArgument {
    PromptArgument::new(name)
        .with_description(desc)
        .with_required(true)
}

/// An optional prompt argument.
fn arg_optional(name: &str, desc: &str) -> PromptArgument {
    PromptArgument::new(name).with_description(desc)
}

/// The curated prompt catalog advertised by `prompts/list`. Each entry's name
/// matches a branch in [`render_prompt`]; the arguments mirror the nbox tool
/// params the plan uses. Order is stable (tests + agent discovery rely on it).
pub fn prompts() -> Vec<Prompt> {
    vec![
        Prompt::new(
            "ip_utilization_audit",
            Some(
                "Audit IP prefix utilization: flag near-full (≥85%) and stale (<10%) prefixes, with per-prefix recommendations.",
            ),
            Some(vec![
                arg_optional(
                    "site",
                    "Scope the audit to a site (slug, name, or id). Optional — omit to audit all sites.",
                ),
                arg_optional(
                    "status",
                    "Filter prefixes by status (e.g. active, deprecated). Optional.",
                ),
            ]),
        ),
        Prompt::new(
            "cable_path_trace",
            Some(
                "Trace the cable path for a device interface end-to-end (A-side ↔ Z-side, through patch panels).",
            ),
            Some(vec![
                arg_required("device", "The device name or id owning the interface."),
                arg_required(
                    "interface",
                    "The interface name on the device (e.g. xe-0/0/1, Ethernet1/49).",
                ),
            ]),
        ),
        Prompt::new(
            "find_stale_prefixes",
            Some(
                "Find prefixes that may be reclaimable (no/few assigned IPs), cross-checked against recent change history.",
            ),
            Some(vec![arg_optional(
                "site",
                "Scope to a site (slug, name, or id). Optional — omit to scan all sites.",
            )]),
        ),
        Prompt::new(
            "object_change_review",
            Some(
                "Review an object's recent change history (audit log) and flag risky changes (deletes, ownership/scope moves).",
            ),
            Some(vec![
                arg_required(
                    "kind",
                    "Object kind: device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip-range, tenant, contact, provider, vm, cluster, vrf, route-target, interface (<device>/<name>).",
                ),
                arg_required(
                    "ref",
                    "The object reference (name, address, CIDR, VID, slug, or id).",
                ),
            ]),
        ),
    ]
}

/// Expand a prompt by name into a [`GetPromptResult`] — a single user-role
/// message with a tailored investigation plan referencing the nbox tools to
/// call. `name` must match a catalog entry; `arguments` are the caller-supplied
/// prompt args (string-valued). Unknown name → `invalid_params`.
pub fn render_prompt(request: GetPromptRequestParams) -> Result<GetPromptResult, ErrorData> {
    let name = request.name;
    let args = request.arguments.as_ref();
    let plan = match name.as_str() {
        "ip_utilization_audit" => ip_utilization_audit(args),
        "cable_path_trace" => cable_path_trace(args),
        "find_stale_prefixes" => find_stale_prefixes(args),
        "object_change_review" => object_change_review(args),
        _ => {
            let known: Vec<String> = prompts().into_iter().map(|p| p.name).collect();
            return Err(ErrorData::invalid_params(
                format!("unknown prompt \"{name}\"; available: {}", known.join(", ")),
                None,
            ));
        }
    };
    let messages = vec![PromptMessage::new_text(PromptMessageRole::User, plan)];
    Ok(GetPromptResult::new(messages))
}

/// Each plan builder: a pure function of the supplied args → a user-role plan
/// string. Plans name the exact nbox tools + args so an agent can run them
/// directly. Synthetic defaults (no real instance data) are used for any
/// missing optional arg.
fn ip_utilization_audit(args: Option<&JsonObject>) -> String {
    let site = arg_str(args, "site");
    let status = arg_str(args, "status");
    let mut filters = String::new();
    if let Some(s) = &site {
        use std::fmt::Write as _;
        let _ = write!(filters, "\n  - Scope to site: {s}");
    }
    if let Some(s) = &status {
        use std::fmt::Write as _;
        let _ = write!(filters, "\n  - Status filter: {s}");
    }
    let scope = if filters.is_empty() {
        " (no scope filters — audit all prefixes)".to_string()
    } else {
        filters
    };
    format!(
        "Audit IP prefix utilization in NetBox.{scope}\n\
         \nSteps:\n\
         1. Call `nbox_search` with an empty `query` (limit ~50) to list prefixes\
         — apply the scope filters above if any.\n\
         2. For each prefix result, call `nbox_get` with `kind=prefix`,\
         `ref=<the prefix CIDR>` to read its `utilization` field.\n\
         3. Flag prefixes with utilization >= 0.85 (near-full — expansion\
         candidates) and prefixes with utilization < 0.10 (stale — reclaim\
         candidates).\n\
         4. For each flagged prefix, call `nbox_get` again and read its\
         `ip_addresses` to confirm the assignment count.\n\
         5. Report a table: prefix | utilization | assigned IPs | status |\
         recommendation (expand / reclaim / monitor).\n\
         \nAll calls are read-only. Use `nbox_status` first to confirm the\
         connection and `nbox_search` with `--partial`-style tolerance if some\
         endpoints fail."
    )
}

fn cable_path_trace(args: Option<&JsonObject>) -> String {
    let device = arg_str(args, "device").unwrap_or_else(|| "<device>".into());
    let interface = arg_str(args, "interface").unwrap_or_else(|| "<interface>".into());
    format!(
        "Trace the cable path for a device interface end-to-end.\n\
         \nDevice: {device}\nInterface: {interface}\n\
         \nSteps:\n\
         1. Call `nbox_get_interface` with `kind=interface`,\
         `ref=\"{device}/{interface}\"` to fetch the interface and its cable path.\n\
         2. Read the `cable_path` — the A-side ↔ Z-side trace, including any\
         intermediate patch panels and the far device/interface.\n\
         3. Report the full path as a hop list:\n\
         A-side: {device} / {interface}\n\
         → [patch panels, if any, each with its position]\n\
         → Z-side: <far device> / <far interface>\n\
         4. Note the link status and flag any unterminated side (an interface\
         with no cable returns an empty path — report \"unterminated\").\n\
         5. If you need device context (site, role), call `nbox_get` with\
         `kind=device`, `ref=\"{device}\"`.\n\
         \nAll calls are read-only."
    )
}

fn find_stale_prefixes(args: Option<&JsonObject>) -> String {
    let site = arg_str(args, "site");
    let scope = match &site {
        Some(s) => format!("Scope to site: {s}"),
        None => "No site scope — scan all prefixes".to_string(),
    };
    format!(
        "Find prefixes that may be reclaimable in NetBox.\n\
         \n{scope}\n\
         \nSteps:\n\
         1. Call `nbox_search` with an empty `query` (limit ~100) to list\
         prefixes; apply the site scope if given.\n\
         2. For each prefix, call `nbox_get` with `kind=prefix`,\
         `ref=<CIDR>` and read `utilization` + the `ip_addresses` list.\n\
         3. Flag prefixes with 0 assigned IPs, or utilization below 0.10.\n\
         4. For each stale candidate, call `nbox_history` with `kind=prefix`,\
         `ref=<CIDR>`, `limit=10` to confirm nothing modified it recently — a\
         prefix touched in the last 90 days is probably in use, so lower its\
         reclaim confidence.\n\
         5. Report: prefix | utilization | assigned IPs | last-change age |\
         reclaim confidence (high if no recent changes, low otherwise).\n\
         \nThe `nbox_history` step is the system audit log (create/update/delete\
         by whom + when) — distinct from `nbox_journal` (operator notes).\
         All calls are read-only."
    )
}

fn object_change_review(args: Option<&JsonObject>) -> String {
    let kind = arg_str(args, "kind").unwrap_or_else(|| "<kind>".into());
    let reference = arg_str(args, "ref").unwrap_or_else(|| "<ref>".into());
    format!(
        "Review an object's recent change history for risk.\n\
         \nObject: kind={kind}, ref=\"{reference}\"\n\
         \nSteps:\n\
         1. Call `nbox_history` with `kind={kind}`, `ref=\"{reference}\"`,\
         `limit=20` to fetch the audit-log entries (newest first).\n\
         2. Group entries by `request_id` — one user action can produce several\
         object-changes sharing a UUID; grouping shows the real actions.\n\
         3. For each group, summarize: who (user), when (time), and the\
         `fields_changed`.\n\
         4. Flag high-risk changes: any `delete` action, or `fields_changed`\
         containing status, tenant, site, cluster, or owner (ownership/scope\
         moves are the riskiest read-only signals).\n\
         5. Report a timeline grouped by request, with risk flags, and a\
         one-line summary of the most recent change.\n\
         \n`nbox_history` reads `/api/core/object-changes/` (NetBox 4.x) — the\
         system audit log, distinct from `nbox_journal` (operator notes).\
         All calls are read-only."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::GetPromptRequestParams;

    fn request(name: &str) -> GetPromptRequestParams {
        let mut r = GetPromptRequestParams::default();
        r.name = name.to_string();
        r
    }

    fn request_args(name: &str, args: &[(&str, &str)]) -> GetPromptRequestParams {
        let mut map = serde_json::Map::new();
        for (k, v) in args {
            map.insert(
                (*k).to_string(),
                serde_json::Value::String((*v).to_string()),
            );
        }
        let mut r = GetPromptRequestParams::default();
        r.name = name.to_string();
        r.arguments = Some(map);
        r
    }

    #[test]
    fn catalog_lists_four_prompts_with_expected_names() {
        let names: Vec<String> = prompts().into_iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "ip_utilization_audit",
                "cable_path_trace",
                "find_stale_prefixes",
                "object_change_review",
            ]
        );
    }

    #[test]
    fn every_prompt_has_a_description_and_arguments() {
        for p in prompts() {
            assert!(p.description.is_some(), "{} has no description", p.name);
            assert!(p.arguments.is_some(), "{} has no arguments", p.name);
        }
    }

    #[test]
    fn cable_path_trace_has_two_required_args() {
        let p = prompts()
            .into_iter()
            .find(|p| p.name == "cable_path_trace")
            .unwrap();
        let args = p.arguments.unwrap();
        assert_eq!(args.len(), 2);
        assert!(args.iter().all(|a| a.required == Some(true)));
        let names: Vec<String> = args.into_iter().map(|a| a.name).collect();
        assert_eq!(names, vec!["device", "interface"]);
    }

    #[test]
    fn render_each_prompt_returns_a_user_message() {
        for p in prompts() {
            let result = render_prompt(request(&p.name)).expect("render");
            assert_eq!(result.messages.len(), 1, "{} returned >1 message", p.name);
            assert_eq!(result.messages[0].role, PromptMessageRole::User);
        }
    }

    #[test]
    fn plans_reference_nbox_tools() {
        // Each plan should name at least one nbox tool, so it's actionable.
        let cases = [
            ("ip_utilization_audit", "nbox_search"),
            ("ip_utilization_audit", "nbox_get"),
            ("cable_path_trace", "nbox_get_interface"),
            ("find_stale_prefixes", "nbox_history"),
            ("object_change_review", "nbox_history"),
        ];
        for (prompt, tool) in cases {
            let result = render_prompt(request(prompt)).unwrap();
            let text = match &result.messages[0].content {
                rmcp::model::PromptMessageContent::Text { text } => text.as_str(),
                _ => panic!("{prompt} returned non-text content"),
            };
            assert!(
                text.contains(tool),
                "{prompt} plan does not reference {tool}: {text}"
            );
        }
    }

    #[test]
    fn cable_path_trace_plan_substitutes_args() {
        let result = render_prompt(request_args(
            "cable_path_trace",
            &[("device", "edge01"), ("interface", "xe-0/0/1")],
        ))
        .unwrap();
        let text = match &result.messages[0].content {
            rmcp::model::PromptMessageContent::Text { text } => text.as_str(),
            _ => panic!("non-text content"),
        };
        assert!(
            text.contains("Device: edge01"),
            "device not substituted: {text}"
        );
        assert!(
            text.contains("Interface: xe-0/0/1"),
            "interface not substituted: {text}"
        );
        assert!(
            text.contains("\"edge01/xe-0/0/1\""),
            "compound ref not in plan: {text}"
        );
    }

    #[test]
    fn unknown_prompt_name_is_invalid_params() {
        let err = render_prompt(request("teapot")).expect_err("unknown prompt should error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unknown prompt") && msg.contains("teapot"),
            "got: {msg}"
        );
        // Lists the available prompts so the caller can self-correct.
        assert!(
            msg.contains("ip_utilization_audit"),
            "missing available list: {msg}"
        );
    }

    #[test]
    fn optional_args_default_when_absent() {
        // ip_utilization_audit with no args still produces a valid plan
        // (scoped to "all prefixes"), not an error.
        let result = render_prompt(request("ip_utilization_audit")).unwrap();
        let text = match &result.messages[0].content {
            rmcp::model::PromptMessageContent::Text { text } => text.as_str(),
            _ => panic!("non-text content"),
        };
        assert!(
            text.contains("no scope filters"),
            "absent-optional plan: {text}"
        );
    }
}
