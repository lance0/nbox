//! Integration tests for the `nbox export prometheus-sd` structured export.
//!
//! These mock a NetBox prefix's IPs (and the assigned-device enrichment fetch)
//! and assert the emitted JSON matches the Prometheus file-SD shape, including
//! the label mapping (device name, site, role, status, tags → labels).

mod support;

use nbox::config::ProfileConfig;
use nbox::netbox::client::NetBoxClient;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use support::binary::{assert_json_stdout, run_nbox, temp_config};

fn client(server: &MockServer) -> NetBoxClient {
    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    NetBoxClient::new(&profile, None).unwrap()
}

/// Mount the prefix-resolution GET (`?prefix=<cidr>`) returning one prefix.
async fn mount_prefix(server: &MockServer, prefix_id: u64, cidr: &str) {
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("prefix", cidr))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{
                "id": prefix_id,
                "url": format!("{}/api/ipam/prefixes/{}/", server.uri(), prefix_id),
                "prefix": cidr,
                "status": {"value": "active", "label": "Active"}
            }]
        })))
        .mount(server)
        .await;
}

/// One NetBox IP-address wire row, assigned to a device interface.
fn ip_row(id: u64, address: &str, device_id: u64, device_name: &str, tags: &[&str]) -> Value {
    let tags_arr: Vec<Value> = tags
        .iter()
        .map(|t| json!({"id": 1, "name": t, "slug": t}))
        .collect();
    json!({
        "id": id,
        "url": format!("http://nb/api/ipam/ip-addresses/{}/", id),
        "address": address,
        "status": {"value": "active", "label": "Active"},
        "assigned_object_type": "dcim.interface",
        "assigned_object_id": 100 + id,
        "assigned_object": {
            "id": 100 + id,
            "name": "eth0",
            "display": "eth0",
            "device": {"id": device_id, "name": device_name, "display": device_name}
        },
        "tags": tags_arr
    })
}

#[tokio::test]
async fn prometheus_sd_groups_ips_by_device_and_labels() {
    let server = MockServer::start().await;
    let cidr = "10.0.0.0/24";
    mount_prefix(&server, 5, cidr).await;

    // Member IPs of the prefix (global table, vrf_id=null).
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("parent", cidr))
        .and(query_param("vrf_id", "null"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 3, "next": null, "previous": null,
            "results": [
                ip_row(1, "10.0.0.5/24", 1, "edge01", &["prod"]),
                ip_row(2, "10.0.0.6/24", 1, "edge01", &["prod"]),
                ip_row(3, "10.0.0.7/24", 2, "edge02", &["monitoring"]),
            ]
        })))
        .mount(&server)
        .await;

    // Device enrichment: one fetch for the distinct device ids.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01",
                 "status": {"value": "active", "label": "Active"},
                 "role": {"id": 9, "display": "router"},
                 "site": {"id": 1, "display": "iad1"},
                 "tags": [{"id": 1, "name": "prod", "slug": "prod"}]},
                {"id": 2, "url": "http://nb/api/dcim/devices/2/", "name": "edge02",
                 "status": {"value": "active", "label": "Active"},
                 "role": {"id": 9, "display": "router"},
                 "site": {"id": 1, "display": "iad1"},
                 "tags": [{"id": 2, "name": "monitoring", "slug": "monitoring"}]},
            ]
        })))
        .mount(&server)
        .await;

    let client = client(&server);
    let p = nbox::domain::detail::resolve_prefix(&client, cidr, None, &|n, v| {
        anyhow::anyhow!("no {n} matched {v}")
    })
    .await
    .unwrap();
    let vrf_id = p.vrf.as_ref().map(|v| v.id);
    let ips = client.prefix_ips(cidr, vrf_id, 5000).await.unwrap();
    assert_eq!(ips.len(), 3);

    // Run the CLI end-to-end via the binary.
    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "export".as_ref(),
        "prometheus-sd".as_ref(),
        "--prefix".as_ref(),
        cidr.as_ref(),
        "--port".as_ref(),
        "9100".as_ref(),
    ]);
    let arr = assert_json_stdout(&out);
    let groups = arr.as_array().expect("SD JSON is an array");
    assert_eq!(groups.len(), 2, "two device groups, got: {groups:?}");

    let edge01 = groups
        .iter()
        .find(|g| g["labels"]["device"] == "edge01")
        .expect("edge01 group");
    let targets = edge01["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().any(|t| t == "10.0.0.5:9100"));
    assert!(targets.iter().any(|t| t == "10.0.0.6:9100"));
    assert_eq!(edge01["labels"]["site"], "iad1");
    assert_eq!(edge01["labels"]["role"], "router");
    assert_eq!(edge01["labels"]["status"], "active");
    assert_eq!(edge01["labels"]["tags"], "prod");

    let edge02 = groups
        .iter()
        .find(|g| g["labels"]["device"] == "edge02")
        .expect("edge02 group");
    assert_eq!(edge02["targets"][0], "10.0.0.7:9100");
    assert_eq!(edge02["labels"]["tags"], "monitoring");
}

