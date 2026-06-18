//! Integration tests: NetBox 4.2+ polymorphic scope surfaced through the shared
//! view fetch path (`detail::*_view_by_ref`) the CLI/MCP/TUI all use — covering
//! the wire → model → view → render chain end to end.
//!
//! A prefix (and the IP that derives context from it) carries a `scope`
//! (the scope object's name, for *any* scope type) plus a friendly `scope_type`
//! (`site`/`location`/`region`/`site-group`). The IP view exposes `scope` /
//! `scope_type` and no longer has a `site` field.

use nbox::config::ProfileConfig;
use nbox::domain::detail;
use nbox::netbox::client::NetBoxClient;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> NetBoxClient {
    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    NetBoxClient::new(&profile, None).unwrap()
}

fn not_found(noun: &str, value: &str) -> anyhow::Error {
    anyhow::anyhow!("no {noun} matched \"{value}\"")
}

/// Mount the children/IPs lookups a prefix view makes (empty here) for `cidr`.
async fn mount_empty_prefix_sections(server: &MockServer, cidr: &str) {
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("within", cidr))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("parent", cidr))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(server)
        .await;
}

/// A prefix scoped to a site/location/region surfaces `scope` (the object name)
/// and the matching friendly `scope_type`.
#[tokio::test]
async fn prefix_surfaces_each_scope_type() {
    for (scope_type, friendly, name) in [
        ("dcim.site", "site", "iad1"),
        ("dcim.location", "location", "row-a"),
        ("dcim.region", "region", "us-east"),
    ] {
        let server = MockServer::start().await;
        let cidr = "10.44.208.0/24";
        Mock::given(method("GET"))
            .and(path("/api/ipam/prefixes/"))
            .and(query_param("prefix", cidr))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{
                    "id": 5, "url": "http://nb/api/ipam/prefixes/5/", "prefix": cidr,
                    "status": {"value": "active", "label": "Active"},
                    "scope_type": scope_type,
                    "scope_id": 1,
                    "scope": {"id": 1, "name": name, "display": name}
                }]
            })))
            .mount(&server)
            .await;
        mount_empty_prefix_sections(&server, cidr).await;

        let view = detail::prefix_view_by_ref(&client(&server), cidr, None, &not_found)
            .await
            .unwrap();

        assert_eq!(view.scope.as_deref(), Some(name));
        assert_eq!(view.scope_type.as_deref(), Some(friendly));

        let v: Value = serde_json::to_value(&view).unwrap();
        assert_eq!(v["scope"], json!(name));
        assert_eq!(v["scope_type"], json!(friendly));

        let plain = view.to_plain();
        assert!(plain.contains(&format!("scope: {name}")), "got: {plain}");
        assert!(
            plain.contains(&format!("scope_type: {friendly}")),
            "got: {plain}"
        );
    }
}

/// The same CIDR in two different VRFs must not bleed children/member IPs across
/// VRFs: `prefix <cidr> --vrf <ref>` resolves the right prefix AND scopes its
/// child-prefix (`within`) and contained-IP (`parent`) sections to that prefix's
/// VRF (`vrf_id=<id>`). The mock keys `within`/`parent` by `vrf_id`, so a leak
/// (an unscoped or wrong-VRF section query) would 404/miss instead of matching.
#[tokio::test]
async fn prefix_scopes_children_and_ips_to_resolved_vrf() {
    let cidr = "10.0.0.0/24";
    // (vrf reference, vrf id, the child + member IP that live in that VRF).
    for (vrf_ref, vrf_id, child, ip) in [
        ("blue", 7u64, "10.0.0.0/26", "10.0.0.1/24"),
        ("red", 8u64, "10.0.0.128/26", "10.0.0.129/24"),
    ] {
        let server = MockServer::start().await;
        // Both VRFs' prefixes share the CIDR; `--vrf` retains exactly one.
        Mock::given(method("GET"))
            .and(path("/api/ipam/prefixes/"))
            .and(query_param("prefix", cidr))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 2, "next": null, "previous": null,
                "results": [
                    {
                        "id": 7, "url": "u", "prefix": cidr,
                        "status": {"value": "active", "label": "Active"},
                        "vrf": {"id": 7, "url": "u", "name": "blue", "rd": "65000:7"}
                    },
                    {
                        "id": 8, "url": "u", "prefix": cidr,
                        "status": {"value": "active", "label": "Active"},
                        "vrf": {"id": 8, "url": "u", "name": "red", "rd": "65000:8"}
                    }
                ]
            })))
            .mount(&server)
            .await;
        // Children (`within`) are keyed by the resolved prefix's VRF id.
        Mock::given(method("GET"))
            .and(path("/api/ipam/prefixes/"))
            .and(query_param("within", cidr))
            .and(query_param("vrf_id", vrf_id.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 99, "url": "u", "prefix": child}]
            })))
            .mount(&server)
            .await;
        // Member IPs (`parent`) are likewise keyed by that VRF id.
        Mock::given(method("GET"))
            .and(path("/api/ipam/ip-addresses/"))
            .and(query_param("parent", cidr))
            .and(query_param("vrf_id", vrf_id.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "next": null, "previous": null,
                "results": [{"id": 99, "url": "u", "address": ip}]
            })))
            .mount(&server)
            .await;

        let view = detail::prefix_view_by_ref(&client(&server), cidr, Some(vrf_ref), &not_found)
            .await
            .unwrap();

        assert_eq!(view.vrf.as_deref(), Some(vrf_ref));
        // Only the resolved VRF's child + IP — never the other VRF's.
        assert_eq!(view.child_prefixes, vec![child.to_string()]);
        assert_eq!(view.ip_addresses.len(), 1);
        assert_eq!(view.ip_addresses[0].address, ip);
    }
}

