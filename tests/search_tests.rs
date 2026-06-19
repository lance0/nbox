//! Integration tests for the multi-endpoint search fan-out.

use nbox::config::{BackendKind, ProfileConfig};
use nbox::netbox::client::NetBoxClient;
use nbox::netbox::search::{ObjectKind, SearchFilters, SearchRequest};
use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> NetBoxClient {
    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    NetBoxClient::new(&profile, None).unwrap()
}

fn graphql_client(server: &MockServer) -> NetBoxClient {
    let profile = ProfileConfig {
        url: server.uri(),
        backend: Some(BackendKind::Graphql),
        ..Default::default()
    };
    NetBoxClient::new(&profile, None).unwrap()
}

fn empty() -> serde_json::Value {
    json!({ "count": 0, "next": null, "previous": null, "results": [] })
}

async fn mount_empty(server: &MockServer, p: &str) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(server)
        .await;
}

async fn mount_graphql_device_schema(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("__schema"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "__schema": {
                    "queryType": {
                        "fields": [{
                            "name": "device_list",
                            "args": [
                                {
                                    "name": "filters",
                                    "type": {"kind": "INPUT_OBJECT", "name": "DeviceFilter"}
                                },
                                {
                                    "name": "pagination",
                                    "type": {"kind": "INPUT_OBJECT", "name": "PaginationInput"}
                                }
                            ]
                        }]
                    }
                }
            }
        })))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("__type"))
        .and(body_string_contains("DeviceFilter"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "device": {
                    "inputFields": [
                        {
                            "name": "q",
                            "type": {"kind": "SCALAR", "name": "String"}
                        },
                        {
                            "name": "status",
                            "type": {"kind": "INPUT_OBJECT", "name": "StringLookup"}
                        }
                    ]
                }
            }
        })))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("__type"))
        .and(body_string_contains("ASNFilter"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {}
        })))
        .mount(server)
        .await;
}

async fn mount_graphql_device_result(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("device_list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "device_list": [{
                    "id": "7",
                    "name": "edge01",
                    "display": "edge01",
                    "site": {
                        "id": "9",
                        "name": "iad1",
                        "display": "iad1",
                        "slug": "iad1"
                    }
                }]
            }
        })))
        .mount(server)
        .await;
}

fn graphql_list_field(name: &str, filter: &str) -> serde_json::Value {
    json!({
        "name": name,
        "args": [
            {
                "name": "filters",
                "type": {"kind": "INPUT_OBJECT", "name": filter}
            },
            {
                "name": "pagination",
                "type": {"kind": "INPUT_OBJECT", "name": "PaginationInput"}
            }
        ]
    })
}

fn graphql_input_field(name: &str) -> serde_json::Value {
    json!({
        "name": name,
        "type": {"kind": "INPUT_OBJECT", "name": "StringLookup"}
    })
}

async fn mount_graphql_capabilities(
    server: &MockServer,
    fields: Vec<serde_json::Value>,
    first_batch: serde_json::Value,
    second_batch: serde_json::Value,
) {
    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("__schema"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "__schema": {
                    "queryType": {
                        "fields": fields
                    }
                }
            }
        })))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("__type"))
        .and(body_string_contains("DeviceFilter"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": first_batch
        })))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("__type"))
        .and(body_string_contains("ASNFilter"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": second_batch
        })))
        .mount(server)
        .await;
}

async fn mount_graphql_scope_refs(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/dcim/regions/"))
        .and(query_param("slug", "scope-ref"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 7, "url": "http://nb/api/dcim/regions/7/", "name": "scope-ref", "slug": "scope-ref"}]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 9, "url": "http://nb/api/dcim/sites/9/", "name": "iad1", "slug": "iad1"}]
        })))
        .mount(server)
        .await;
}

async fn mount_graphql_vrf_ref(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/ipam/vrfs/3/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 3,
            "url": "http://nb/api/ipam/vrfs/3/",
            "name": "blue",
            "rd": "65000:3"
        })))
        .mount(server)
        .await;
}

fn graphql_request_query(request: &wiremock::Request) -> Option<String> {
    request
        .body_json::<serde_json::Value>()
        .ok()?
        .get("query")?
        .as_str()
        .map(str::to_string)
}

fn graphql_request_for(requests: &[wiremock::Request], list_name: &str) -> serde_json::Value {
    requests
        .iter()
        .filter_map(|request| request.body_json::<serde_json::Value>().ok())
        .find(|body| {
            body.get("query")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|query| query.contains(list_name))
        })
        .unwrap_or_else(|| panic!("{list_name} query was sent"))
}

#[tokio::test]
async fn search_merges_ranks_and_dedups_across_endpoints() {
    let server = MockServer::start().await;

    // Devices: one exact-ish hit.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01",
                "site": {"id": 9, "display": "iad1"}
            }]
        })))
        .mount(&server)
        .await;
    // VLAN whose name contains the query (lower score than the exact device).
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 5, "url": "http://nb/api/ipam/vlans/5/", "vid": 10, "name": "edge01-transit"}]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/dcim/sites/").await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "edge01".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap()
        .results;

    assert_eq!(results.len(), 2);
    // Exact device match ranks first.
    assert_eq!(results[0].kind, ObjectKind::Device);
    assert_eq!(results[0].display, "edge01");
    assert_eq!(results[0].subtitle.as_deref(), Some("iad1"));
    // Web URL is derived from the API URL.
    assert_eq!(results[0].url, "http://nb/dcim/devices/1/");
    // VLAN (partial match) ranks lower.
    assert_eq!(results[1].kind, ObjectKind::Vlan);
}

#[tokio::test]
async fn graphql_backend_search_shapes_lookup_filters_and_synthesizes_urls() {
    let server = MockServer::start().await;
    mount_graphql_device_schema(&server).await;
    mount_graphql_device_result(&server).await;

    let results = graphql_client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 10,
            filters: SearchFilters {
                status: Some("active".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
        .results;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, ObjectKind::Device);
    assert_eq!(results[0].display, "edge01");
    assert_eq!(results[0].subtitle.as_deref(), Some("iad1"));
    assert_eq!(results[0].url, format!("{}/dcim/devices/7/", server.uri()));

    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|request| request.url.path() == "/graphql/"),
        "GraphQL backend should not hit REST search endpoints: {requests:#?}"
    );
    let device_query: serde_json::Value = requests
        .iter()
        .map(|request| request.body_json().unwrap())
        .find(|body: &serde_json::Value| {
            body.get("query")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|query| query.contains("device_list"))
        })
        .expect("device_list query was sent");
    assert!(
        device_query["query"]
            .as_str()
            .unwrap()
            .contains("pagination: {offset: 0, limit: 10}"),
        "query should use offset pagination when the schema advertises it: {device_query:#}"
    );
    assert_eq!(device_query["variables"]["filters"]["q"], "edge");
    assert_eq!(
        device_query["variables"]["filters"]["status"],
        json!({"exact": "STATUS_ACTIVE"})
    );
}

