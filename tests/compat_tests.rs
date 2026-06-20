//! NetBox 4.2 / 4.3 / 4.5 compatibility matrix, pinned as tests.
//!
//! Each version moved an API contract nbox depends on; this file locks the
//! behavior nbox emits across that range so a regression in one version's path
//! can't pass silently. The matrix it mirrors lives in `docs/COMPATIBILITY.md`.
//!
//!   - **4.2** introduced the polymorphic `scope` (`scope_type` + `scope_id`),
//!     dropping the prefix `site` FK → scope filtering sends `scope_type=dcim.<kind>`.
//!   - **4.3** moved GraphQL filtering to per-field lookups and dropped the
//!     full-text `q` filter → `nbox search` is always REST.
//!   - **4.5** dropped the prefix `utilization` field → nbox computes container
//!     utilization client-side from the fetched tree; v2 tokens (`nbt_…`) are sent
//!     as `Bearer`, v1 as `Token`.
//!
//! Patterns mirror `tests/client_tests.rs` and `tests/search_tests.rs`.

use nbox::config::{ApiConfig, ApiSurface, BackendPreference, ProfileConfig};
use nbox::netbox::capabilities::EffectiveBackend;
use nbox::netbox::client::NetBoxClient;
use nbox::netbox::prefix_tree::build_nodes;
use nbox::netbox::status::{MIN_MAJOR, MIN_MINOR, Status};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> NetBoxClient {
    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    NetBoxClient::new(&profile, None).unwrap()
}

fn status_for(version: &str) -> Status {
    Status {
        netbox_version: version.into(),
        django_version: None,
        python_version: None,
    }
}

// ---------------------------------------------------------------------------
// 1. Minimum-version gating (floor = 4.2).
// ---------------------------------------------------------------------------

/// The capability report's `compatible` flag tracks the 4.2 floor exactly, and
/// `minimum_supported` reports the build's `MIN_MAJOR.MIN_MINOR` regardless of the
/// connected version. 4.1 is below the floor; 4.2 and 4.5 meet it.
#[tokio::test]
async fn capabilities_compatible_flag_tracks_the_4_2_floor() {
    let server = MockServer::start().await;
    let nbox = client(&server);
    let expected_min = format!("{MIN_MAJOR}.{MIN_MINOR}");

    for (version, want_compatible) in [
        ("4.1.0", false),
        ("4.2.0", true),
        ("4.3.0", true),
        ("4.5.5", true),
    ] {
        let caps = nbox.capabilities(&status_for(version)).await;
        assert_eq!(
            caps.version.compatible, want_compatible,
            "version {version} compatibility"
        );
        assert_eq!(caps.version.netbox, version);
        assert_eq!(
            caps.version.minimum_supported, expected_min,
            "minimum is the build floor, not the connected version"
        );
    }
}

/// `verify_compatible` (the TUI's launch probe) rejects a sub-4.2 `/api/status/`
/// and accepts 4.2+, surfacing the reported version on success.
#[tokio::test]
async fn verify_compatible_gates_on_the_status_version() {
    // Below the floor → error naming the version and the floor.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/status/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "netbox-version": "4.1.9"
        })))
        .mount(&server)
        .await;
    let err = client(&server)
        .verify_compatible()
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("4.1.9"), "error names the version: {err}");
    assert!(err.contains("4.2"), "error names the floor: {err}");

    // At the floor → accepted, version echoed back.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/status/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "netbox-version": "4.2.0"
        })))
        .mount(&server)
        .await;
    let status = client(&server).verify_compatible().await.unwrap();
    assert_eq!(status.netbox_version, "4.2.0");
}

// ---------------------------------------------------------------------------
// 2. Client-side container utilization (NetBox 4.5 dropped the API field).
// ---------------------------------------------------------------------------