/// A global/no-VRF prefix scopes its child/contained sections to the global table
/// (`vrf_id=null`), so it can't pick up rows from any VRF that shares the CIDR.
#[tokio::test]
async fn global_prefix_scopes_children_and_ips_to_global_table() {
    let server = MockServer::start().await;
    let cidr = "10.0.0.0/24";
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("prefix", cidr))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "u", "prefix": cidr,
                "status": {"value": "active", "label": "Active"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("within", cidr))
        .and(query_param("vrf_id", "null"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 2, "url": "u", "prefix": "10.0.0.0/26"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("parent", cidr))
        .and(query_param("vrf_id", "null"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 3, "url": "u", "address": "10.0.0.1/24"}]
        })))
        .mount(&server)
        .await;

    let view = detail::prefix_view_by_ref(&client(&server), cidr, None, &not_found)
        .await
        .unwrap();

    assert!(view.vrf.is_none());
    assert_eq!(view.child_prefixes, vec!["10.0.0.0/26".to_string()]);
    assert_eq!(view.ip_addresses.len(), 1);
    assert_eq!(view.ip_addresses[0].address, "10.0.0.1/24");
}

/// A VLAN with a non-site polymorphic scope surfaces `scope` + a friendly
/// `scope_type` and carries no `site` field.
#[tokio::test]
async fn vlan_surfaces_non_site_scope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("vid", "208"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "http://nb/api/ipam/vlans/3/", "vid": 208, "name": "users",
                "status": {"value": "active", "label": "Active"},
                "scope_type": "dcim.region",
                "scope_id": 9,
                "scope": {"id": 9, "name": "us-east", "display": "us-east"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("vlan_id", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&server)
        .await;

    let view = detail::vlan_view_by_ref(&client(&server), "208", None, None, &not_found)
        .await
        .unwrap();

    assert_eq!(view.scope.as_deref(), Some("us-east"));
    assert_eq!(view.scope_type.as_deref(), Some("region"));

    let v: Value = serde_json::to_value(&view).unwrap();
    assert_eq!(v["scope"], json!("us-east"));
    assert_eq!(v["scope_type"], json!("region"));
    assert!(
        v.get("site").is_none(),
        "vlan view must have no site key: {v}"
    );
}

/// An IP whose most-specific parent prefix is region-scoped derives `scope` +
/// `scope_type` from that prefix, and the IP view exposes no `site` field.
#[tokio::test]
async fn ip_derives_non_site_scope_from_parent_prefix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("address", "10.0.0.5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 7, "url": "http://nb/api/ipam/ip-addresses/7/",
                "address": "10.0.0.5/24",
                "status": {"value": "active", "label": "Active"}
            }]
        })))
        .mount(&server)
        .await;
    // No VRF on the IP → parent lookup is scoped to the global table.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("contains", "10.0.0.5"))
        .and(query_param("vrf_id", "null"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "u", "prefix": "10.0.0.0/16"},
                {
                    "id": 2, "url": "u", "prefix": "10.0.0.0/24",
                    "scope_type": "dcim.location",
                    "scope_id": 4,
                    "scope": {"id": 4, "name": "row-a", "display": "row-a"}
                }
            ]
        })))
        .mount(&server)
        .await;

    let view = detail::ip_view_by_ref(&client(&server), "10.0.0.5", None, &not_found)
        .await
        .unwrap();

    // Context comes from the most-specific (/24) parent prefix.
    assert_eq!(view.parent_prefix.as_deref(), Some("10.0.0.0/24"));
    assert_eq!(view.scope.as_deref(), Some("row-a"));
    assert_eq!(view.scope_type.as_deref(), Some("location"));

    // The renamed field set: `scope`/`scope_type` present, `site` gone.
    let v: Value = serde_json::to_value(&view).unwrap();
    assert_eq!(v["scope"], json!("row-a"));
    assert_eq!(v["scope_type"], json!("location"));
    assert!(
        v.get("site").is_none(),
        "ip view must have no site key: {v}"
    );

    let plain = view.to_key_values().render();
    assert!(plain.contains("scope: row-a"), "got: {plain}");
    assert!(plain.contains("scope_type: location"), "got: {plain}");
    assert!(!plain.contains("site:"), "no site row expected: {plain}");
}