#[tokio::test]
async fn graphql_backend_caps_pagination_limit_to_netbox_max() {
    let server = MockServer::start().await;
    mount_graphql_device_schema(&server).await;
    mount_graphql_device_result(&server).await;

    let results = graphql_client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 5_000,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap()
        .results;

    assert_eq!(results.len(), 1);
    let requests = server.received_requests().await.unwrap();
    let device_query = requests
        .iter()
        .filter_map(graphql_request_query)
        .find(|query| query.contains("device_list"))
        .expect("device_list query was sent");
    assert!(
        device_query.contains("pagination: {offset: 0, limit: 1000}"),
        "GraphQL pagination should be capped to NetBox's max page size: {device_query}"
    );
}

#[tokio::test]
async fn graphql_backend_decode_errors_name_the_list() {
    let server = MockServer::start().await;
    mount_graphql_device_schema(&server).await;
    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("device_list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "device_list": {
                    "id": "7",
                    "name": "edge01"
                }
            }
        })))
        .mount(&server)
        .await;

    let err = graphql_client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 10,
            filters: SearchFilters::default(),
        })
        .await
        .expect_err("malformed list payload should error");

    assert!(
        format!("{err:#}").contains("deserializing GraphQL device_list rows"),
        "got: {err:#}"
    );
}

#[tokio::test]
async fn graphql_backend_propagates_graphql_errors() {
    let server = MockServer::start().await;
    mount_graphql_device_schema(&server).await;
    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("device_list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": null,
            "errors": [{"message": "field exploded"}]
        })))
        .mount(&server)
        .await;

    let err = graphql_client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 10,
            filters: SearchFilters::default(),
        })
        .await
        .expect_err("GraphQL errors should fail the search branch");

    assert!(
        format!("{err:#}").contains("field exploded"),
        "got: {err:#}"
    );
}

#[tokio::test]
async fn graphql_backend_null_data_without_errors_fails_closed() {
    let server = MockServer::start().await;
    mount_graphql_device_schema(&server).await;
    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("device_list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": null
        })))
        .mount(&server)
        .await;

    let err = graphql_client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 10,
            filters: SearchFilters::default(),
        })
        .await
        .expect_err("null GraphQL data should fail the search branch");

    assert!(
        format!("{err:#}").contains("GraphQL response did not include data"),
        "got: {err:#}"
    );
}

