//! Integration tests for endpoint query helpers.

use nbox::config::ProfileConfig;
use nbox::netbox::client::NetBoxClient;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> NetBoxClient {
    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    NetBoxClient::new(&profile, None).unwrap()
}

/// Mount an empty paginated page for a `path`+`query_param` combination, used to
/// model a resolution step (slug/name__ie) that finds nothing before a fallback.
async fn mount_empty_page(server: &MockServer, p: &str, key: &str, value: &str) {
    Mock::given(method("GET"))
        .and(path(p))
        .and(query_param(key, value))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn device_by_name_uses_name_ie_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("name__ie", "edge01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01",
                "status": {"value": "active", "label": "Active"}
            }]
        })))
        .mount(&server)
        .await;

    let device = client(&server)
        .device_by_ref("edge01")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(device.name, "edge01");
}

#[tokio::test]
async fn device_by_id_hits_detail_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/5/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 5, "url": "http://nb/api/dcim/devices/5/", "name": "edge05"
        })))
        .mount(&server)
        .await;

    let device = client(&server).device_by_ref("5").await.unwrap().unwrap();
    assert_eq!(device.id, 5);
    assert_eq!(device.name, "edge05");
}

#[tokio::test]
async fn device_by_missing_id_returns_none_not_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/99999/"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Not found."))
        .mount(&server)
        .await;

    // A 404 on the ID detail endpoint must map to Ok(None), per the contract.
    let device = client(&server).device_by_ref("99999").await.unwrap();
    assert!(device.is_none());
}

#[tokio::test]
async fn device_not_found_returns_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&server)
        .await;

    let device = client(&server).device_by_ref("nope").await.unwrap();
    assert!(device.is_none());
}

#[tokio::test]
async fn device_contains_with_multiple_matches_is_ambiguous() {
    let server = MockServer::start().await;
    // Exact (name__ie) finds nothing; the contains fallback returns several.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("name__ie", "edge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("name__ic", "edge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "u", "name": "edge01"},
                {"id": 2, "url": "u", "name": "edge02"}
            ]
        })))
        .mount(&server)
        .await;

    let err = client(&server).device_by_ref("edge").await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("ambiguous"), "got: {msg}");
    assert!(
        msg.contains("edge01") && msg.contains("edge02"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn ip_candidates_use_address_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("address", "10.44.208.55"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 7, "url": "http://nb/ip/7/", "address": "10.44.208.55/24"}]
        })))
        .mount(&server)
        .await;

    let ips = client(&server).ip_candidates("10.44.208.55").await.unwrap();
    assert_eq!(ips.len(), 1);
    assert_eq!(ips[0].address, "10.44.208.55/24");
}

#[tokio::test]
async fn prefixes_containing_use_contains_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("contains", "10.44.208.55"))
        .and(query_param("vrf_id", "null"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "http://nb/p/1/", "prefix": "10.44.0.0/16"},
                {"id": 2, "url": "http://nb/p/2/", "prefix": "10.44.208.0/24"}
            ]
        })))
        .mount(&server)
        .await;

    // No VRF → scoped to the global table (vrf_id=null).
    let prefixes = client(&server)
        .prefixes_containing("10.44.208.55", None)
        .await
        .unwrap();
    assert_eq!(prefixes.len(), 2);
}

#[tokio::test]
async fn prefixes_containing_scopes_to_vrf() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("contains", "10.0.0.1"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 9, "url": "http://nb/p/9/", "prefix": "10.0.0.0/24"}]
        })))
        .mount(&server)
        .await;

    let prefixes = client(&server)
        .prefixes_containing("10.0.0.1", Some(7))
        .await
        .unwrap();
    assert_eq!(prefixes.len(), 1);
}

#[tokio::test]
async fn prefix_by_cidr_uses_prefix_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("prefix", "10.44.208.0/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 5, "url": "http://nb/p/5/", "prefix": "10.44.208.0/24"}]
        })))
        .mount(&server)
        .await;

    let prefix = client(&server)
        .prefix_by_cidr("10.44.208.0/24")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prefix.prefix, "10.44.208.0/24");
}

