//! End-to-end test of `nbox serve` — the MCP server (read-only by default) over stdio.
//!
//! This launches the *real* compiled binary (`env!("CARGO_BIN_EXE_nbox")`) with
//! `serve`, then speaks newline-delimited JSON-RPC over its stdin/stdout (via the
//! shared `support::serve::ServeChild` harness). It proves three things at once:
//!
//!   1. `nbox serve` is wired correctly: `connect()` builds a client from a
//!      minimal profile + a dummy token without hitting the network, and the MCP
//!      server starts.
//!   2. The server speaks the protocol: `initialize` returns a `result` with
//!      `serverInfo` + `capabilities.tools`, and `tools/list` returns exactly the
//!      expected tool set.
//!   3. The stdio invariant holds: every line the child writes to stdout is valid
//!      JSON-RPC (nothing — banners, logs, prompts — leaks onto stdout).

mod support;

use serde_json::{Value, json};
use support::serve::{PROTOCOL_VERSION, ServeChild, tool_payload};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build a minimal serve config pointing at `url`. The token comes from the
/// `NBOX_TOKEN` override the harness sets, so `token_env` is just a placeholder.
fn serve_config(url: &str) -> String {
    format!(
        "active_profile = \"test\"\n\n[profiles.test]\nurl = \"{url}\"\ntoken_env = \"NBOX_TEST_TOKEN_UNUSED\"\n"
    )
}

/// The exact tool set `nbox serve` must expose (see the `#[tool(...)]` adapters
/// in `src/mcp/mod.rs`). Order-independent: we compare as sorted sets.
const EXPECTED_TOOLS: &[&str] = &[
    "nbox_status",
    "nbox_search",
    "nbox_get",
    "nbox_get_interface",
    "nbox_next_ip",
    "nbox_next_prefix",
    "nbox_journal",
    "nbox_history",
    "nbox_list_tags",
    "nbox_tagged",
    "nbox_cache_clear",
    "nbox_plan_write",
    "nbox_apply_write",
];

async fn mount_device_status_update_requiring_token(mock: &MockServer, token: &str) {
    let auth = format!("Bearer {token}");
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(header("authorization", auth.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
            "next": null,
            "previous": null,
            "results": [{
                "id": 1,
                "name": "edge01",
                "slug": "edge01",
                "status": {"value": "planned", "label": "Planned"},
                "display": "edge01",
                "url": "u",
                "custom_fields": {}
            }]
        })))
        .mount(mock)
        .await;
    Mock::given(method("OPTIONS"))
        .and(path("/api/dcim/devices/"))
        .and(header("authorization", auth.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "Device",
            "actions": {
                "POST": {
                    "status": {
                        "type": "choice",
                        "label": "Status",
                        "choices": [
                            {"value": "active", "display": "Active"},
                            {"value": "planned", "display": "Planned"}
                        ]
                    }
                }
            }
        })))
        .mount(mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/1/"))
        .and(header("authorization", auth.as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "id": 1,
                    "name": "edge01",
                    "slug": "edge01",
                    "status": {"value": "planned", "label": "Planned"},
                    "display": "edge01",
                    "url": "u",
                    "custom_fields": {}
                }))
                .insert_header("ETag", "\"v1\""),
        )
        .mount(mock)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/dcim/devices/1/"))
        .and(header("authorization", auth.as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "id": 1,
                    "name": "edge01",
                    "slug": "edge01",
                    "status": {"value": "active", "label": "Active"},
                    "display": "edge01",
                    "url": "u",
                    "custom_fields": {}
                }))
                .insert_header("ETag", "\"v2\""),
        )
        .mount(mock)
        .await;
}

