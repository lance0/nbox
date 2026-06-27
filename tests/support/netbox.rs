use nbox::config::ProfileConfig;
use nbox::netbox::client::NetBoxClient;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub fn client(server: &MockServer) -> NetBoxClient {
    let profile = ProfileConfig {
        url: server.uri(),
        ..Default::default()
    };
    NetBoxClient::new(&profile, None).expect("create NetBox client")
}

pub fn not_found(noun: &str, value: &str) -> anyhow::Error {
    anyhow::anyhow!("no {noun} matched \"{value}\"")
}

pub fn page(results: Vec<Value>) -> Value {
    json!({
        "count": results.len(),
        "next": null,
        "previous": null,
        "results": results
    })
}

pub fn empty_page() -> Value {
    page(Vec::new())
}

pub async fn mount_empty_list(server: &MockServer, endpoint: &str) {
    Mock::given(method("GET"))
        .and(path(endpoint))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_page()))
        .mount(server)
        .await;
}

// --- NetBox wire-JSON builders ---------------------------------------------
//
// These return raw API response shapes (nested `status: {value, label}`,
// `scope: {id, display}`, …) for use as wiremock response bodies. They are a
// DIFFERENT layer from the view-struct builders in `fixtures.rs` (which produce
// the flat output shapes). Each builder yields the minimal identity shape; use
// the [`Wire`] wrapper's chainable helpers to add the optional relation fields
// a given test asserts on (`site`, `scope`, `group`, `provider`, `vrf`, …).
//
// `url` uses a generic `http://nb/...` placeholder. Tests that assert on the
// rewritten web URL still pass — `api_to_web_url` only strips the `/api/`
// segment — and tests that need the mock server's real URI override it with
// `.url(...)`.

/// A NetBox device wire object (raw API response shape).
pub fn nb_device(id: u64, name: &str) -> Wire {
    Wire::from(json!({
        "id": id,
        "url": format!("http://nb/api/dcim/devices/{}/", id),
        "name": name,
        "display": name,
        "status": {"value": "active", "label": "Active"},
        "custom_fields": {},
    }))
}

/// A NetBox site wire object.
pub fn nb_site(id: u64, name: &str, slug: &str) -> Wire {
    Wire::from(json!({
        "id": id,
        "url": format!("http://nb/api/dcim/sites/{}/", id),
        "name": name,
        "display": name,
        "slug": slug,
        "custom_fields": {},
    }))
}

/// A NetBox IP address wire object.
pub fn nb_ip(id: u64, address: &str) -> Wire {
    Wire::from(json!({
        "id": id,
        "url": format!("http://nb/api/ipam/ip-addresses/{}/", id),
        "address": address,
        "status": {"value": "active", "label": "Active"},
    }))
}

/// A NetBox prefix wire object.
pub fn nb_prefix(id: u64, cidr: &str) -> Wire {
    Wire::from(json!({
        "id": id,
        "url": format!("http://nb/api/ipam/prefixes/{}/", id),
        "prefix": cidr,
        "status": {"value": "active", "label": "Active"},
    }))
}

/// A NetBox VLAN wire object.
pub fn nb_vlan(id: u64, vid: u16, name: &str) -> Wire {
    Wire::from(json!({
        "id": id,
        "url": format!("http://nb/api/ipam/vlans/{}/", id),
        "vid": vid,
        "name": name,
        "display": format!("{} ({})", vid, name),
    }))
}

/// A NetBox circuit wire object.
pub fn nb_circuit(id: u64, cid: &str) -> Wire {
    Wire::from(json!({
        "id": id,
        "url": format!("http://nb/api/circuits/circuits/{}/", id),
        "cid": cid,
        "status": {"value": "active", "label": "Active"},
    }))
}

/// A NetBox VRF wire object.
pub fn nb_vrf(id: u64, name: &str, rd: &str) -> Wire {
    Wire::from(json!({
        "id": id,
        "url": format!("http://nb/api/ipam/vrfs/{}/", id),
        "name": name,
        "rd": rd,
    }))
}