#[tokio::test]
async fn prefix_children_and_ips_use_within_and_parent() {
    let server = MockServer::start().await;
    // No VRF → children/member IPs are scoped to the global table (vrf_id=null).
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("within", "10.44.208.0/24"))
        .and(query_param("vrf_id", "null"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 6, "url": "u", "prefix": "10.44.208.0/26"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("parent", "10.44.208.0/24"))
        .and(query_param("vrf_id", "null"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 7, "url": "u", "address": "10.44.208.1/24"}]
        })))
        .mount(&server)
        .await;

    let cli = client(&server);
    let children = cli
        .prefix_children("10.44.208.0/24", None, 50)
        .await
        .unwrap();
    let ips = cli.prefix_ips("10.44.208.0/24", None, 50).await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(ips.len(), 1);
}

#[tokio::test]
async fn prefix_children_and_ips_scope_to_vrf() {
    let server = MockServer::start().await;
    // A VRF id is threaded into both the `within` (children) and `parent` (IPs)
    // queries so a CIDR shared across VRFs can't pull the wrong VRF's rows.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("within", "10.0.0.0/24"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 6, "url": "u", "prefix": "10.0.0.0/26"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("parent", "10.0.0.0/24"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 7, "url": "u", "address": "10.0.0.1/24"}]
        })))
        .mount(&server)
        .await;

    let cli = client(&server);
    let children = cli
        .prefix_children("10.0.0.0/24", Some(7), 50)
        .await
        .unwrap();
    let ips = cli.prefix_ips("10.0.0.0/24", Some(7), 50).await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(ips.len(), 1);
}

#[tokio::test]
async fn available_ips_and_prefixes_parse_bare_arrays() {
    let server = MockServer::start().await;
    // These endpoints return a bare JSON array, not a paginated page.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/5/available-ips/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"family": 4, "address": "10.44.208.1/24"},
            {"family": 4, "address": "10.44.208.2/24"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/5/available-prefixes/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"family": 4, "prefix": "10.44.208.0/26"}
        ])))
        .mount(&server)
        .await;

    let cli = client(&server);
    let ips = cli.prefix_available_ips(5, 10).await.unwrap();
    assert_eq!(ips.len(), 2);
    assert_eq!(ips[0].address, "10.44.208.1/24");

    let prefixes = cli.prefix_available_prefixes(5).await.unwrap();
    assert_eq!(prefixes.len(), 1);
    assert_eq!(prefixes[0].prefix, "10.44.208.0/26");
}

#[tokio::test]
async fn vlan_by_numeric_ref_uses_vid_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("vid", "208"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 3, "url": "u", "vid": 208, "name": "users"}]
        })))
        .mount(&server)
        .await;

    let vlan = client(&server).vlan_by_ref("208").await.unwrap().unwrap();
    assert_eq!(vlan.vid, 208);
}

#[tokio::test]
async fn site_by_ref_tries_slug_first() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 1, "url": "u", "name": "iad1", "slug": "iad1"}]
        })))
        .mount(&server)
        .await;

    let site = client(&server).site_by_ref("iad1").await.unwrap().unwrap();
    assert_eq!(site.slug, "iad1");
}

#[tokio::test]
async fn circuit_by_cid_uses_cid_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/circuits/circuits/"))
        .and(query_param("cid", "ACME-1234"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "http://nb/api/circuits/circuits/3/", "cid": "ACME-1234",
                "provider": {"id": 1, "display": "ACME"}
            }]
        })))
        .mount(&server)
        .await;

    let circuit = client(&server)
        .circuit_by_ref("ACME-1234")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(circuit.cid, "ACME-1234");
    assert_eq!(circuit.provider.unwrap().label(), "ACME");
}

#[tokio::test]
async fn circuit_by_id_hits_detail_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/circuits/circuits/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "http://nb/api/circuits/circuits/7/", "cid": "ACME-7"
        })))
        .mount(&server)
        .await;

    let circuit = client(&server).circuit_by_ref("7").await.unwrap().unwrap();
    assert_eq!(circuit.id, 7);
    assert_eq!(circuit.cid, "ACME-7");
}

#[tokio::test]
async fn tags_lists_all() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/extras/tags/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "name": "Critical", "slug": "critical",
                "color": "ff0000", "tagged_items": 3
            }]
        })))
        .mount(&server)
        .await;

    let tags = client(&server).tags(200).await.unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].slug, "critical");
    assert_eq!(tags[0].tagged_items, Some(3));
}

#[tokio::test]
async fn journal_entries_filter_by_assigned_object() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/extras/journal-entries/"))
        .and(query_param("assigned_object_type", "dcim.device"))
        .and(query_param("assigned_object_id", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 5, "created": "2024-01-02",
                "kind": {"value": "info", "label": "Info"}, "comments": "rebooted"
            }]
        })))
        .mount(&server)
        .await;

    let entries = client(&server)
        .journal_entries("dcim.device", 1, 20)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].comments, "rebooted");
}

