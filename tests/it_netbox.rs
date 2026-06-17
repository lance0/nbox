//! Gated end-to-end integration tests against a LIVE NetBox 4.2.x.
//!
//! These run the *real* compiled binary (`env!("CARGO_BIN_EXE_nbox")`) against a
//! running NetBox seeded by `tests/integration/seed.py`. They catch what wiremock
//! can't: polymorphic scope filters, pagination offset windows, available-prefix
//! shapes, and the serializer/detail-model shapes of the real API.
//!
//! Every test is `#[ignore]`, so plain `cargo test` SKIPS them — the offline
//! suite stays green and unchanged. The `netbox-integration` workflow boots the
//! fixture, seeds it, and runs `cargo test --test it_netbox -- --ignored`.
//!
//! Run locally:
//!   docker compose -f tests/integration/docker-compose.yml up -d
//!   ./tests/integration/wait-for-ready.sh
//!   ./tests/integration/seed.py
//!   NBOX_URL=http://localhost:8000 \
//!     NETBOX_TOKEN=0123456789abcdef0123456789abcdef0fedcba9 \
//!     cargo test --test it_netbox -- --ignored
//!
//! Config: each invocation writes a throwaway profile pointing at `NBOX_URL` and
//! passes the token via `NBOX_TOKEN` (the direct override `connect()` reads
//! first). The token comes from `NETBOX_TOKEN`, matching the seed + compose +
//! workflow. A separate low-`page_size` profile drives the pagination case.

use std::io::Write;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::NamedTempFile;

/// The seeded API token's default — kept in sync with `docker-compose.yml`'s
/// `SUPERUSER_API_TOKEN` and `seed.py`. `NETBOX_TOKEN` overrides it.
const DEFAULT_TOKEN: &str = "0123456789abcdef0123456789abcdef0fedcba9";

/// Read `NBOX_URL`, or fall back to the compose default host port.
fn netbox_url() -> String {
    std::env::var("NBOX_URL").unwrap_or_else(|_| "http://localhost:8000".to_string())
}

/// Read `NETBOX_TOKEN`, or fall back to the seeded default.
fn netbox_token() -> String {
    std::env::var("NETBOX_TOKEN").unwrap_or_else(|_| DEFAULT_TOKEN.to_string())
}

/// A throwaway config file holding one profile that points at the live NetBox.
/// `page_size` is configurable so the pagination test can force multiple pages.
/// The `NamedTempFile` is returned so it lives as long as the test needs it.
fn temp_config(page_size: usize) -> NamedTempFile {
    let mut config = NamedTempFile::new().expect("create temp config");
    write!(
        config,
        "active_profile = \"ci\"\n\
         \n\
         [profiles.ci]\n\
         url = \"{url}\"\n\
         token_env = \"NETBOX_TOKEN_UNUSED\"\n\
         page_size = {page_size}\n\
         verify_tls = false\n",
        url = netbox_url(),
    )
    .expect("write temp config");
    config.flush().expect("flush temp config");
    config
}

/// Run `nbox <args>` against the live instance with the given page size, using a
/// throwaway profile and `NBOX_TOKEN` for auth. Returns the captured output.
fn run_nbox_with_page_size(page_size: usize, args: &[&str]) -> Output {
    let config = temp_config(page_size);
    let output = Command::new(env!("CARGO_BIN_EXE_nbox"))
        .arg("--config")
        .arg(config.path())
        .args(args)
        // `NBOX_TOKEN` is the highest-priority token source in `resolve_token`,
        // so the bogus `token_env` above is never consulted.
        .env("NBOX_TOKEN", netbox_token())
        .env_remove("NBOX_LOG")
        .env_remove("RUST_LOG")
        .output()
        .expect("spawn nbox");
    // `config` (the temp file) drops here, after the child has fully run.
    output
}

/// Run `nbox <args>` with the default page size (100).
fn run_nbox(args: &[&str]) -> Output {
    run_nbox_with_page_size(100, args)
}

