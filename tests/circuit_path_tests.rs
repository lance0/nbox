//! Integration tests for the circuit cable-path walker, exercised through the
//! shared `detail::circuit_view_by_ref` (the path the CLI/MCP/TUI use). Circuit
//! terminations have no `/trace/` endpoint in NetBox, so nbox walks the chain
//! itself: termination → cable → (patch panel rear↔front) → device. These cover
//! the two-segment resolution and the graceful dead-end when a panel isn't wired.

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

fn page(results: Vec<Value>) -> Value {
    json!({ "count": results.len(), "next": null, "previous": null, "results": results })
}

/// Mount the circuit lookup (`?cid=`) returning one circuit with the given id.
async fn mount_circuit(server: &MockServer, id: u64, cid: &str) {
    Mock::given(method("GET"))
        .and(path("/api/circuits/circuits/"))
        .and(query_param("cid", cid))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![json!({
            "id": id, "url": "u", "cid": cid,
            "provider": {"id": 1, "display": "ACME"},
            "type": {"id": 2, "display": "Direct Connect"},
            "status": {"value": "active", "label": "Active"},
            "commit_rate": 10_000_000
        })])))
        .mount(server)
        .await;
}

#[tokio::test]
async fn circuit_path_walks_through_a_wired_panel_to_the_device() {
    let server = MockServer::start().await;
    mount_circuit(&server, 7, "ACME-1").await;

    // A-side lands on a panel rear-port (via the termination's cable); Z-side is a
    // provider network (no cable).
    Mock::given(method("GET"))
        .and(path("/api/circuits/circuit-terminations/"))
        .and(query_param("circuit_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            json!({
                "id": 10, "term_side": "A",
                "termination": {"id": 1, "display": "DC1", "name": "DC1"},
                "termination_type": "dcim.site",
                "cable": {"id": 100, "display": "#100"},
                "link_peers_type": "dcim.rearport",
                "link_peers": [
                    {"id": 50, "url": "http://nb/api/dcim/rear-ports/50/", "name": "R1",
                     "device": {"id": 9, "name": "panel-1"}}
                ]
            }),
            json!({
                "id": 11, "term_side": "Z",
                "termination": {"id": 2, "display": "ACME Cloud"},
                "termination_type": "circuits.providernetwork",
                "link_peers": []
            }),
        ])))
        .mount(&server)
        .await;

    // The panel: front-port F1 maps to rear-port 50 and is cabled (#200) onward to
    // a real router interface.
    Mock::given(method("GET"))
        .and(path("/api/dcim/front-ports/"))
        .and(query_param("device_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![json!({
            "id": 60, "url": "http://nb/api/dcim/front-ports/60/", "name": "F1",
            "device": {"id": 9, "name": "panel-1"},
            "rear_port": {"id": 50},
            "cable": {"id": 200, "display": "#200"},
            "link_peers_type": "dcim.interface",
            "link_peers": [
                {"id": 70, "url": "http://nb/api/dcim/interfaces/70/", "name": "et-0/0/1",
                 "device": {"id": 8, "name": "edge-1"}}
            ]
        })])))
        .mount(&server)
        .await;

    let view = detail::circuit_view_by_ref(&client(&server), "ACME-1", &not_found)
        .await
        .unwrap();

    // A-side path has two segments: panel (rear R1) then the router interface.
    let a = &view.terminations[0];
    assert_eq!(a.side, "A");
    assert_eq!(
        a.path.len(),
        2,
        "expected two cable segments, got {:?}",
        a.path
    );
    assert_eq!(a.path[0].to, "panel-1 R1");
    assert_eq!(a.path[0].cable.as_deref(), Some("#100"));
    assert!(!a.path[0].endpoint);
    assert_eq!(a.path[1].to, "edge-1 et-0/0/1");
    assert_eq!(a.path[1].cable.as_deref(), Some("#200"));
    assert!(
        a.path[1].endpoint,
        "the router interface is the resolved endpoint"
    );

    // Z-side (provider network) has no cabled path.
    assert_eq!(view.terminations[1].side, "Z");
    assert!(view.terminations[1].path.is_empty());

    // The diagram draws both segments under A.
    assert!(
        view.diagram
            .iter()
            .any(|l| l.contains("↳ panel-1 R1  ·  #100"))
    );
    assert!(
        view.diagram
            .iter()
            .any(|l| l.contains("↳ edge-1 et-0/0/1  ·  #200"))
    );
}

#[tokio::test]
async fn circuit_path_dead_ends_gracefully_at_an_unwired_panel() {
    let server = MockServer::start().await;
    mount_circuit(&server, 8, "ACME-2").await;

    Mock::given(method("GET"))
        .and(path("/api/circuits/circuit-terminations/"))
        .and(query_param("circuit_id", "8"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![json!({
            "id": 12, "term_side": "A",
            "termination": {"id": 1, "display": "DC1", "name": "DC1"},
            "termination_type": "dcim.site",
            "cable": {"id": 100, "display": "#100"},
            "link_peers_type": "dcim.rearport",
            "link_peers": [
                {"id": 50, "url": "http://nb/api/dcim/rear-ports/50/", "name": "R1",
                 "device": {"id": 9, "name": "panel-1"}}
            ]
        })])))
        .mount(&server)
        .await;

    // The panel has front-ports, but NONE map to rear-port 50 (unwired panel).
    Mock::given(method("GET"))
        .and(path("/api/dcim/front-ports/"))
        .and(query_param("device_id", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![json!({
            "id": 61, "url": "http://nb/api/dcim/front-ports/61/", "name": "F2",
            "device": {"id": 9, "name": "panel-1"},
            "rear_port": null,
            "link_peers": []
        })])))
        .mount(&server)
        .await;

    let view = detail::circuit_view_by_ref(&client(&server), "ACME-2", &not_found)
        .await
        .unwrap();

    // The walk stops at the panel — one segment, not fabricated past the dead-end.
    let a = &view.terminations[0];
    assert_eq!(
        a.path.len(),
        1,
        "should stop at the unwired panel: {:?}",
        a.path
    );
    assert_eq!(a.path[0].to, "panel-1 R1");
    assert!(!a.path[0].endpoint);
}