#[tokio::test]
async fn graphql_backend_prefix_and_cluster_use_polymorphic_scope_filters() {
    let server = MockServer::start().await;
    mount_graphql_scope_refs(&server).await;
    mount_graphql_capabilities(
        &server,
        vec![
            graphql_list_field("prefix_list", "PrefixFilter"),
            graphql_list_field("cluster_list", "ClusterFilter"),
        ],
        json!({
            "prefix": {
                "inputFields": [
                    graphql_input_field("q"),
                    graphql_input_field("scope_type"),
                    graphql_input_field("scope_id")
                ]
            }
        }),
        json!({
            "cluster": {
                "inputFields": [
                    graphql_input_field("q"),
                    graphql_input_field("scope_type"),
                    graphql_input_field("scope_id")
                ]
            }
        }),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("prefix_list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "prefix_list": [{
                    "id": "11",
                    "prefix": "10.20.0.0/16",
                    "display": "10.20.0.0/16",
                    "scope": {"id": "7", "name": "scope-ref", "display": "scope-ref", "slug": "scope-ref"}
                }]
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("cluster_list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "cluster_list": [{
                    "id": "12",
                    "name": "cluster-a",
                    "display": "cluster-a",
                    "type": null,
                    "scope": {"id": "7", "name": "scope-ref", "display": "scope-ref", "slug": "scope-ref"}
                }]
            }
        })))
        .mount(&server)
        .await;

    let results = graphql_client(&server)
        .search(SearchRequest {
            query: "scope".into(),
            limit: 10,
            filters: SearchFilters {
                region: Some("scope-ref".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
        .results;

    assert!(
        results.iter().any(|r| r.kind == ObjectKind::Prefix),
        "got: {results:?}"
    );
    assert!(
        results.iter().any(|r| r.kind == ObjectKind::Cluster),
        "got: {results:?}"
    );
    let requests = server.received_requests().await.unwrap();
    for list_name in ["prefix_list", "cluster_list"] {
        let body = graphql_request_for(&requests, list_name);
        assert_eq!(
            body["variables"]["filters"]["scope_type"],
            json!({"exact": "dcim.region"}),
            "{list_name} should carry scope_type: {body:#}"
        );
        assert_eq!(
            body["variables"]["filters"]["scope_id"],
            json!({"exact": 7}),
            "{list_name} should carry scope_id: {body:#}"
        );
    }
}

#[tokio::test]
async fn graphql_backend_ip_and_prefix_use_vrf_id_filter() {
    let server = MockServer::start().await;
    mount_graphql_vrf_ref(&server).await;
    mount_graphql_capabilities(
        &server,
        vec![
            graphql_list_field("ip_address_list", "IPAddressFilter"),
            graphql_list_field("prefix_list", "PrefixFilter"),
        ],
        json!({
            "ip": {
                "inputFields": [
                    graphql_input_field("q"),
                    graphql_input_field("vrf_id")
                ]
            },
            "prefix": {
                "inputFields": [
                    graphql_input_field("q"),
                    graphql_input_field("vrf_id")
                ]
            }
        }),
        json!({}),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("ip_address_list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "ip_address_list": [{
                    "id": "21",
                    "address": "10.0.0.10/24",
                    "display": "10.0.0.10/24",
                    "dns_name": ""
                }]
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("prefix_list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "prefix_list": [{
                    "id": "22",
                    "prefix": "10.0.0.0/24",
                    "display": "10.0.0.0/24",
                    "scope": null
                }]
            }
        })))
        .mount(&server)
        .await;

    let results = graphql_client(&server)
        .search(SearchRequest {
            query: "10.0".into(),
            limit: 10,
            filters: SearchFilters {
                vrf: Some("3".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
        .results;

    assert!(
        results.iter().any(|r| r.kind == ObjectKind::IpAddress),
        "got: {results:?}"
    );
    assert!(
        results.iter().any(|r| r.kind == ObjectKind::Prefix),
        "got: {results:?}"
    );
    let requests = server.received_requests().await.unwrap();
    for list_name in ["ip_address_list", "prefix_list"] {
        let body = graphql_request_for(&requests, list_name);
        assert_eq!(
            body["variables"]["filters"]["vrf_id"],
            json!({"exact": 3}),
            "{list_name} should carry vrf_id: {body:#}"
        );
    }
}

#[tokio::test]
async fn graphql_backend_vlan_and_vm_use_site_id_filter() {
    let server = MockServer::start().await;
    mount_graphql_scope_refs(&server).await;
    mount_graphql_capabilities(
        &server,
        vec![
            graphql_list_field("vlan_list", "VLANFilter"),
            graphql_list_field("virtual_machine_list", "VirtualMachineFilter"),
        ],
        json!({
            "vlan": {
                "inputFields": [
                    graphql_input_field("q"),
                    graphql_input_field("site_id")
                ]
            }
        }),
        json!({
            "virtualMachine": {
                "inputFields": [
                    graphql_input_field("q"),
                    graphql_input_field("site_id")
                ]
            }
        }),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("vlan_list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "vlan_list": [{
                    "id": "31",
                    "vid": 1234,
                    "name": "ci-vlan",
                    "display": "ci-vlan",
                    "site": {"id": "9", "name": "iad1", "display": "iad1", "slug": "iad1"},
                    "group": null
                }]
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql/"))
        .and(body_string_contains("virtual_machine_list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "virtual_machine_list": [{
                    "id": "32",
                    "name": "vm01",
                    "display": "vm01",
                    "cluster": null,
                    "site": {"id": "9", "name": "iad1", "display": "iad1", "slug": "iad1"}
                }]
            }
        })))
        .mount(&server)
        .await;

    let results = graphql_client(&server)
        .search(SearchRequest {
            query: "ci".into(),
            limit: 10,
            filters: SearchFilters {
                site: Some("iad1".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
        .results;

    assert!(
        results.iter().any(|r| r.kind == ObjectKind::Vlan),
        "got: {results:?}"
    );
    assert!(
        results.iter().any(|r| r.kind == ObjectKind::Vm),
        "got: {results:?}"
    );
    let requests = server.received_requests().await.unwrap();
    for list_name in ["vlan_list", "virtual_machine_list"] {
        let body = graphql_request_for(&requests, list_name);
        assert_eq!(
            body["variables"]["filters"]["site_id"],
            json!({"exact": 9}),
            "{list_name} should carry site_id: {body:#}"
        );
    }
}

#[tokio::test]
async fn graphql_backend_unsupported_filter_skips_without_unfiltered_query() {
    let server = MockServer::start().await;
    mount_graphql_capabilities(
        &server,
        vec![graphql_list_field("aggregate_list", "AggregateFilter")],
        json!({
            "aggregate": {
                "inputFields": [
                    graphql_input_field("q")
                ]
            }
        }),
        json!({}),
    )
    .await;

    let outcome = graphql_client(&server)
        .search(SearchRequest {
            query: "10".into(),
            limit: 10,
            filters: SearchFilters {
                role: Some("leaf".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(outcome.results.is_empty(), "got: {:?}", outcome.results);
    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .filter_map(graphql_request_query)
            .all(|query| !query.contains("aggregate_list")),
        "aggregate_list should be skipped instead of queried without the unsupported role filter: {requests:#?}"
    );
}

#[tokio::test]
async fn graphql_backend_caches_capabilities_across_client_clones() {
    let server = MockServer::start().await;
    mount_graphql_device_schema(&server).await;
    mount_graphql_device_result(&server).await;

    let client = graphql_client(&server);
    let cloned = client.clone();
    for client in [client, cloned] {
        let results = client
            .search(SearchRequest {
                query: "edge".into(),
                limit: 10,
                filters: SearchFilters::default(),
            })
            .await
            .unwrap()
            .results;
        assert_eq!(results.len(), 1);
    }

    let requests = server.received_requests().await.unwrap();
    let queries: Vec<String> = requests.iter().filter_map(graphql_request_query).collect();
    let introspection_queries = queries
        .iter()
        .filter(|query| query.contains("__schema") || query.contains("__type"))
        .count();
    let data_queries = queries
        .iter()
        .filter(|query| query.contains("device_list") && !query.contains("__schema"))
        .count();

    assert_eq!(
        introspection_queries, 3,
        "capability probes should run once and be shared by cloned clients: {queries:#?}"
    );
    assert_eq!(data_queries, 2, "each search still sends its data query");
}

#[tokio::test]
async fn graphql_backend_skips_scoped_branch_when_filter_is_not_in_schema() {
    let server = MockServer::start().await;
    mount_graphql_device_schema(&server).await;
    mount_graphql_device_result(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 9, "url": "http://nb/api/dcim/sites/9/", "name": "iad1", "slug": "iad1"}]
        })))
        .mount(&server)
        .await;

    let outcome = graphql_client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 10,
            filters: SearchFilters {
                site: Some("iad1".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(outcome.results.is_empty(), "got: {:?}", outcome.results);
    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .filter_map(|request| request.body_json::<serde_json::Value>().ok())
            .all(|body| body
                .get("query")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|query| !query.contains("device_list"))),
        "device_list query should be skipped when site_id is not in DeviceFilter: {requests:#?}"
    );
}

#[tokio::test]
async fn search_truncates_to_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 3, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "http://nb/api/dcim/sites/1/", "name": "site-a", "slug": "site-a"},
                {"id": 2, "url": "http://nb/api/dcim/sites/2/", "name": "site-b", "slug": "site-b"},
                {"id": 3, "url": "http://nb/api/dcim/sites/3/", "name": "site-c", "slug": "site-c"}
            ]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "site".into(),
            limit: 2,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap()
        .results;
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn search_reports_partial_endpoint_failures() {
    let server = MockServer::start().await;
    // Devices succeed; sites return a 403; the rest are empty.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap();
    // Got the device, but the sites endpoint failure is reported (not hidden).
    assert_eq!(outcome.results.len(), 1);
    assert_eq!(outcome.errors.len(), 1);
    assert!(
        outcome.errors[0].contains("sites"),
        "got: {:?}",
        outcome.errors
    );
}