#[test]
fn serve_handshake_lists_all_tools_with_clean_stdout() {
    // The profile URL is unreachable on purpose: the initialize/tools/list
    // handshake never makes a network call, so the bogus URL keeps it offline.
    let mut server = ServeChild::spawn(&serve_config("http://127.0.0.1:1/"), &[], "dummy");

    // 1) initialize (id 1) with a valid protocolVersion, empty capabilities, and
    //    a clientInfo. The server replies with the negotiated InitializeResult.
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "nbox-e2e-test", "version": "0.0.0" }
        }
    }));

    let init = server.read_response(1);
    assert_eq!(init["jsonrpc"], "2.0");
    let result = init
        .get("result")
        .unwrap_or_else(|| panic!("initialize had no result: {init}"));
    let server_info = &result["serverInfo"];
    assert!(
        server_info.get("name").and_then(Value::as_str).is_some(),
        "initialize result missing serverInfo.name: {init}"
    );
    assert!(
        result["capabilities"].get("tools").is_some(),
        "initialize result missing capabilities.tools: {init}"
    );
    assert!(
        result["capabilities"].get("resources").is_some(),
        "initialize result missing capabilities.resources: {init}"
    );
    assert!(
        result["capabilities"].get("prompts").is_some(),
        "initialize result missing capabilities.prompts: {init}"
    );
    assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);

    // 2) notifications/initialized (no id — a notification, no response expected).
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));

    // 3) tools/list (id 2).
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));

    let list = server.read_response(2);
    let tools = list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list had no result.tools array: {list}"));

    let mut got: Vec<String> = tools
        .iter()
        .map(|t| {
            t["name"]
                .as_str()
                .unwrap_or_else(|| panic!("tool entry missing name: {t}"))
                .to_string()
        })
        .collect();
    got.sort();

    let mut want: Vec<String> = EXPECTED_TOOLS.iter().map(ToString::to_string).collect();
    want.sort();

    assert_eq!(got, want, "tools/list returned an unexpected tool set");

    // 4) resources/templates/list (id 3): the single `nbox://{kind}/{ref}` template.
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "resources/templates/list",
        "params": {}
    }));

    let templates = server.read_response(3);
    let list = templates["result"]["resourceTemplates"]
        .as_array()
        .unwrap_or_else(|| panic!("templates list had no result.resourceTemplates: {templates}"));
    assert_eq!(list.len(), 1, "expected exactly one resource template");
    assert_eq!(list[0]["uriTemplate"], "nbox://{kind}/{ref}");

    // 5) resources/list (id 4): no static resources, but the method must succeed.
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "resources/list",
        "params": {}
    }));

    let resources = server.read_response(4);
    let empty = resources["result"]["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("resources/list had no result.resources: {resources}"));
    assert!(
        empty.is_empty(),
        "expected no static resources: {resources}"
    );

    // 6) prompts/list (id 5): the curated investigation-prompt catalog.
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "prompts/list",
        "params": {}
    }));

    let prompts_resp = server.read_response(5);
    let prompts = prompts_resp["result"]["prompts"]
        .as_array()
        .unwrap_or_else(|| panic!("prompts/list had no result.prompts: {prompts_resp}"));
    let names: Vec<String> = prompts
        .iter()
        .map(|p| {
            p["name"]
                .as_str()
                .unwrap_or_else(|| panic!("prompt missing name: {p}"))
                .to_string()
        })
        .collect();
    assert_eq!(
        names,
        vec![
            "ip_utilization_audit",
            "cable_path_trace",
            "find_stale_prefixes",
            "object_change_review",
        ],
        "prompts/list returned an unexpected catalog"
    );

    // 7) prompts/get (id 6): expanding a named prompt returns a user-role message.
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "prompts/get",
        "params": {
            "name": "cable_path_trace",
            "arguments": {"device": "edge01", "interface": "xe-0/0/1"}
        }
    }));

    let get = server.read_response(6);
    let messages = get["result"]["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("prompts/get had no result.messages: {get}"));
    assert_eq!(messages.len(), 1, "expected one message: {get}");
    assert_eq!(messages[0]["role"], "user");
    let text = messages[0]["content"]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("prompt message had no text: {get}"));
    assert!(
        text.contains("interface=\"xe-0/0/1\""),
        "args not substituted: {text}"
    );
    assert!(
        text.contains("nbox_get_interface"),
        "plan missing tool ref: {text}"
    );

    server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn local_stdio_writes_plan_and_apply_through_json_rpc() {
    let netbox = MockServer::start().await;
    mount_device_status_update_requiring_token(&netbox, "nbt_profile.key").await;
    let mut server = ServeChild::spawn(
        &serve_config(&netbox.uri()),
        &["--local-writes"],
        "nbt_profile.key",
    );

    let init = server.handshake();
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap_or_default()
            .contains("Local writes are enabled"),
        "initialize instructions should describe local writes: {init}"
    );

    let plan_msg = server.call(
        2,
        "nbox_plan_write",
        json!({
            "operation": { "kind": "device_status", "device": "edge01", "status": "active" }
        }),
    );
    let plan = tool_payload(&plan_msg);
    assert_eq!(plan["target"]["kind"], "device", "plan: {plan}");
    assert_eq!(plan["patch"], json!({"status": "active"}), "plan: {plan}");
    assert!(
        !plan["confirm_token"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "plan carries a confirm token"
    );

    let receipt_msg = server.call(3, "nbox_apply_write", json!({ "plan": plan }));
    let receipt = tool_payload(&receipt_msg);
    assert_eq!(receipt["applied"], true, "receipt: {receipt}");
    assert_eq!(receipt["status"], 200, "receipt: {receipt}");

    server.shutdown();
}