/// A prefix tree node built from a payload that OMITS `utilization` (NetBox 4.5+):
/// a container fills its utilization from direct-child coverage, while a leaf
/// without children stays `None`. Exercises the pure `build_nodes` /
/// `fill_child_coverage` path — no wiremock needed.
#[test]
fn container_utilization_is_computed_client_side_when_api_omits_it() {
    // /16 carved into a /17 + a /18 → 0.5 + 0.25 = 75% covered. None of the rows
    // carry `utilization` (the 4.5 API shape).
    let prefix =
        |id: u64, cidr: &str, depth: u64, children: u64| -> nbox::netbox::models::ipam::Prefix {
            serde_json::from_value(json!({
                "id": id,
                "url": format!("http://nb/api/ipam/prefixes/{id}/"),
                "prefix": cidr,
                "status": {"value": "active", "label": "Active"},
                "children": children,
                "_depth": depth,
            }))
            .unwrap()
        };

    let nodes = build_nodes(vec![
        prefix(1, "10.0.0.0/16", 0, 2),
        prefix(2, "10.0.0.0/17", 1, 0),
        prefix(3, "10.0.128.0/18", 1, 0),
    ]);

    // The container's utilization was synthesized from its children.
    assert_eq!(
        nodes[0].utilization,
        Some(75),
        "container utilization computed from child coverage"
    );
    // Leaves (no children) stay None — IP-level utilization would need per-prefix
    // queries the API no longer answers cheaply.
    assert_eq!(nodes[1].utilization, None);
    assert_eq!(nodes[2].utilization, None);
}

/// An older NetBox (≤ 4.4) that still serves `utilization` keeps its
/// API-provided value rather than overwriting it with the client-side estimate.
#[test]
fn api_provided_utilization_is_preserved_on_older_netbox() {
    let container: nbox::netbox::models::ipam::Prefix = serde_json::from_value(json!({
        "id": 1, "url": "http://nb/api/ipam/prefixes/1/", "prefix": "10.0.0.0/16",
        "status": {"value": "active", "label": "Active"},
        "children": 1, "_depth": 0,
        "utilization": 61,
    }))
    .unwrap();
    let child: nbox::netbox::models::ipam::Prefix = serde_json::from_value(json!({
        "id": 2, "url": "http://nb/api/ipam/prefixes/2/", "prefix": "10.0.0.0/24",
        "status": {"value": "active", "label": "Active"},
        "children": 0, "_depth": 1,
    }))
    .unwrap();

    let nodes = build_nodes(vec![container, child]);
    assert_eq!(
        nodes[0].utilization,
        Some(61),
        "an API-provided value wins over the computed one"
    );
}

// ---------------------------------------------------------------------------
// 3. Search is REST regardless of version (4.3 dropped the GraphQL `q` filter).
// ---------------------------------------------------------------------------

/// Even with a `graphql` preference for the search surface, the search backend
/// resolves to a REST fallback — without probing the schema — carrying the
/// product-rule reason. This holds on every NetBox version: it's not a schema
/// gap, it's that GraphQL has no REST-equivalent full-text `q`.
#[tokio::test]
async fn search_surface_is_rest_even_when_graphql_is_preferred() {
    let profile = ProfileConfig {
        // No /graphql/ endpoint is mounted: the search fallback must not probe.
        url: "http://netbox.invalid".into(),
        api: Some(ApiConfig {
            search: Some(BackendPreference::Graphql),
            vrf: Some(BackendPreference::Rest),
        }),
        ..Default::default()
    };
    let nbox = NetBoxClient::new(&profile, None).unwrap();

    let effective = nbox.effective_backend(ApiSurface::Search).await;
    assert!(!effective.uses_graphql(), "search never uses GraphQL");
    assert_eq!(effective.label(), "rest");
    let reason = match &effective {
        EffectiveBackend::RestFallback { reason } => reason.clone(),
        other => panic!("search must be a REST fallback, got {other:?}"),
    };
    assert!(
        reason.to_lowercase().contains("full-text") || reason.contains('q'),
        "fallback reason names the missing full-text q search: {reason}"
    );

    // The routing the `status` surface exposes agrees: configured graphql,
    // effective rest, with the reason carried.
    let routing = nbox.api_routing().await;
    assert_eq!(routing.search.configured, BackendPreference::Graphql);
    assert_eq!(routing.search.effective, "rest");
    assert_eq!(routing.search.reason.as_deref(), Some(reason.as_str()));
}