/// A NetBox tenant wire object.
pub fn nb_tenant(id: u64, name: &str, slug: &str) -> Wire {
    Wire::from(json!({
        "id": id,
        "url": format!("http://nb/api/tenancy/tenants/{}/", id),
        "name": name,
        "display": name,
        "slug": slug,
    }))
}

/// A NetBox rack wire object.
pub fn nb_rack(id: u64, name: &str) -> Wire {
    Wire::from(json!({
        "id": id,
        "url": format!("http://nb/api/dcim/racks/{}/", id),
        "name": name,
    }))
}

/// A chainable wrapper around a NetBox wire [`Value`] that adds optional
/// relation fields the base builders omit. Call `.build()` (or pass the `Wire`
/// directly where a `Value` is expected — it converts via [`Into::<Value>`]) to
/// finalize. Implements [`Serialize`] by delegating to the inner [`Value`], so
/// a `Wire` can be passed straight to `ResponseTemplate::set_body_json` or
/// [`page`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Wire {
    value: Value,
}

impl Wire {
    /// Wrap a pre-built wire object (the base builders use this internally).
    pub fn from(value: Value) -> Self {
        Wire { value }
    }

    /// Override the `url` field (e.g. with the mock server's real URI).
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.set("url", json!(url.into()));
        self
    }

    /// Add/override the `display` field.
    pub fn display(mut self, display: impl Into<String>) -> Self {
        self.set("display", json!(display.into()));
        self
    }

    /// Set the `status` choice (`{value, label}`).
    pub fn status(mut self, value: &str, label: &str) -> Self {
        self.set("status", json!({"value": value, "label": label}));
        self
    }

    /// Add a `site` brief (`{id, display}`).
    pub fn site(mut self, id: u64, display: &str) -> Self {
        self.set("site", json!({"id": id, "display": display}));
        self
    }

    /// Add a `group` brief (`{id, display}`).
    pub fn group(mut self, id: u64, display: &str) -> Self {
        self.set("group", json!({"id": id, "display": display}));
        self
    }

    /// Add a `provider` brief (`{id, display}`).
    pub fn provider(mut self, id: u64, display: &str) -> Self {
        self.set("provider", json!({"id": id, "display": display}));
        self
    }

    /// Add a `tenant` brief (`{id, display}`).
    pub fn tenant(mut self, id: u64, display: &str) -> Self {
        self.set("tenant", json!({"id": id, "display": display}));
        self
    }

    /// Add a `vrf` brief (`{id, display}`).
    pub fn vrf(mut self, id: u64, display: &str) -> Self {
        self.set("vrf", json!({"id": id, "display": display}));
        self
    }

    /// Add a polymorphic scope: `scope_type` + `scope: {id, display}`.
    pub fn scope(mut self, content_type: &str, id: u64, display: &str) -> Self {
        self.set("scope_type", json!(content_type));
        self.set("scope", json!({"id": id, "display": display}));
        self
    }

    /// Add a `type` brief (`{id, display}`) — for clusters/providers.
    pub fn type_(mut self, id: u64, display: &str) -> Self {
        self.set("type", json!({"id": id, "display": display}));
        self
    }

    /// Add a `cluster` brief (`{id, display}`).
    pub fn cluster(mut self, id: u64, display: &str) -> Self {
        self.set("cluster", json!({"id": id, "display": display}));
        self
    }

    /// Set an arbitrary field by key.
    pub fn set(&mut self, key: &str, value: Value) {
        if let Some(obj) = self.value.as_object_mut() {
            obj.insert(key.to_string(), value);
        }
    }

    /// Finalize into the underlying [`Value`].
    pub fn build(self) -> Value {
        self.value
    }
}

impl From<Wire> for Value {
    fn from(w: Wire) -> Value {
        w.value
    }
}

impl std::ops::Deref for Wire {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.value
    }
}