#[tokio::test]
async fn ip_range_by_start_uses_start_address_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-ranges/"))
        .and(query_param("start_address", "10.0.0.10/24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/ipam/ip-ranges/1/",
                "start_address": "10.0.0.10/24", "end_address": "10.0.0.20/24", "size": 11
            }]
        })))
        .mount(&server)
        .await;

    let range = client(&server)
        .ip_range_by_ref("10.0.0.10/24")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(range.start_address, "10.0.0.10/24");
    assert_eq!(range.size, Some(11));
}

#[tokio::test]
async fn aggregate_by_cidr_uses_prefix_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/aggregates/"))
        .and(query_param("prefix", "10.0.0.0/8"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/ipam/aggregates/1/", "prefix": "10.0.0.0/8",
                "rir": {"id": 1, "display": "RFC 1918"}
            }]
        })))
        .mount(&server)
        .await;

    let agg = client(&server)
        .aggregate_by_ref("10.0.0.0/8")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(agg.prefix, "10.0.0.0/8");
}

#[tokio::test]
async fn asn_by_ref_uses_asn_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/asns/"))
        .and(query_param("asn", "64512"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 9, "url": "http://nb/api/ipam/asns/9/", "asn": 64512}]
        })))
        .mount(&server)
        .await;

    let asn = client(&server).asn_by_ref(64512).await.unwrap().unwrap();
    assert_eq!(asn.asn, 64512);
}

#[tokio::test]
async fn rack_by_id_hits_detail_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/racks/12/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 12, "url": "u", "name": "r12"
        })))
        .mount(&server)
        .await;

    let rack = client(&server).rack_by_ref("12").await.unwrap().unwrap();
    assert_eq!(rack.name, "r12");
}

#[tokio::test]
async fn tenant_by_slug_uses_slug_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tenancy/tenants/"))
        .and(query_param("slug", "acme"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 4, "url": "http://nb/api/tenancy/tenants/4/",
                "name": "Acme Corp", "slug": "acme"
            }]
        })))
        .mount(&server)
        .await;

    let tenant = client(&server)
        .tenant_by_ref("acme")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tenant.id, 4);
    assert_eq!(tenant.slug, "acme");
}

#[tokio::test]
async fn tenant_by_id_hits_detail_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tenancy/tenants/9/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 9, "url": "http://nb/api/tenancy/tenants/9/", "name": "corp", "slug": "corp"
        })))
        .mount(&server)
        .await;

    let tenant = client(&server).tenant_by_ref("9").await.unwrap().unwrap();
    assert_eq!(tenant.id, 9);
    assert_eq!(tenant.name, "corp");
}

#[tokio::test]
async fn tenant_by_name_falls_back_to_name_ie_then_ic() {
    let server = MockServer::start().await;
    // slug + name__ie miss; name__ic resolves the single match.
    Mock::given(method("GET"))
        .and(path("/api/tenancy/tenants/"))
        .and(query_param("slug", "acme corp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tenancy/tenants/"))
        .and(query_param("name__ie", "acme corp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0, "next": null, "previous": null, "results": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tenancy/tenants/"))
        .and(query_param("name__ic", "acme corp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 4, "url": "u", "name": "Acme Corp", "slug": "acme"}]
        })))
        .mount(&server)
        .await;

    let tenant = client(&server)
        .tenant_by_ref("acme corp")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tenant.id, 4);
}

#[tokio::test]
async fn tenant_contains_with_multiple_matches_is_ambiguous() {
    let server = MockServer::start().await;
    mount_empty_page(&server, "/api/tenancy/tenants/", "slug", "acme").await;
    mount_empty_page(&server, "/api/tenancy/tenants/", "name__ie", "acme").await;
    Mock::given(method("GET"))
        .and(path("/api/tenancy/tenants/"))
        .and(query_param("name__ic", "acme"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "u", "name": "Acme East", "slug": "acme-east"},
                {"id": 2, "url": "u", "name": "Acme West", "slug": "acme-west"}
            ]
        })))
        .mount(&server)
        .await;

    let err = client(&server).tenant_by_ref("acme").await.unwrap_err();
    assert_eq!(nbox::error::NboxError::exit_code_for(&err), 5);
    let msg = format!("{err:#}");
    assert!(msg.contains("ambiguous"), "got: {msg}");
    assert!(
        msg.contains("Acme East") && msg.contains("Acme West"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn contact_by_name_uses_name_ie_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tenancy/contacts/"))
        .and(query_param("name__ie", "Jane Doe"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 7, "url": "http://nb/api/tenancy/contacts/7/", "name": "Jane Doe",
                "email": "jane@example.com"
            }]
        })))
        .mount(&server)
        .await;

    let contact = client(&server)
        .contact_by_ref("Jane Doe")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(contact.id, 7);
    assert_eq!(contact.email.as_deref(), Some("jane@example.com"));
}