#[tokio::test]
async fn search_surfaces_circuits_aggregates_asns_and_ip_ranges() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/circuits/circuits/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/circuits/circuits/1/", "cid": "edge-wan-1",
                "provider": {"id": 7, "display": "ACME"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/aggregates/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 2, "url": "http://nb/api/ipam/aggregates/2/", "prefix": "10.0.0.0/8",
                "rir": {"id": 3, "display": "RFC 1918"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/asns/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "http://nb/api/ipam/asns/3/", "asn": 64512,
                "rir": {"id": 3, "display": "RFC 6996"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-ranges/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 4, "url": "http://nb/api/ipam/ip-ranges/4/",
                "start_address": "10.0.0.10/24", "end_address": "10.0.0.20/24"
            }]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/dcim/sites/").await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap()
        .results;

    let kinds: Vec<ObjectKind> = results.iter().map(|r| r.kind).collect();
    assert!(kinds.contains(&ObjectKind::Circuit), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::Aggregate), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::Asn), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::IpRange), "got: {kinds:?}");

    let circuit = results
        .iter()
        .find(|r| r.kind == ObjectKind::Circuit)
        .unwrap();
    assert_eq!(circuit.display, "edge-wan-1");
    assert_eq!(circuit.subtitle.as_deref(), Some("ACME"));
    assert_eq!(circuit.url, "http://nb/circuits/circuits/1/");

    let asn = results.iter().find(|r| r.kind == ObjectKind::Asn).unwrap();
    assert_eq!(asn.display, "AS64512");

    let range = results
        .iter()
        .find(|r| r.kind == ObjectKind::IpRange)
        .unwrap();
    assert_eq!(range.display, "10.0.0.10/24-10.0.0.20/24");
}

#[tokio::test]
async fn search_surfaces_tenants_and_contacts() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tenancy/tenants/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/tenancy/tenants/1/",
                "name": "Acme Corp", "slug": "acme",
                "group": {"id": 5, "display": "Customers"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tenancy/contacts/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 2, "url": "http://nb/api/tenancy/contacts/2/",
                "name": "Acme NOC", "email": "noc@acme.example"
            }]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/dcim/sites/").await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "acme".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap();
    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let results = outcome.results;

    let kinds: Vec<ObjectKind> = results.iter().map(|r| r.kind).collect();
    assert!(kinds.contains(&ObjectKind::Tenant), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::Contact), "got: {kinds:?}");

    let tenant = results
        .iter()
        .find(|r| r.kind == ObjectKind::Tenant)
        .unwrap();
    assert_eq!(tenant.display, "Acme Corp");
    assert_eq!(tenant.subtitle.as_deref(), Some("Customers"));
    assert_eq!(tenant.url, "http://nb/tenancy/tenants/1/");

    let contact = results
        .iter()
        .find(|r| r.kind == ObjectKind::Contact)
        .unwrap();
    assert_eq!(contact.display, "Acme NOC");
    assert_eq!(contact.subtitle.as_deref(), Some("noc@acme.example"));
    assert_eq!(contact.url, "http://nb/tenancy/contacts/2/");
}

#[tokio::test]
async fn search_surfaces_providers() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/circuits/providers/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/circuits/providers/1/",
                "name": "ACME Telecom", "slug": "acme-telecom",
                "asns": [{"id": 5, "url": "u", "asn": 64512}]
            }]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/dcim/sites/").await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "acme".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap();
    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);

    let provider = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::Provider)
        .unwrap();
    assert_eq!(provider.display, "ACME Telecom");
    // Subtitle prefers the first AS number.
    assert_eq!(provider.subtitle.as_deref(), Some("AS64512"));
    assert_eq!(provider.url, "http://nb/circuits/providers/1/");
}

#[tokio::test]
async fn search_surfaces_vms_and_clusters() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/virtualization/virtual-machines/1/",
                "name": "prod-web-01",
                "cluster": {"id": 5, "display": "prod"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/clusters/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 2, "url": "http://nb/api/virtualization/clusters/2/",
                "name": "prod",
                "type": {"id": 1, "display": "VMware"}
            }]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/dcim/sites/").await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "prod".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap();
    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let results = outcome.results;

    let kinds: Vec<ObjectKind> = results.iter().map(|r| r.kind).collect();
    assert!(kinds.contains(&ObjectKind::Vm), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::Cluster), "got: {kinds:?}");

    let vm = results.iter().find(|r| r.kind == ObjectKind::Vm).unwrap();
    assert_eq!(vm.display, "prod-web-01");
    // VM subtitle prefers the cluster.
    assert_eq!(vm.subtitle.as_deref(), Some("prod"));
    assert_eq!(vm.url, "http://nb/virtualization/virtual-machines/1/");

    let cluster = results
        .iter()
        .find(|r| r.kind == ObjectKind::Cluster)
        .unwrap();
    assert_eq!(cluster.display, "prod");
    // Cluster subtitle prefers the type.
    assert_eq!(cluster.subtitle.as_deref(), Some("VMware"));
    assert_eq!(cluster.url, "http://nb/virtualization/clusters/2/");
}

#[tokio::test]
async fn search_matches_asn_by_number() {
    let server = MockServer::start().await;
    // A numeric query is routed to the `asn=` filter (not the text `q`), so the
    // ASN endpoint must see `asn=64512` and no `q`.
    Mock::given(method("GET"))
        .and(path("/api/ipam/asns/"))
        .and(wiremock::matchers::query_param("asn", "64512"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 3, "url": "http://nb/api/ipam/asns/3/", "asn": 64512}]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/dcim/sites/").await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "64512".into(),
            limit: 25,
            filters: SearchFilters::default(),
        })
        .await
        .unwrap()
        .results;

    let asn = results
        .iter()
        .find(|r| r.kind == ObjectKind::Asn)
        .expect("asn surfaced by number");
    assert_eq!(asn.display, "AS64512");
}