#[tokio::test]
async fn prometheus_sd_tag_path_queries_ip_addresses_by_tag() {
    let server = MockServer::start().await;

    // Tag resolution: by name.
    Mock::given(method("GET"))
        .and(path("/api/extras/tags/"))
        .and(query_param("name", "prod"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 7, "name": "prod", "slug": "prod"}]
        })))
        .mount(&server)
        .await;

    // IPs carrying the tag (slug form).
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("tag", "prod"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [ip_row(1, "10.9.0.5/24", 1, "edge01", &["prod"])]
        })))
        .mount(&server)
        .await;

    // Device enrichment.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "u", "name": "edge01",
                 "status": {"value": "active", "label": "Active"},
                 "site": {"id": 1, "display": "iad1"},
                 "tags": [{"id": 1, "name": "prod", "slug": "prod"}]},
            ]
        })))
        .mount(&server)
        .await;

    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "export".as_ref(),
        "prometheus-sd".as_ref(),
        "--tag".as_ref(),
        "prod".as_ref(),
    ]);
    let arr = assert_json_stdout(&out);
    let groups = arr.as_array().unwrap();
    assert_eq!(groups.len(), 1);
    // Default port 9100.
    assert_eq!(groups[0]["targets"][0], "10.9.0.5:9100");
    assert_eq!(groups[0]["labels"]["device"], "edge01");
    assert_eq!(groups[0]["labels"]["site"], "iad1");
}

#[tokio::test]
async fn prometheus_sd_requires_prefix_or_tag() {
    let server = MockServer::start().await;
    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "export".as_ref(),
        "prometheus-sd".as_ref(),
    ]);
    // Usage error → exit 2, clean stdout.
    assert_eq!(out.code, Some(2), "stderr: {}", out.stderr);
    assert!(out.stdout.is_empty(), "stdout must be clean on usage error");
    assert!(
        out.stderr.contains("--prefix") && out.stderr.contains("--tag"),
        "stderr should name the required flags: {}",
        out.stderr
    );
}

#[tokio::test]
async fn prometheus_sd_rejects_both_prefix_and_tag() {
    let server = MockServer::start().await;
    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "export".as_ref(),
        "prometheus-sd".as_ref(),
        "--prefix".as_ref(),
        "10.0.0.0/24".as_ref(),
        "--tag".as_ref(),
        "prod".as_ref(),
    ]);
    assert_eq!(out.code, Some(2));
    assert!(out.stdout.is_empty());
    assert!(
        out.stderr.contains("mutually exclusive"),
        "stderr: {}",
        out.stderr
    );
}

// ============================== address-list ==============================

#[tokio::test]
async fn address_list_prefix_emits_host_cidrs_deduped() {
    let server = MockServer::start().await;
    let cidr = "10.0.0.0/24";
    mount_prefix(&server, 5, cidr).await;

    // Assigned IPs of the prefix — note the interface /24 masks and a duplicate.
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("parent", cidr))
        .and(query_param("vrf_id", "null"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 3, "next": null, "previous": null,
            "results": [
                ip_row(1, "10.0.0.6/24", 1, "edge01", &[]),
                ip_row(2, "10.0.0.5/24", 1, "edge01", &[]),
                ip_row(3, "10.0.0.5/24", 1, "edge01", &[]),
            ]
        })))
        .mount(&server)
        .await;

    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "export".as_ref(),
        "address-list".as_ref(),
        "--prefix".as_ref(),
        cidr.as_ref(),
    ]);
    let arr = assert_json_stdout(&out);
    // Host entries (/32), the interface mask dropped; deduped and sorted.
    assert_eq!(arr, json!(["10.0.0.5/32", "10.0.0.6/32"]));
}

#[tokio::test]
async fn address_list_tag_includes_ips_and_prefixes() {
    let server = MockServer::start().await;

    // Tag resolution (path-only matcher — accepts whatever lookup form is used).
    Mock::given(method("GET"))
        .and(path("/api/extras/tags/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 7, "name": "prod", "slug": "prod"}]
        })))
        .mount(&server)
        .await;

    // IPs carrying the tag → host entries.
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("tag", "prod"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [ip_row(1, "10.9.0.5/24", 1, "edge01", &["prod"])]
        })))
        .mount(&server)
        .await;

    // Prefixes carrying the tag → their own CIDR.
    Mock::given(method("GET"))
        .and(path("/api/ipam/prefixes/"))
        .and(query_param("tag", "prod"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [{"id": 1, "url": "http://nb/api/ipam/prefixes/1/", "prefix": "10.9.0.0/24"}]
        })))
        .mount(&server)
        .await;

    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "export".as_ref(),
        "address-list".as_ref(),
        "--tag".as_ref(),
        "prod".as_ref(),
    ]);
    let arr = assert_json_stdout(&out);
    // Network sorts before the host within the same /24.
    assert_eq!(arr, json!(["10.9.0.0/24", "10.9.0.5/32"]));
}