/// A VLAN that belongs to a *group* surfaces the GROUP's scope on the new
/// additive `group_scope` / `group_scope_type` fields. A VLAN group is itself
/// polymorphically scoped (the VLAN is not), and the nested `group` brief omits
/// that scope, so the view does ONE follow-up GET of the group by id. The VLAN's
/// own scope fields are untouched.
#[tokio::test]
async fn vlan_with_grouped_scope_surfaces_group_scope() {
    let server = MockServer::start().await;
    // The VLAN: directly sited (its own scope), plus a group reference.
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("vid", "208"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "http://nb/api/ipam/vlans/3/", "vid": 208, "name": "users",
                "status": {"value": "active", "label": "Active"},
                "site": {"id": 1, "name": "iad1", "display": "iad1"},
                "group": {"id": 9, "name": "iad1-campus", "display": "iad1-campus"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("vlan_id", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&server)
        .await;
    // The follow-up group fetch by id — region-scoped. Asserted to be hit once.
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlan-groups/9/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 9, "name": "iad1-campus", "slug": "iad1-campus",
            "scope_type": "dcim.region",
            "scope_id": 5,
            "scope": {"id": 5, "name": "us-east", "display": "us-east"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let view = detail::vlan_view_by_ref(&client(&server), "208", None, None, &not_found)
        .await
        .unwrap();

    // The VLAN's own scope (its direct site) is unchanged.
    assert_eq!(view.scope.as_deref(), Some("iad1"));
    assert_eq!(view.scope_type.as_deref(), Some("site"));
    assert_eq!(view.group.as_deref(), Some("iad1-campus"));
    // The group's scope is surfaced on the new additive fields.
    assert_eq!(view.group_scope.as_deref(), Some("us-east"));
    assert_eq!(view.group_scope_type.as_deref(), Some("region"));

    let v: Value = serde_json::to_value(&view).unwrap();
    assert_eq!(v["group_scope"], json!("us-east"));
    assert_eq!(v["group_scope_type"], json!("region"));

    let plain = view.to_plain();
    assert!(plain.contains("group_scope: us-east"), "got: {plain}");
    assert!(plain.contains("group_scope_type: region"), "got: {plain}");
}

/// A VLAN with NO group makes NO second request: the group fetch is gated on the
/// VLAN actually having a group. The vlan-groups endpoint is mounted with
/// `.expect(0)` so any stray fetch fails the test, and the new `group_scope` /
/// `group_scope_type` fields are omitted (output otherwise unchanged).
#[tokio::test]
async fn vlan_without_group_makes_no_group_fetch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("vid", "208"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "http://nb/api/ipam/vlans/3/", "vid": 208, "name": "users",
                "status": {"value": "active", "label": "Active"},
                "site": {"id": 1, "name": "iad1", "display": "iad1"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("vlan_id", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&server)
        .await;
    // No group → this must never be requested.
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlan-groups/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&server)
        .await;

    let view = detail::vlan_view_by_ref(&client(&server), "208", None, None, &not_found)
        .await
        .unwrap();

    // Unchanged output: own scope present, group-scope fields absent.
    assert_eq!(view.scope.as_deref(), Some("iad1"));
    assert_eq!(view.scope_type.as_deref(), Some("site"));
    assert!(view.group_scope.is_none());
    assert!(view.group_scope_type.is_none());

    let v: Value = serde_json::to_value(&view).unwrap();
    assert!(v.get("group_scope").is_none(), "no group_scope key: {v}");
    assert!(
        v.get("group_scope_type").is_none(),
        "no group_scope_type key: {v}"
    );
}