#[tokio::test]
async fn search_with_site_scopes_prefixes_by_scope_type_and_id() {
    // NetBox 4.2 dropped the prefix `site` FK for the polymorphic `scope`, so
    // `?site=` is a dead filter on prefixes. With `--site`, search resolves the
    // site to its id once and filters prefixes by `scope_type=dcim.site` +
    // `scope_id=<id>` rather than skipping the prefix endpoint entirely.
    let server = MockServer::start().await;

    // Site resolution: `site_by_ref` looks the slug up first; return id 9.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 9, "url": "http://nb/api/dcim/sites/9/", "name": "iad1", "slug": "iad1"
            }]
        })))
        .mount(&server)
        .await;

    // The prefix endpoint must carry the translated scope params, and a matching
    // prefix comes back (proving it's queried, not skipped).
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("scope_type", "dcim.site"))
        .and(query_param("scope_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 11, "url": "http://nb/api/ipam/prefixes/11/", "prefix": "10.1.0.0/24",
                "scope_type": "dcim.site", "scope": {"id": 9, "display": "iad1"}
            }]
        })))
        .mount(&server)
        .await;

    // The site-search branch also hits `/api/dcim/sites/` (with `q=`, no `slug`);
    // give it an empty page so the resolution mock above stays unambiguous.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("q", "10.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(&server)
        .await;

    // Devices/VLANs/VMs filter by the RESOLVED `site_id`, never the slug-only
    // `?site=` (which would silently miss an id/display-name `--site`). Each comes
    // back with a hit, proving it's queried with `site_id` and surfaced.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "10.1-edge",
                "site": {"id": 9, "display": "iad1"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 5, "url": "http://nb/api/ipam/vlans/5/", "vid": 101, "name": "10.1-vlan",
                "site": {"id": 9, "display": "iad1"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 7, "url": "http://nb/api/virtualization/virtual-machines/7/",
                "name": "10.1-vm", "site": {"id": 9, "display": "iad1"}
            }]
        })))
        .mount(&server)
        .await;
    // Clusters honor `--site` via the polymorphic scope; give an empty page.
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "10.1".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("iad1".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
        .results;

    let prefix = results
        .iter()
        .find(|r| r.kind == ObjectKind::Prefix)
        .expect("site-scoped prefix surfaced");
    assert_eq!(prefix.display, "10.1.0.0/24");
    assert_eq!(prefix.subtitle.as_deref(), Some("iad1"));
    // The device/VLAN/VM hits prove `site_id` filtering reaches them (the bug was
    // them silently missing when `--site` wasn't a slug).
    let device = results
        .iter()
        .find(|r| r.kind == ObjectKind::Device)
        .expect("site-scoped device surfaced");
    assert_eq!(device.display, "10.1-edge");
    let vlan = results
        .iter()
        .find(|r| r.kind == ObjectKind::Vlan)
        .expect("site-scoped VLAN surfaced");
    assert_eq!(vlan.display, "101 10.1-vlan");
    let vm = results
        .iter()
        .find(|r| r.kind == ObjectKind::Vm)
        .expect("site-scoped VM surfaced");
    assert_eq!(vm.display, "10.1-vm");
}

#[tokio::test]
async fn search_with_unknown_site_errors_not_found_not_empty() {
    // An unknown `--site` must fail with a typed not-found (exit 4), not quietly
    // return an empty result set — site resolution happens before the fan-out.
    let server = MockServer::start().await;

    // Every site lookup (`slug`, `name__ie`, `name__ic`) comes back empty, so the
    // site can't be resolved.
    mount_empty(&server, "/api/dcim/sites/").await;

    let err = client(&server)
        .search(SearchRequest {
            query: "10.1".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("nope".into()),
                ..Default::default()
            },
        })
        .await
        .expect_err("unknown site should error, not return empty");

    assert_eq!(nbox::error::NboxError::exit_code_for(&err), 4);
    assert!(
        format!("{err:#}").contains("no site matched \"nope\""),
        "got: {err:#}"
    );
}

#[tokio::test]
async fn search_skips_non_site_endpoints_unchanged_with_active_site() {
    // The allowlist/skip behavior for endpoints that genuinely can't honor
    // `--site` (IPs, aggregates, ASNs, …) is unchanged: they are skipped, so
    // their endpoints are never hit. Mount ONLY the endpoints that should be
    // reached; an unexpected request to a skipped endpoint would 404 and surface
    // as a partial failure (asserted absent below).
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 9, "url": "http://nb/api/dcim/sites/9/", "name": "iad1", "slug": "iad1"
            }]
        })))
        .mount(&server)
        .await;
    // Endpoints that DO honor `--site` (directly or via scope) are reached.
    mount_empty(&server, "/api/dcim/devices/").await; // accepts `site`
    mount_empty(&server, "/api/ipam/vlans/").await; // accepts `site`
    mount_empty(&server, "/api/ipam/prefixes/").await; // scope-filtered
    mount_empty(&server, "/api/virtualization/virtual-machines/").await; // accepts `site`
    mount_empty(&server, "/api/virtualization/clusters/").await; // accepts `site`
    // The site-search branch (`q=` lookup) is reached too; fall through to a
    // catch-all empty page for `/api/dcim/sites/` so it doesn't 404.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(&server)
        .await;

    // No mocks for ip-addresses / circuits / aggregates / asns / ip-ranges:
    // those are skipped because they can't honor `--site`. If the skip logic
    // regressed, they'd be requested, 404, and show up in `outcome.errors`.
    let outcome = client(&server)
        .search(SearchRequest {
            query: "x".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("iad1".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(
        outcome.errors.is_empty(),
        "no endpoint should have been hit that can't honor --site; errors: {:?}",
        outcome.errors
    );
}

/// Shared helper: a scope flag resolves its ref to an id and the prefix request
/// carries `scope_type=<content_type>` + `scope_id`. `endpoint`/`content_type`
/// vary per scope kind; `filters` selects which flag is set.
async fn assert_scope_filters_prefixes(endpoint: &str, content_type: &str, filters: SearchFilters) {
    let server = MockServer::start().await;

    // Scope resolution: `*_by_ref` looks the slug up first; return id 7.
    Mock::given(method("GET"))
        .and(path(endpoint))
        .and(query_param("slug", "scope-ref"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 7, "url": "http://nb/api/.../7/", "name": "Scope Ref", "slug": "scope-ref"
            }]
        })))
        .mount(&server)
        .await;
    // Catch-all for the scope endpoint so other lookups don't 404.
    mount_empty(&server, endpoint).await;

    // The prefix endpoint must carry the translated scope params, and a matching
    // prefix comes back (proving it's queried, not skipped).
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("scope_type", content_type))
        .and(query_param("scope_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 11, "url": "http://nb/api/ipam/prefixes/11/", "prefix": "10.2.0.0/24",
                "scope_type": content_type, "scope": {"id": 7, "display": "Scope Ref"}
            }]
        })))
        .mount(&server)
        .await;

    // Devices + clusters honor region/site-group/location scopes; give them empty
    // pages so the fan-out doesn't 404. Other endpoints are skipped.
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let c = client(&server);
    // Box the fan-out future to keep it off the stack (clippy::large_futures).
    let results = Box::pin(c.search(SearchRequest {
        query: "10.2".into(),
        limit: 25,
        filters,
    }))
    .await
    .unwrap()
    .results;

    let prefix = results
        .iter()
        .find(|r| r.kind == ObjectKind::Prefix)
        .expect("scope-filtered prefix surfaced");
    assert_eq!(prefix.display, "10.2.0.0/24");
}