/// Assert the command succeeded (exit 0); on failure, dump both streams.
fn assert_ok(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "`nbox {what}` exited {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Stdout as a String.
fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Parse stdout as JSON.
fn json_of(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not valid JSON: {e}\n--- stdout ---\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

// --- status ----------------------------------------------------------------

/// `nbox status` reports a 4.2.x version and exits 0. Proves connectivity, auth,
/// and the `/api/status/` serializer shape against the real API.
#[test]
#[ignore = "requires a live NetBox (netbox-integration workflow)"]
fn status_reports_netbox_4_2() {
    let out = run_nbox(&["-o", "json", "status"]);
    assert_ok(&out, "status");
    let v = json_of(&out);
    let version = v["netbox_version"]
        .as_str()
        .expect("status JSON has netbox_version");
    assert!(
        version.starts_with("4.2"),
        "expected NetBox 4.2.x, got {version:?}"
    );
}

// --- search ----------------------------------------------------------------

/// Plain `nbox search` finds a seeded object (the device by name).
#[test]
#[ignore = "requires a live NetBox (netbox-integration workflow)"]
fn search_finds_seeded_device() {
    let out = run_nbox(&["-o", "json", "search", "ci-dev1"]);
    assert_ok(&out, "search ci-dev1");
    let results = json_of(&out);
    let arr = results.as_array().expect("search JSON is an array");
    assert!(
        arr.iter()
            .any(|r| r["kind"] == "device" && r["display"] == "ci-dev1"),
        "search did not surface device ci-dev1: {results}"
    );
}

/// `nbox search --site ci-site` returns the site-scoped prefix. This proves the
/// polymorphic `scope_type=dcim.site` + `scope_id` filter against the REAL API —
/// the exact thing wiremock can't validate, since 4.2 dropped the prefix `site`
/// FK for the polymorphic scope.
#[test]
#[ignore = "requires a live NetBox (netbox-integration workflow)"]
fn search_site_returns_scope_filtered_prefix() {
    let out = run_nbox(&["-o", "json", "search", "10.10", "--site", "ci-site"]);
    assert_ok(&out, "search 10.10 --site ci-site");
    let results = json_of(&out);
    let arr = results.as_array().expect("search JSON is an array");
    assert!(
        arr.iter()
            .any(|r| r["kind"] == "prefix" && r["display"] == "10.10.0.0/16"),
        "site-scoped prefix 10.10.0.0/16 not returned for --site ci-site: {results}"
    );
}

// --- prefix ----------------------------------------------------------------

/// `nbox prefix <scoped cidr>` resolves the site-scoped prefix and surfaces its
/// scope. Exercises the detail-model + scope serializer shapes.
#[test]
#[ignore = "requires a live NetBox (netbox-integration workflow)"]
fn prefix_lookup_resolves_scoped_prefix() {
    let out = run_nbox(&["-o", "json", "prefix", "10.10.0.0/16"]);
    assert_ok(&out, "prefix 10.10.0.0/16");
    let v = json_of(&out);
    assert_eq!(v["prefix"], "10.10.0.0/16", "got: {v}");
    assert_eq!(v["scope"], "ci-site", "scope should be the site: {v}");
    assert_eq!(v["scope_type"], "site", "scope_type should be site: {v}");
}

/// The duplicate prefix exists in two VRFs; addressing it by CIDR is ambiguous
/// (exit 5), and `--vrf ci-vrf` disambiguates to exit 0. Proves the real API
/// returns both, and the VRF filter narrows it.
#[test]
#[ignore = "requires a live NetBox (netbox-integration workflow)"]
fn prefix_duplicate_is_ambiguous_without_vrf() {
    let ambiguous = run_nbox(&["-o", "json", "prefix", "10.0.0.0/24"]);
    assert_eq!(
        ambiguous.status.code(),
        Some(5),
        "expected ambiguous (exit 5) for the duplicated prefix\n--- stderr ---\n{}",
        String::from_utf8_lossy(&ambiguous.stderr)
    );

    let scoped = run_nbox(&["-o", "json", "prefix", "10.0.0.0/24", "--vrf", "ci-vrf"]);
    assert_ok(&scoped, "prefix 10.0.0.0/24 --vrf ci-vrf");
    let v = json_of(&scoped);
    assert_eq!(v["prefix"], "10.0.0.0/24", "got: {v}");
    assert_eq!(v["vrf"], "ci-vrf", "vrf should be ci-vrf: {v}");
}

// --- ip --------------------------------------------------------------------

/// `nbox ip <addr>` resolves the seeded IP and derives parent-prefix context.
#[test]
#[ignore = "requires a live NetBox (netbox-integration workflow)"]
fn ip_lookup_resolves_seeded_address() {
    let out = run_nbox(&["-o", "json", "ip", "10.10.0.5"]);
    assert_ok(&out, "ip 10.10.0.5");
    let v = json_of(&out);
    assert_eq!(v["address"], "10.10.0.5/24", "got: {v}");
    // The IP is assigned to ci-dev1's interface and falls under 10.10.0.0/16.
    assert_eq!(
        v["parent_prefix"], "10.10.0.0/16",
        "parent prefix should be the scoped /16: {v}"
    );
    let assigned = v["assigned"].as_str().unwrap_or_default();
    assert!(
        assigned.contains("ci-dev1"),
        "ip should be assigned to ci-dev1: {v}"
    );
}

// --- device ----------------------------------------------------------------

/// `nbox device ci-dev1` returns the device detail with its interface and the
/// primary IP. Exercises the composed detail model against the real API.
#[test]
#[ignore = "requires a live NetBox (netbox-integration workflow)"]
fn device_detail_includes_interface_and_primary_ip() {
    let out = run_nbox(&["-o", "json", "device", "ci-dev1"]);
    assert_ok(&out, "device ci-dev1");
    let v = json_of(&out);
    assert_eq!(v["name"], "ci-dev1", "got: {v}");
    assert_eq!(v["site"], "ci-site", "device should be at ci-site: {v}");
    assert_eq!(
        v["primary_ip4"], "10.10.0.5/24",
        "device primary_ip4 should be the seeded IP: {v}"
    );
    let ifaces = v["interfaces"].as_array().expect("interfaces array");
    assert!(
        ifaces.iter().any(|i| i["name"] == "xe-0/0/1"),
        "device should carry interface xe-0/0/1: {v}"
    );
}

// --- interface -------------------------------------------------------------

/// `nbox interface ci-dev1 xe-0/0/1` resolves the slash-named interface and its
/// assigned address. Proves interface names with slashes round-trip end to end.
#[test]
#[ignore = "requires a live NetBox (netbox-integration workflow)"]
fn interface_lookup_resolves_slash_named_interface() {
    let out = run_nbox(&["-o", "json", "interface", "ci-dev1", "xe-0/0/1"]);
    assert_ok(&out, "interface ci-dev1 xe-0/0/1");
    let v = json_of(&out);
    assert_eq!(v["name"], "xe-0/0/1", "got: {v}");
    assert_eq!(v["device"], "ci-dev1", "got: {v}");
    let ips = v["ip_addresses"].as_array().expect("ip_addresses array");
    assert!(
        ips.iter().any(|a| a == "10.10.0.5/24"),
        "interface should carry the assigned IP: {v}"
    );
}

// --- next-ip / next-prefix -------------------------------------------------

/// `nbox next-ip <prefix>` returns an available address inside the scoped /16.
/// Exercises the `…/available-ips/` shape against the real API.
#[test]
#[ignore = "requires a live NetBox (netbox-integration workflow)"]
fn next_ip_returns_available_address() {
    let out = run_nbox(&["-o", "json", "next-ip", "10.10.0.0/16"]);
    assert_ok(&out, "next-ip 10.10.0.0/16");
    let v = json_of(&out);
    assert_eq!(v["prefix"], "10.10.0.0/16", "got: {v}");
    let available = v["available"].as_array().expect("available array");
    assert!(
        !available.is_empty(),
        "next-ip should return at least one address: {v}"
    );
    let first = available[0].as_str().unwrap_or_default();
    assert!(
        first.starts_with("10.10."),
        "available address should be within the /16: {v}"
    );
}

/// `nbox next-prefix <prefix> --length 24` returns the first free /24 inside the
/// scoped /16. Exercises the `…/available-prefixes/` full-page request + the
/// local subnetting that picks a block of the requested length.
#[test]
#[ignore = "requires a live NetBox (netbox-integration workflow)"]
fn next_prefix_returns_available_block() {
    let out = run_nbox(&[
        "-o",
        "json",
        "next-prefix",
        "10.10.0.0/16",
        "--length",
        "24",
    ]);
    assert_ok(&out, "next-prefix 10.10.0.0/16 --length 24");
    let v = json_of(&out);
    assert_eq!(v["prefix"], "10.10.0.0/16", "got: {v}");
    let available = v["available"].as_array().expect("available array");
    assert!(
        !available.is_empty(),
        "next-prefix should return at least one /24: {v}"
    );
    let first = available[0].as_str().unwrap_or_default();
    assert!(
        first.ends_with("/24") && first.starts_with("10.10."),
        "available block should be a /24 within the /16: {v}"
    );
}

// --- tags ------------------------------------------------------------------

/// `nbox tags` lists the seeded tag.
#[test]
#[ignore = "requires a live NetBox (netbox-integration workflow)"]
fn tags_lists_seeded_tag() {
    let out = run_nbox(&["-o", "json", "tags"]);
    assert_ok(&out, "tags");
    let v = json_of(&out);
    let tags = v["tags"].as_array().expect("tags array");
    assert!(
        tags.iter().any(|t| t["slug"] == "ci-tag"),
        "tags should include ci-tag: {v}"
    );
}

// --- pagination ------------------------------------------------------------

/// One pagination case that spans more than one page. The prefix detail lists
/// child prefixes via `list_all` (offset-windowed pagination); the seed nests 25
/// child /24s under 10.10.0.0/16. Driven with a low `page_size` (5), gathering
/// them must walk several offset windows. The full expected set returning proves
/// the offset-windows fix against the REAL paginator: no skips, no duplicates,
/// no early truncation. (Walking by rows-returned instead of requested-window
/// would skip mounted offsets and lose rows; this asserts every child is back.)
#[test]
#[ignore = "requires a live NetBox (netbox-integration workflow)"]
fn pagination_spans_multiple_pages() {
    // Page size 5 over 25 children → five pages at offsets 0,5,10,15,20.
    let out = run_nbox_with_page_size(5, &["-o", "json", "prefix", "10.10.0.0/16"]);
    assert_ok(&out, "prefix 10.10.0.0/16 (page_size 5)");
    let v = json_of(&out);
    let children: Vec<&str> = v["child_prefixes"]
        .as_array()
        .expect("child_prefixes array")
        .iter()
        .filter_map(Value::as_str)
        .filter(|d| d.ends_with("/24") && d.starts_with("10.10."))
        .collect();

    // All 25 nested /24s must come back — more than five pages' worth, proving we
    // followed `next` across every offset window rather than stopping at page 1.
    assert!(
        children.len() >= 25,
        "expected >=25 child /24s across pages, got {}: {children:?}",
        children.len()
    );
    // Spot-check the first and last windows are both represented.
    assert!(
        children.contains(&"10.10.1.0/24"),
        "first child prefix (offset 0 window) missing: {children:?}"
    );
    assert!(
        children.contains(&"10.10.25.0/24"),
        "last child prefix (final offset window) missing — paginated early?: {children:?}"
    );
    // No duplicates across pages.
    let mut sorted = children.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        children.len(),
        "duplicate child rows across pages: {children:?}"
    );

    // The human path works too: plain output lists the child prefixes.
    let plain = run_nbox_with_page_size(5, &["prefix", "10.10.0.0/16"]);
    assert_ok(&plain, "prefix 10.10.0.0/16 (plain)");
    let text = stdout_of(&plain);
    assert!(
        text.contains("Child Prefixes") && text.contains("10.10.25.0/24"),
        "plain prefix output should list child prefixes including the last one"
    );
}