#[tokio::test]
async fn contact_by_id_hits_detail_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tenancy/contacts/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "http://nb/api/tenancy/contacts/7/", "name": "Jane Doe"
        })))
        .mount(&server)
        .await;

    let contact = client(&server).contact_by_ref("7").await.unwrap().unwrap();
    assert_eq!(contact.id, 7);
    assert_eq!(contact.name, "Jane Doe");
}

#[tokio::test]
async fn contact_contains_with_multiple_matches_is_ambiguous() {
    let server = MockServer::start().await;
    // Contacts have no slug: exact (name__ie) misses, contains returns several.
    mount_empty_page(&server, "/api/tenancy/contacts/", "name__ie", "jane").await;
    Mock::given(method("GET"))
        .and(path("/api/tenancy/contacts/"))
        .and(query_param("name__ic", "jane"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "u", "name": "Jane Doe"},
                {"id": 2, "url": "u", "name": "Jane Roe"}
            ]
        })))
        .mount(&server)
        .await;

    let err = client(&server).contact_by_ref("jane").await.unwrap_err();
    assert_eq!(nbox::error::NboxError::exit_code_for(&err), 5);
    let msg = format!("{err:#}");
    assert!(msg.contains("ambiguous"), "got: {msg}");
    assert!(
        msg.contains("Jane Doe") && msg.contains("Jane Roe"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn provider_by_slug_uses_slug_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/circuits/providers/"))
        .and(query_param("slug", "acme-telecom"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 4, "url": "http://nb/api/circuits/providers/4/",
                "name": "ACME Telecom", "slug": "acme-telecom"
            }]
        })))
        .mount(&server)
        .await;

    let provider = client(&server)
        .provider_by_ref("acme-telecom")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(provider.id, 4);
    assert_eq!(provider.slug, "acme-telecom");
}

#[tokio::test]
async fn provider_by_id_hits_detail_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/circuits/providers/9/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 9, "url": "http://nb/api/circuits/providers/9/",
            "name": "Upstream", "slug": "upstream"
        })))
        .mount(&server)
        .await;

    let provider = client(&server).provider_by_ref("9").await.unwrap().unwrap();
    assert_eq!(provider.id, 9);
    assert_eq!(provider.name, "Upstream");
}

#[tokio::test]
async fn provider_by_name_falls_back_to_name_ie_then_ic() {
    let server = MockServer::start().await;
    // slug + name__ie miss; name__ic resolves the single match.
    mount_empty_page(&server, "/api/circuits/providers/", "slug", "acme telecom").await;
    mount_empty_page(
        &server,
        "/api/circuits/providers/",
        "name__ie",
        "acme telecom",
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/circuits/providers/"))
        .and(query_param("name__ic", "acme telecom"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 4, "url": "u", "name": "ACME Telecom", "slug": "acme-telecom"}]
        })))
        .mount(&server)
        .await;

    let provider = client(&server)
        .provider_by_ref("acme telecom")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(provider.id, 4);
}

#[tokio::test]
async fn provider_contains_with_multiple_matches_is_ambiguous() {
    let server = MockServer::start().await;
    mount_empty_page(&server, "/api/circuits/providers/", "slug", "acme").await;
    mount_empty_page(&server, "/api/circuits/providers/", "name__ie", "acme").await;
    Mock::given(method("GET"))
        .and(path("/api/circuits/providers/"))
        .and(query_param("name__ic", "acme"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "u", "name": "ACME East", "slug": "acme-east"},
                {"id": 2, "url": "u", "name": "ACME West", "slug": "acme-west"}
            ]
        })))
        .mount(&server)
        .await;

    let err = client(&server).provider_by_ref("acme").await.unwrap_err();
    assert_eq!(nbox::error::NboxError::exit_code_for(&err), 5);
    let msg = format!("{err:#}");
    assert!(msg.contains("ambiguous"), "got: {msg}");
    assert!(
        msg.contains("ACME East") && msg.contains("ACME West"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn vm_by_name_uses_name_ie_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .and(query_param("name__ie", "web-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 5, "url": "http://nb/api/virtualization/virtual-machines/5/",
                "name": "web-01",
                "status": {"value": "active", "label": "Active"}
            }]
        })))
        .mount(&server)
        .await;

    let vm = client(&server).vm_by_ref("web-01").await.unwrap().unwrap();
    assert_eq!(vm.id, 5);
    assert_eq!(vm.name, "web-01");
}