#[tokio::test]
async fn search_with_region_scopes_prefixes_by_scope_type_and_id() {
    assert_scope_filters_prefixes(
        "/api/dcim/regions/",
        "dcim.region",
        SearchFilters {
            region: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_site_group_scopes_prefixes_by_scope_type_and_id() {
    assert_scope_filters_prefixes(
        "/api/dcim/site-groups/",
        "dcim.sitegroup",
        SearchFilters {
            site_group: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_location_scopes_prefixes_by_scope_type_and_id() {
    assert_scope_filters_prefixes(
        "/api/dcim/locations/",
        "dcim.location",
        SearchFilters {
            location: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

/// Shared helper: an unknown scope ref must fail with a typed not-found (exit 4),
/// not a silent-empty result — resolution happens before the fan-out.
async fn assert_unknown_scope_is_not_found(endpoint: &str, noun: &str, filters: SearchFilters) {
    let server = MockServer::start().await;
    // Every lookup (`slug`, `name__ie`, `name__ic`) comes back empty.
    mount_empty(&server, endpoint).await;

    let c = client(&server);
    // Box the fan-out future to keep it off the stack (clippy::large_futures).
    let err = Box::pin(c.search(SearchRequest {
        query: "10.2".into(),
        limit: 25,
        filters,
    }))
    .await
    .expect_err("unknown scope ref should error, not return empty");

    assert_eq!(nbox::error::NboxError::exit_code_for(&err), 4);
    assert!(
        format!("{err:#}").contains(&format!("no {noun} matched \"nope\"")),
        "got: {err:#}"
    );
}

#[tokio::test]
async fn search_with_unknown_region_errors_not_found() {
    assert_unknown_scope_is_not_found(
        "/api/dcim/regions/",
        "region",
        SearchFilters {
            region: Some("nope".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_unknown_site_group_errors_not_found() {
    assert_unknown_scope_is_not_found(
        "/api/dcim/site-groups/",
        "site group",
        SearchFilters {
            site_group: Some("nope".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_unknown_location_errors_not_found() {
    assert_unknown_scope_is_not_found(
        "/api/dcim/locations/",
        "location",
        SearchFilters {
            location: Some("nope".into()),
            ..Default::default()
        },
    )
    .await;
}

/// Shared helper: a scope flag also filters CLUSTERS by `scope_type`+`scope_id`
/// (NetBox 4.2+ scopes a cluster polymorphically, same as a prefix).
async fn assert_scope_filters_clusters(endpoint: &str, content_type: &str, filters: SearchFilters) {
    let server = MockServer::start().await;

    // Scope resolution: `*_by_ref` looks the slug up first; return id 7.
    Mock::given(method("GET"))
        .and(path(endpoint))
        .and(query_param("slug", "scope-ref"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 7, "url": "http://nb/api/.../7/", "name": "Scope Ref", "slug": "scope-ref"
            }]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, endpoint).await;

    // The cluster endpoint must carry the translated scope params, and a matching
    // cluster comes back (proving it's queried, not skipped).
    Mock::given(method("GET"))
        .and(path("/api/virtualization/clusters/"))
        .and(query_param("scope_type", content_type))
        .and(query_param("scope_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 12, "url": "http://nb/api/virtualization/clusters/12/", "name": "prod",
                "scope_type": content_type, "scope": {"id": 7, "display": "Scope Ref"}
            }]
        })))
        .mount(&server)
        .await;

    // Prefixes + devices also honor the scope; give them empty pages so the
    // fan-out doesn't 404. Everything else is skipped.
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/dcim/devices/").await;

    let c = client(&server);
    // Box the fan-out future to keep it off the stack (clippy::large_futures).
    let results = Box::pin(c.search(SearchRequest {
        query: "prod".into(),
        limit: 25,
        filters,
    }))
    .await
    .unwrap()
    .results;

    let cluster = results
        .iter()
        .find(|r| r.kind == ObjectKind::Cluster)
        .expect("scope-filtered cluster surfaced");
    assert_eq!(cluster.display, "prod");
}

#[tokio::test]
async fn search_with_region_scopes_clusters_by_scope_type_and_id() {
    assert_scope_filters_clusters(
        "/api/dcim/regions/",
        "dcim.region",
        SearchFilters {
            region: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_site_group_scopes_clusters_by_scope_type_and_id() {
    assert_scope_filters_clusters(
        "/api/dcim/site-groups/",
        "dcim.sitegroup",
        SearchFilters {
            site_group: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_location_scopes_clusters_by_scope_type_and_id() {
    assert_scope_filters_clusters(
        "/api/dcim/locations/",
        "dcim.location",
        SearchFilters {
            location: Some("scope-ref".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn search_with_two_scope_filters_is_a_usage_error() {
    // NetBox prefix scope is a single type+id, so combining scope flags is a
    // usage error (exit 2) — surfaced before any endpoint is hit. No mocks
    // mounted: a request to any endpoint would 404 and prove the early bail-out
    // didn't run.
    let server = MockServer::start().await;

    let err = client(&server)
        .search(SearchRequest {
            query: "10.2".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("iad1".into()),
                region: Some("us-east".into()),
                ..Default::default()
            },
        })
        .await
        .expect_err("two scope filters should be a usage error");

    assert_eq!(nbox::error::NboxError::exit_code_for(&err), 2);
    assert!(
        format!("{err:#}").contains("mutually exclusive"),
        "got: {err:#}"
    );
}

#[tokio::test]
async fn search_with_vrf_filters_ip_and_prefix_by_vrf_id() {
    // `--vrf` resolves the ref to an id once, then filters the VRF-capable
    // endpoints (IPs, prefixes) by `vrf_id=`. VRF-incapable endpoints (devices,
    // sites, …) are not vrf-filtered — they're queried with `q` only.
    let server = MockServer::start().await;

    // VRF resolution: a non-numeric ref tries `rd` first (VRFs have no slug);
    // return id 7. A catch-all keeps the later name fallbacks from 404ing.
    Mock::given(method("GET"))
        .and(path("/api/ipam/vrfs/"))
        .and(query_param("rd", "blue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 7, "url": "http://nb/api/ipam/vrfs/7/", "name": "blue", "rd": "blue"
            }]
        })))
        .mount(&server)
        .await;

    // IPs carry the vrf filter and a matching IP comes back (proving it's
    // applied, not dropped).
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 21, "url": "http://nb/api/ipam/ip-addresses/21/", "address": "10.0.0.1/24",
                "vrf": {"id": 7, "display": "blue"}
            }]
        })))
        .mount(&server)
        .await;
    // Prefixes carry the vrf filter too.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 31, "url": "http://nb/api/ipam/prefixes/31/", "prefix": "10.0.0.0/24",
                "vrf": {"id": 7, "display": "blue"}
            }]
        })))
        .mount(&server)
        .await;

    // VRF-incapable endpoints are queried WITHOUT a vrf filter (matched on `q`).
    // A device hit here must NOT carry `vrf_id`; mount it on the plain `q` query
    // so a regression that vrf-filtered devices would 404 instead.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("q", "10.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "10.0-edge"}]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/dcim/sites/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "10.0".into(),
            limit: 25,
            filters: SearchFilters {
                vrf: Some("blue".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(
        outcome.errors.is_empty(),
        "no vrf filter should leak onto a vrf-incapable endpoint; errors: {:?}",
        outcome.errors
    );
    let ip = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::IpAddress)
        .expect("vrf-filtered IP surfaced");
    assert_eq!(ip.display, "10.0.0.1/24");
    let prefix = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::Prefix)
        .expect("vrf-filtered prefix surfaced");
    assert_eq!(prefix.display, "10.0.0.0/24");
    // The device (vrf-incapable) still surfaces — it isn't vrf-filtered away.
    assert!(
        outcome.results.iter().any(|r| r.kind == ObjectKind::Device),
        "vrf-incapable device should still surface"
    );
}

#[tokio::test]
async fn search_with_vrf_resolved_by_id_filters_prefixes() {
    // A numeric `--vrf` resolves straight off the detail endpoint, then filters.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/ipam/vrfs/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "http://nb/api/ipam/vrfs/7/", "name": "blue", "rd": "65000:7"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 31, "url": "http://nb/api/ipam/prefixes/31/", "prefix": "10.0.0.0/24",
                "vrf": {"id": 7, "display": "blue"}
            }]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/ipam/ip-addresses/").await;
    mount_empty(&server, "/api/dcim/devices/").await;
    mount_empty(&server, "/api/dcim/sites/").await;
    mount_empty(&server, "/api/ipam/vlans/").await;
    mount_empty(&server, "/api/circuits/circuits/").await;
    mount_empty(&server, "/api/ipam/aggregates/").await;
    mount_empty(&server, "/api/ipam/asns/").await;
    mount_empty(&server, "/api/ipam/ip-ranges/").await;
    mount_empty(&server, "/api/tenancy/tenants/").await;
    mount_empty(&server, "/api/tenancy/contacts/").await;
    mount_empty(&server, "/api/circuits/providers/").await;
    mount_empty(&server, "/api/virtualization/virtual-machines/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "10.0".into(),
            limit: 25,
            filters: SearchFilters {
                vrf: Some("7".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
        .results;

    let prefix = results
        .iter()
        .find(|r| r.kind == ObjectKind::Prefix)
        .expect("vrf-filtered prefix surfaced");
    assert_eq!(prefix.display, "10.0.0.0/24");
}

#[tokio::test]
async fn search_with_unknown_vrf_errors_not_found_not_empty() {
    // An unknown `--vrf` must fail with a typed not-found (exit 4), not quietly
    // return an empty result set — VRF resolution happens before the fan-out.
    let server = MockServer::start().await;

    // Every VRF lookup (`rd`, `name__ie`, `name__ic`) comes back empty.
    mount_empty(&server, "/api/ipam/vrfs/").await;

    let err = client(&server)
        .search(SearchRequest {
            query: "10.0".into(),
            limit: 25,
            filters: SearchFilters {
                vrf: Some("nope".into()),
                ..Default::default()
            },
        })
        .await
        .expect_err("unknown vrf should error, not return empty");

    assert_eq!(nbox::error::NboxError::exit_code_for(&err), 4);
    assert!(
        format!("{err:#}").contains("no VRF matched \"nope\""),
        "got: {err:#}"
    );
}

#[tokio::test]
async fn search_combines_vrf_and_site_scope_on_prefixes() {
    // `--vrf` is orthogonal to `--site`: prefixes carry BOTH `scope_*` and
    // `vrf_id` (NetBox ANDs them); other endpoints honor only what they can.
    let server = MockServer::start().await;

    // Site resolution → id 9.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("slug", "iad1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 9, "url": "http://nb/api/dcim/sites/9/", "name": "iad1", "slug": "iad1"}]
        })))
        .mount(&server)
        .await;
    // VRF resolution (by id) → id 7.
    Mock::given(method("GET"))
        .and(path("/api/ipam/vrfs/7/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "url": "http://nb/api/ipam/vrfs/7/", "name": "blue"
        })))
        .mount(&server)
        .await;

    // Prefixes must carry scope_type/scope_id AND vrf_id together.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("scope_type", "dcim.site"))
        .and(query_param("scope_id", "9"))
        .and(query_param("vrf_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 31, "url": "http://nb/api/ipam/prefixes/31/", "prefix": "10.0.0.0/24",
                "scope_type": "dcim.site", "scope": {"id": 9, "display": "iad1"},
                "vrf": {"id": 7, "display": "blue"}
            }]
        })))
        .mount(&server)
        .await;

    // The site-search branch hits `/api/dcim/sites/` with `q=`; empty page so the
    // slug resolution above stays unambiguous.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("q", "10.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(&server)
        .await;
    // Devices/VLANs/VMs filter by the resolved `site_id` (not the slug `?site=`);
    // IPs skip on site since they can't carry `--site`. Clusters honor it via the
    // polymorphic scope.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let results = client(&server)
        .search(SearchRequest {
            query: "10.0".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("iad1".into()),
                vrf: Some("7".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
        .results;

    let prefix = results
        .iter()
        .find(|r| r.kind == ObjectKind::Prefix)
        .expect("vrf+site-scoped prefix surfaced");
    assert_eq!(prefix.display, "10.0.0.0/24");
}

#[tokio::test]
async fn search_region_scope_skips_non_prefix_non_device_non_cluster_endpoints() {
    // An id-based scope (region) has no clean filter on IPs/sites/circuits/…, so
    // those endpoints are skipped (never hit). Only the region lookup, the prefix
    // endpoint, the device endpoint, and the cluster endpoint are mounted (the
    // latter three honor the region scope); an unexpected request to a skipped
    // endpoint would 404 and surface as a partial failure.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/regions/"))
        .and(query_param("slug", "us-east"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": 3, "url": "http://nb/api/dcim/regions/3/", "name": "US East", "slug": "us-east"
            }]
        })))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/ipam/prefixes/").await; // scope-filtered
    mount_empty(&server, "/api/dcim/devices/").await; // region_id-filtered
    mount_empty(&server, "/api/virtualization/clusters/").await; // scope-filtered

    let outcome = client(&server)
        .search(SearchRequest {
            query: "x".into(),
            limit: 25,
            filters: SearchFilters {
                region: Some("us-east".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(
        outcome.errors.is_empty(),
        "no endpoint should have been hit that can't honor --region; errors: {:?}",
        outcome.errors
    );
}

/// Regression (H3): `search --site <id>` must filter devices/VLANs/VMs by the
/// RESOLVED `site_id=<id>`, never the slug-only `?site=<id>` (which silently
/// matches nothing — a numeric `--site` is an id, not a slug). The numeric ref is
/// resolved straight off the site detail endpoint, then applied as `site_id`.
#[tokio::test]
async fn search_with_numeric_site_filters_devices_vlans_vms_by_site_id() {
    let server = MockServer::start().await;

    // Numeric `--site` → resolved via the detail endpoint (id 9).
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/9/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 9, "url": "http://nb/api/dcim/sites/9/", "name": "iad1", "slug": "iad1"
        })))
        .mount(&server)
        .await;

    // Devices/VLANs/VMs each carry `site_id=9` and a hit comes back — proving the
    // resolved id reaches them (the bug was a raw `site=9` slug query missing all).
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 5, "url": "http://nb/api/ipam/vlans/5/", "vid": 10, "name": "edge"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 7, "url": "http://nb/api/virtualization/virtual-machines/7/", "name": "edge-vm"}]
        })))
        .mount(&server)
        .await;
    // Prefixes + clusters honor `--site` via the polymorphic scope (empty here).
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;
    // The site-search branch hits `/api/dcim/sites/` with `q=` (no detail id).
    mount_empty(&server, "/api/dcim/sites/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("9".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let kinds: Vec<ObjectKind> = outcome.results.iter().map(|r| r.kind).collect();
    assert!(kinds.contains(&ObjectKind::Device), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::Vlan), "got: {kinds:?}");
    assert!(kinds.contains(&ObjectKind::Vm), "got: {kinds:?}");
}

