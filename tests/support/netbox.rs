use nbox::config::ProfileConfig;
use nbox::netbox::client::NetBoxClient;
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