#[tokio::test]
async fn vm_by_id_hits_detail_endpoint() {
    let server = MockServer::start().await;
    // The id fast-path excludes config_context; matching on path alone is enough.
    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/5/"))
        .and(query_param("exclude", "config_context"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 5, "url": "http://nb/api/virtualization/virtual-machines/5/",
            "name": "web-01"
        })))
        .mount(&server)
        .await;

    let vm = client(&server).vm_by_ref("5").await.unwrap().unwrap();
    assert_eq!(vm.id, 5);
    assert_eq!(vm.name, "web-01");
}

#[tokio::test]
async fn vm_by_name_falls_back_to_name_ic() {
    let server = MockServer::start().await;
    // VMs have no slug: exact (name__ie) misses, contains resolves the match.
    mount_empty_page(
        &server,
        "/api/virtualization/virtual-machines/",
        "name__ie",
        "web",
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .and(query_param("name__ic", "web"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 5, "url": "u", "name": "web-01"}]
        })))
        .mount(&server)
        .await;

    let vm = client(&server).vm_by_ref("web").await.unwrap().unwrap();
    assert_eq!(vm.id, 5);
}

#[tokio::test]
async fn vm_contains_with_multiple_matches_is_ambiguous() {
    let server = MockServer::start().await;
    mount_empty_page(
        &server,
        "/api/virtualization/virtual-machines/",
        "name__ie",
        "web",
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .and(query_param("name__ic", "web"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "u", "name": "web-01"},
                {"id": 2, "url": "u", "name": "web-02"}
            ]
        })))
        .mount(&server)
        .await;

    let err = client(&server).vm_by_ref("web").await.unwrap_err();
    assert_eq!(nbox::error::NboxError::exit_code_for(&err), 5);
    let msg = format!("{err:#}");
    assert!(msg.contains("ambiguous"), "got: {msg}");
    assert!(
        msg.contains("web-01") && msg.contains("web-02"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn cluster_by_name_uses_name_ie_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/clusters/"))
        .and(query_param("name__ie", "prod"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "http://nb/api/virtualization/clusters/3/",
                "name": "prod",
                "type": {"id": 1, "display": "VMware"}
            }]
        })))
        .mount(&server)
        .await;

    let cluster = client(&server)
        .cluster_by_ref("prod")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cluster.id, 3);
    assert_eq!(cluster.name, "prod");
}

#[tokio::test]
async fn cluster_by_id_hits_detail_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/clusters/3/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 3, "url": "http://nb/api/virtualization/clusters/3/", "name": "prod"
        })))
        .mount(&server)
        .await;

    let cluster = client(&server).cluster_by_ref("3").await.unwrap().unwrap();
    assert_eq!(cluster.id, 3);
    assert_eq!(cluster.name, "prod");
}

#[tokio::test]
async fn cluster_by_name_falls_back_to_name_ic() {
    let server = MockServer::start().await;
    mount_empty_page(&server, "/api/virtualization/clusters/", "name__ie", "prod").await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/clusters/"))
        .and(query_param("name__ic", "prod"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 3, "url": "u", "name": "prod-east"}]
        })))
        .mount(&server)
        .await;

    let cluster = client(&server)
        .cluster_by_ref("prod")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cluster.id, 3);
}

#[tokio::test]
async fn cluster_contains_with_multiple_matches_is_ambiguous() {
    let server = MockServer::start().await;
    mount_empty_page(&server, "/api/virtualization/clusters/", "name__ie", "prod").await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/clusters/"))
        .and(query_param("name__ic", "prod"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "u", "name": "prod-east"},
                {"id": 2, "url": "u", "name": "prod-west"}
            ]
        })))
        .mount(&server)
        .await;

    let err = client(&server).cluster_by_ref("prod").await.unwrap_err();
    assert_eq!(nbox::error::NboxError::exit_code_for(&err), 5);
    let msg = format!("{err:#}");
    assert!(msg.contains("ambiguous"), "got: {msg}");
    assert!(
        msg.contains("prod-east") && msg.contains("prod-west"),
        "got: {msg}"
    );
}