#[tokio::test]
async fn address_list_plain_format_is_newline_delimited() {
    let server = MockServer::start().await;
    let cidr = "10.0.0.0/24";
    mount_prefix(&server, 5, cidr).await;
    Mock::given(method("GET"))
        .and(path("/api/ipam/ip-addresses/"))
        .and(query_param("parent", cidr))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                ip_row(1, "10.0.0.5/24", 1, "edge01", &[]),
                ip_row(2, "10.0.0.6/24", 1, "edge01", &[]),
            ]
        })))
        .mount(&server)
        .await;

    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "export".as_ref(),
        "address-list".as_ref(),
        "--prefix".as_ref(),
        cidr.as_ref(),
        "--format".as_ref(),
        "plain".as_ref(),
    ]);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(lines, vec!["10.0.0.5/32", "10.0.0.6/32"]);
}

#[tokio::test]
async fn address_list_rejects_bad_family() {
    let server = MockServer::start().await;
    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "export".as_ref(),
        "address-list".as_ref(),
        "--prefix".as_ref(),
        "10.0.0.0/24".as_ref(),
        "--family".as_ref(),
        "5".as_ref(),
    ]);
    assert_eq!(out.code, Some(2), "stderr: {}", out.stderr);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.contains("--family"), "stderr: {}", out.stderr);
}

// ============================ device-inventory ============================

/// Mount `/api/dcim/devices/` with the given query params required, returning
/// two representative devices.
async fn mount_devices(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2, "next": null, "previous": null,
            "results": [
                {"id": 2, "url": "http://nb/api/dcim/devices/2/", "name": "edge02",
                 "status": {"value": "offline", "label": "Offline"},
                 "role": {"id": 9, "display": "router"},
                 "site": {"id": 1, "display": "iad1"},
                 "device_type": {"id": 3, "display": "ASR-9001"},
                 "serial": "ZZ9", "asset_tag": "",
                 "primary_ip4": {"id": 50, "display": "10.0.0.2/24"},
                 "tags": [{"id": 1, "name": "prod", "slug": "prod"}]},
                {"id": 1, "url": "http://nb/api/dcim/devices/1/", "name": "edge01",
                 "status": {"value": "active", "label": "Active"},
                 "role": {"id": 9, "display": "router"},
                 "site": {"id": 1, "display": "iad1"},
                 "device_type": {"id": 3, "display": "ASR-9001"},
                 "serial": "AA1", "asset_tag": "m075216",
                 "primary_ip4": {"id": 51, "display": "10.0.0.1/24"},
                 "tags": [{"id": 1, "name": "prod", "slug": "prod"},
                          {"id": 2, "name": "us-east", "slug": "us-east"}]},
            ]
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn device_inventory_json_is_sorted_records() {
    let server = MockServer::start().await;
    mount_devices(&server).await;

    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "export".as_ref(),
        "device-inventory".as_ref(),
    ]);
    let arr = assert_json_stdout(&out);
    let recs = arr.as_array().expect("array");
    assert_eq!(recs.len(), 2);
    // Sorted by name: edge01 before edge02.
    assert_eq!(recs[0]["name"], "edge01");
    assert_eq!(recs[0]["status"], "active");
    assert_eq!(recs[0]["model"], "ASR-9001");
    assert_eq!(recs[0]["primary_ip"], "10.0.0.1", "mask stripped");
    assert_eq!(recs[0]["asset_tag"], "m075216");
    assert_eq!(recs[0]["tags"], json!(["prod", "us-east"]));
    assert_eq!(recs[1]["name"], "edge02");
    // An unset asset tag is null, not "".
    assert_eq!(recs[1]["asset_tag"], Value::Null);
}

#[tokio::test]
async fn device_inventory_csv_has_header_and_rows() {
    let server = MockServer::start().await;
    mount_devices(&server).await;

    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "export".as_ref(),
        "device-inventory".as_ref(),
        "--format".as_ref(),
        "csv".as_ref(),
    ]);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(
        lines[0],
        "name,status,role,site,model,platform,serial,asset_tag,rack,primary_ip,tenant,tags"
    );
    assert!(lines[1].starts_with("edge01,active,"), "{}", lines[1]);
    // Multi-tag cell is comma-joined and quoted.
    assert!(lines[1].contains("\"prod,us-east\""), "{}", lines[1]);
}

#[tokio::test]
async fn device_inventory_forwards_filters() {
    let server = MockServer::start().await;
    // The mock only matches when the site filter is forwarded as a query param.
    Mock::given(method("GET"))
        .and(path("/api/dcim/devices/"))
        .and(query_param("site", "iad1"))
        .and(query_param("status", "active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "next": null, "previous": null,
            "results": [
                {"id": 1, "url": "u", "name": "edge01",
                 "status": {"value": "active", "label": "Active"}},
            ]
        })))
        .mount(&server)
        .await;

    let config = temp_config(&server.uri());
    let out = run_nbox([
        "--config".as_ref(),
        config.path().as_os_str(),
        "--no-tui".as_ref(),
        "export".as_ref(),
        "device-inventory".as_ref(),
        "--site".as_ref(),
        "iad1".as_ref(),
        "--status".as_ref(),
        "active".as_ref(),
    ]);
    let arr = assert_json_stdout(&out);
    assert_eq!(arr.as_array().unwrap().len(), 1);
    assert_eq!(arr[0]["name"], "edge01");
}