/// The capability report marks the GraphQL `search` surface unsupported with the
/// reason string, independent of the connected version. A pure-REST profile keeps
/// `status` cheap (no probe), so the per-surface GraphQL shape is summarized via
/// the routing block, and the search REST capability stays true on 4.2 and 4.5.
#[tokio::test]
async fn search_capability_is_rest_only_across_versions() {
    let server = MockServer::start().await;
    let nbox = client(&server);

    for version in ["4.2.0", "4.3.0", "4.5.5"] {
        let caps = nbox.capabilities(&status_for(version)).await;
        assert!(caps.rest.search, "REST always backs search on {version}");
        // A REST profile doesn't probe GraphQL, so it reports no surfaces — search
        // can only ever route to REST here.
        assert!(!caps.graphql.probed, "REST profile skips the GraphQL probe");
        assert!(caps.graphql.surfaces.is_none());
    }
}

// ---------------------------------------------------------------------------
// 4. Scope filtering shape (4.2 polymorphic scope).
// ---------------------------------------------------------------------------

/// A `--site` search resolves the site to its id once, then filters the prefix
/// endpoint by `scope_type=dcim.site` + `scope_id=<id>` (the 4.2 polymorphic
/// scope), not the dead `?site=` slug filter. The prefix mock matches ONLY when
/// both scope params are present, so a regression to the old FK would 404/miss.
#[tokio::test]
async fn site_scope_filters_prefixes_by_scope_type_and_id() {
    let server = MockServer::start().await;

    // Site resolution: the slug lookup returns id 9.
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

    // The prefix endpoint must carry the translated polymorphic scope params.
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

    // Remaining endpoints the search fan-out touches: empty pages so the run is
    // unambiguous and the assertion isolates the prefix scope behavior.
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(query_param("q", "10.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(&server)
        .await;
    for endpoint in [
        "/api/dcim/devices/",
        "/api/ipam/vlans/",
        "/api/virtualization/virtual-machines/",
        "/api/virtualization/clusters/",
        "/api/dcim/racks/",
        "/api/ipam/vrfs/",
        "/api/ipam/route-targets/",
    ] {
        Mock::given(method("GET"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
            .mount(&server)
            .await;
    }

    let results = client(&server)
        .search(nbox::netbox::search::SearchRequest {
            query: "10.1".into(),
            limit: 25,
            filters: nbox::netbox::search::SearchFilters {
                site: Some("iad1".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
        .results;

    let prefix = results
        .iter()
        .find(|r| r.kind == nbox::netbox::search::ObjectKind::Prefix)
        .expect("site-scoped prefix surfaced via scope_type/scope_id");
    assert_eq!(prefix.display, "10.1.0.0/24");
    assert_eq!(prefix.subtitle.as_deref(), Some("iad1"));
}

// ---------------------------------------------------------------------------
// Token scheme: v2 (`nbt_…`) → Bearer, v1 → Token (auto-detected). NetBox 4.5
// added v2 tokens; older versions only issue v1.
// ---------------------------------------------------------------------------

/// Auto scheme sends a v2 token (`nbt_<key>.<secret>`) as `Authorization: Bearer`.
#[tokio::test]
async fn v2_token_is_sent_as_bearer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(header("authorization", "Bearer nbt_key.secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .expect(1)
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    let nbox = NetBoxClient::new(&profile, Some("nbt_key.secret".into())).unwrap();
    let _page: nbox::netbox::pagination::Page<Value> = nbox
        .list(nbox::netbox::endpoints::Endpoint::Sites, vec![])
        .await
        .unwrap();
}

/// Auto scheme sends a legacy v1 token as `Authorization: Token`.
#[tokio::test]
async fn v1_token_is_sent_as_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/dcim/sites/"))
        .and(header("authorization", "Token 0123456789abcdef"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .expect(1)
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    let nbox = NetBoxClient::new(&profile, Some("0123456789abcdef".into())).unwrap();
    let _page: nbox::netbox::pagination::Page<Value> = nbox
        .list(nbox::netbox::endpoints::Endpoint::Sites, vec![])
        .await
        .unwrap();
}

fn empty_page() -> Value {
    json!({ "count": 0, "next": null, "previous": null, "results": [] })
}