/// Regression (H3): `search --site <display-name>` resolves the name to an id and
/// filters devices/VLANs/VMs by `site_id`, never the slug-only `?site=<name>`.
#[tokio::test]
async fn search_with_site_name_filters_devices_by_site_id() {
    let server = MockServer::start().await;

    // A display-name `--site`: slug + exact miss, `name__ic` resolves to id 9.
    for key in ["slug", "name__ie"] {
        Mock::given(method("GET"))
            .and(path("/api/dcim/sites/"))
            .and(query_param(key, "IAD One"))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("name__ic", "IAD One"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 9, "url": "http://nb/api/dcim/sites/9/", "name": "IAD One", "slug": "iad1"}]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/vlans/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/virtualization/virtual-machines/"))
        .and(query_param("site_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty()))
        .mount(&server)
        .await;
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;
    // The site-search branch (`q=`) — catch-all empty page.
    mount_empty(&server, "/api/dcim/sites/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 25,
            filters: SearchFilters {
                site: Some("IAD One".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let device = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::Device)
        .expect("name-resolved site filters devices by site_id");
    assert_eq!(device.display, "edge01");
}

/// Regression (H3): `search --region <ref>` filters DEVICES by the resolved
/// `region_id` (devices expose `region_id`/`site_group_id`/`location_id` cleanly),
/// not a raw `region=` value. Confirms the id-based scopes also use `*_id`.
#[tokio::test]
async fn search_with_region_filters_devices_by_region_id() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/dcim/regions/"))
        .and(query_param("slug", "us-east"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 3, "url": "http://nb/api/dcim/regions/3/", "name": "US East", "slug": "us-east"}]
        })))
        .mount(&server)
        .await;
    // The device endpoint must carry `region_id=3` (not a raw `region=`).
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("region_id", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01"}]
        })))
        .mount(&server)
        .await;
    // Prefixes + clusters honor a region scope; empty pages keep the fan-out clean.
    mount_empty(&server, "/api/ipam/prefixes/").await;
    mount_empty(&server, "/api/virtualization/clusters/").await;

    let outcome = client(&server)
        .search(SearchRequest {
            query: "edge".into(),
            limit: 25,
            filters: SearchFilters {
                region: Some("us-east".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
    let device = outcome
        .results
        .iter()
        .find(|r| r.kind == ObjectKind::Device)
        .expect("region-scoped device filtered by region_id");
    assert_eq!(device.display, "edge01");
}
