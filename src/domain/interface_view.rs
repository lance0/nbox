//! Flattened interface view for `nbox interface` (plain + JSON).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::domain::custom;
use crate::domain::util::non_empty;
use crate::netbox::models::dcim::Interface;
use crate::netbox::models::ipam::IpAddress;
use crate::output::plain::KeyValues;

/// An interface, normalized to flat fields plus its assigned addresses.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InterfaceView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub untagged_vlan: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tagged_vlans: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cable: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub connected_to: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ip_addresses: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<String>,
    /// The cable path as a multi-line ASCII diagram (a vertical A↔Z chain with any
    /// intermediate panels), derived from the same trace hops as `trace`. Not
    /// serialized — it's a rendering of `trace` for the plain/TUI surfaces, while
    /// the structured flat `trace` lines remain the machine-readable form.
    #[serde(skip)]
    pub diagram: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, Value>,
}

impl InterfaceView {
    /// Build a view from a wire [`Interface`], the IPs assigned to it, and the
    /// raw cable-trace hops (`…/trace/`), rendered into a readable path.
    pub fn build(i: Interface, ips: Vec<IpAddress>, trace: Vec<Value>) -> Self {
        Self {
            device: i.device.map(|b| b.label()),
            name: i.name,
            enabled: i.enabled,
            type_: i.type_.map(|c| c.label),
            mtu: i.mtu,
            mac_address: i.mac_address.and_then(non_empty),
            mode: i.mode.map(|c| c.label),
            untagged_vlan: i.untagged_vlan.map(|b| b.label()),
            tagged_vlans: i.tagged_vlans.into_iter().map(|b| b.label()).collect(),
            cable: i.cable.map(|b| b.label()),
            connected_to: i
                .connected_endpoints
                .unwrap_or_default()
                .into_iter()
                .map(|b| b.endpoint_label())
                .collect(),
            description: i.description.and_then(non_empty),
            ip_addresses: ips.into_iter().map(|ip| ip.address).collect(),
            trace: format_trace(&trace),
            diagram: format_trace_diagram(&trace),
            owner: i.owner.map(|bo| bo.label()),
            tags: i.tags.into_iter().map(|tag| tag.slug).collect(),
            custom_fields: custom::fields(&i.custom_fields),
        }
    }

    /// The interface's attribute key-values (no sections).
    fn attributes(&self) -> String {
        let mut kv = KeyValues::new();
        kv.push_opt("device", self.device.clone())
            .push("name", self.name.clone())
            .push_opt("enabled", self.enabled.map(|b| b.to_string()))
            .push_opt("type", self.type_.clone())
            .push_opt("mtu", self.mtu.map(|m| m.to_string()))
            .push_opt("mac", self.mac_address.clone())
            .push_opt("mode", self.mode.clone())
            .push_opt("untagged_vlan", self.untagged_vlan.clone())
            .push_opt("cable", self.cable.clone())
            .push_opt("description", self.description.clone());
        if !self.tags.is_empty() {
            kv.push("tags", self.tags.join(", "));
        }
        kv.push_opt("owner", self.owner.clone());
        custom::append(&mut kv, &self.custom_fields);
        kv.render()
    }

    /// Render header fields plus tagged-VLAN / connection / IP sections, with the
    /// cable path drawn as the A↔Z diagram. The CLI's full `nbox interface` output.
    pub fn to_plain(&self) -> String {
        let mut out = self.attributes();
        out.push_str(&section("Tagged VLANs", &self.tagged_vlans));
        out.push_str(&section("Connected To", &self.connected_to));
        out.push_str(&section("Cable Path", &self.diagram));
        out.push_str(&section("IP Addresses", &self.ip_addresses));
        out
    }

    /// The TUI detail body: like [`Self::to_plain`] but without the cable path —
    /// the TUI surfaces that in a dedicated, scrollable Path tab instead.
    pub fn to_summary_plain(&self) -> String {
        let mut out = self.attributes();
        out.push_str(&section("Tagged VLANs", &self.tagged_vlans));
        out.push_str(&section("Connected To", &self.connected_to));
        out.push_str(&section("IP Addresses", &self.ip_addresses));
        out
    }
}

/// A `\n\nTitle\n  item` block, or empty when there are no items.
fn section(title: &str, items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = items.iter().map(|i| format!("  {i}")).collect();
    format!("\n\n{title}\n{}", lines.join("\n"))
}

/// Render trace hops (`[near terminations, cable, far terminations]`) into
/// readable `near --[cable]-- far` lines, tolerating the polymorphic JSON.
fn format_trace(hops: &[Value]) -> Vec<String> {
    hops.iter()
        .filter_map(|hop| {
            let arr = hop.as_array()?;
            let near = termination_labels(arr.first());
            let far = termination_labels(arr.get(2));
            let mid = match arr.get(1).and_then(cable_label) {
                Some(c) => format!(" --[{c}]-- "),
                None => " -- ".to_string(),
            };
            let line = format!("{near}{mid}{far}");
            (line.trim() != "--").then_some(line)
        })
        .collect()
}

/// Join the `display` labels of a terminations array (or a single object).
fn termination_labels(v: Option<&Value>) -> String {
    match v {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(label_of)
            .collect::<Vec<_>>()
            .join(", "),
        Some(other) => label_of(other).unwrap_or_default(),
        None => String::new(),
    }
}

/// A `device name` (or just `name`) label from a termination object.
fn label_of(v: &Value) -> Option<String> {
    let name = v
        .get("display")
        .or_else(|| v.get("name"))
        .and_then(|x| x.as_str())?;
    let device = v.get("device").and_then(|d| {
        d.get("display")
            .or_else(|| d.get("name"))
            .and_then(|x| x.as_str())
    });
    Some(match device {
        Some(dev) => format!("{dev} {name}"),
        None => name.to_string(),
    })
}

/// The display/label of a cable object, if present.
fn cable_label(v: &Value) -> Option<String> {
    v.get("display")
        .or_else(|| v.get("label"))
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string)
}

/// One termination of a cable path: its owning device (when it's an interface)
/// and the port label. `None` device → a non-interface termination (e.g. a
/// circuit). `None` overall → an empty/unterminated side. Reads the first
/// termination of a side (a breakout/LAG collapses to its first port).
fn term_one(v: Option<&Value>) -> Option<(Option<String>, String)> {
    let first = match v {
        Some(Value::Array(items)) => items.first()?,
        Some(other) => other,
        None => return None,
    };
    let port = first
        .get("display")
        .or_else(|| first.get("name"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    // Prefer the bare device `name` over `display` (which can carry an asset tag);
    // the diagram reads cleaner with stable names.
    let device = first.get("device").and_then(|d| {
        d.get("name")
            .or_else(|| d.get("display"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(std::string::ToString::to_string)
    });
    Some((device, port))
}

/// Draw the cable path as a vertical A↔Z diagram: the near interface on top (`A`),
/// the far interface on the bottom (`Z`), each `┿`-marked cable segment between
/// them carrying its descriptor. A patch panel in the path (its two faces share a
/// device) collapses to one pass-through stop (`front → rear`); an unterminated
/// side is shown explicitly rather than dropped. Tolerates the polymorphic JSON.
///
/// ```text
///  A  edge01
///     swp1
///     │
///     ┿ #4120 · connected
///     │
///  Z  core01
///     1/1/c13/1
/// ```
fn format_trace_diagram(hops: &[Value]) -> Vec<String> {
    struct Stop {
        device: Option<String>,
        ports: Vec<String>,
    }
    let stop_from = |t: Option<(Option<String>, String)>| match t {
        Some((device, port)) => Stop {
            device,
            ports: vec![port],
        },
        None => Stop {
            device: None,
            ports: Vec::new(),
        },
    };

    // Build the ordered chain of stops and the cable between each adjacent pair.
    let mut stops: Vec<Stop> = Vec::new();
    let mut cables: Vec<String> = Vec::new();
    for (idx, hop) in hops.iter().enumerate() {
        let Some(arr) = hop.as_array() else { continue };
        let near = term_one(arr.first());
        let far = term_one(arr.get(2));
        if idx == 0 {
            stops.push(stop_from(near));
        } else if let Some((device, port)) = near {
            // A later hop's near end is the other face of the previous stop's
            // device (a panel pass-through) — merge it in; otherwise it's a
            // distinct mid stop.
            match stops.last_mut() {
                Some(last) if last.device.is_some() && last.device == device => {
                    if last.ports.last().map(String::as_str) != Some(port.as_str()) {
                        last.ports.push(port);
                    }
                }
                _ => stops.push(Stop {
                    device,
                    ports: vec![port],
                }),
            }
        }
        cables.push(cable_descr(arr.get(1)));
        stops.push(stop_from(far));
    }
    if stops.is_empty() {
        return Vec::new();
    }

    // Render: A on top, Z on bottom, mids unlabeled; a cable segment between stops.
    let mut lines = Vec::new();
    let last = stops.len() - 1;
    for (i, stop) in stops.iter().enumerate() {
        let role = if i == 0 {
            "A"
        } else if i == last {
            "Z"
        } else {
            " "
        };
        let device = stop.device.as_deref().unwrap_or("(unterminated)");
        lines.push(format!(" {role}  {device}"));
        if !stop.ports.is_empty() {
            lines.push(format!("    {}", stop.ports.join(" → ")));
        }
        if i < last {
            lines.push("    │".to_string());
            lines.push(format!(
                "    ┿ {}",
                cables.get(i).cloned().unwrap_or_default()
            ));
            lines.push("    │".to_string());
        }
    }
    lines
}

/// A cable's one-line descriptor for the diagram: `label · type · length · status`
/// from whichever fields the trace's cable object carries (all optional). Falls
/// back to `"cable"` when the object is absent or carries nothing legible.
fn cable_descr(v: Option<&Value>) -> String {
    let Some(v) = v else {
        return "cable".to_string();
    };
    let mut parts: Vec<String> = Vec::new();
    // The cable's name: its display/label when set, else NetBox's canonical
    // `#<id>` — the trace serializer omits `display` (it sends `display_url`) and
    // often leaves `label` empty, so without this the cable id is lost.
    if let Some(label) = cable_label(v).or_else(|| {
        v.get("id")
            .and_then(Value::as_u64)
            .map(|id| format!("#{id}"))
    }) {
        parts.push(label);
    }
    if let Some(t) = choice_label(v.get("type")) {
        parts.push(t);
    }
    if let Some(len) = v.get("length").and_then(Value::as_f64) {
        let unit = choice_value(v.get("length_unit")).unwrap_or_default();
        parts.push(format!("{len}{unit}"));
    }
    if let Some(status) = choice_label(v.get("status")) {
        // The trace serializes status as a bare value string (`connected`); a
        // `{value,label}` choice already carries a cased label. Title-case the
        // first letter so the diagram reads `Connected` either way (idempotent).
        parts.push(title_first(&status));
    }
    if parts.is_empty() {
        "cable".to_string()
    } else {
        parts.join(" · ")
    }
}

/// The human label of a NetBox choice (`{value,label}`), preferring `label`;
/// tolerates a plain string. Empty → `None`.
fn choice_label(v: Option<&Value>) -> Option<String> {
    let v = v?;
    v.get("label")
        .or_else(|| v.get("value"))
        .and_then(Value::as_str)
        .or_else(|| v.as_str())
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string)
}

/// Uppercase the first character of `s`, leaving the rest as-is. Idempotent for
/// already-capitalized text. Used to present a bare status value (`connected`) as
/// `Connected` without disturbing an all-caps label like `CAT6`.
fn title_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The short value of a NetBox choice (`{value,label}`), preferring `value`;
/// tolerates a plain string. Used for the length unit (`m`, not `Meters`).
fn choice_value(v: Option<&Value>) -> Option<String> {
    let v = v?;
    v.get("value")
        .and_then(Value::as_str)
        .or_else(|| v.as_str())
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_interface_with_vlans_and_ips() {
        let iface: Interface = serde_json::from_value(json!({
            "id": 42, "url": "u", "name": "xe-0/0/1",
            "device": {"id": 1, "display": "edge01"},
            "enabled": true,
            "type": {"value": "10gbase-x-sfpp", "label": "SFP+ (10GE)"},
            "mtu": 9000,
            "mode": {"value": "tagged", "label": "Tagged"},
            "untagged_vlan": {"id": 5, "display": "10 (mgmt)"},
            "tagged_vlans": [{"id": 6, "display": "20 (prod)"}, {"id": 7, "display": "30 (dev)"}],
            "cable": {"id": 3, "display": "#3"},
            "connected_endpoints": [{"id": 99, "display": "core01 xe-1/0/0"}],
            "tags": [{"id": 1, "name": "uplink", "slug": "uplink"}],
            "custom_fields": {}
        }))
        .unwrap();
        let ips: Vec<IpAddress> = vec![
            serde_json::from_value(json!({"id": 8, "url": "u", "address": "10.0.0.1/31"})).unwrap(),
        ];

        let view = InterfaceView::build(iface, ips, vec![]);
        assert!(view.trace.is_empty());
        assert_eq!(view.device.as_deref(), Some("edge01"));
        assert_eq!(view.type_.as_deref(), Some("SFP+ (10GE)"));
        assert_eq!(view.mode.as_deref(), Some("Tagged"));
        assert_eq!(view.untagged_vlan.as_deref(), Some("10 (mgmt)"));
        assert_eq!(view.tagged_vlans, vec!["20 (prod)", "30 (dev)"]);
        assert_eq!(view.connected_to, vec!["core01 xe-1/0/0"]);
        assert_eq!(view.tags, vec!["uplink"]);

        let plain = view.to_plain();
        assert!(plain.contains("name: xe-0/0/1"));
        assert!(plain.contains("tags: uplink"));
        assert!(plain.contains("Tagged VLANs\n  20 (prod)\n  30 (dev)"));
        assert!(plain.contains("Connected To\n  core01 xe-1/0/0"));
        assert!(plain.contains("IP Addresses\n  10.0.0.1/31"));
    }

    #[test]
    fn tags_dropped_when_empty() {
        let iface: Interface =
            serde_json::from_value(json!({"id": 1, "url": "u", "name": "xe-0/0/0"})).unwrap();
        let view = InterfaceView::build(iface, vec![], vec![]);
        assert!(view.tags.is_empty());
        let value = serde_json::to_value(&view).unwrap();
        assert!(value.get("tags").is_none());
        assert!(!view.to_plain().contains("tags:"));
    }

    #[test]
    fn renders_cable_trace_path() {
        let iface: Interface =
            serde_json::from_value(json!({"id": 1, "url": "u", "name": "xe-0/0/0"})).unwrap();
        // One hop: near interface --[cable]-- far interface (each side an array).
        let trace = vec![json!([
            [{"display": "xe-0/0/0", "device": {"display": "edge01"}}],
            {"display": "Cable #3"},
            [{"display": "xe-1/0/0", "device": {"display": "core01"}}]
        ])];

        let view = InterfaceView::build(iface, vec![], trace);
        // The flat, machine-readable trace is unchanged (the JSON contract).
        assert_eq!(
            view.trace,
            vec!["edge01 xe-0/0/0 --[Cable #3]-- core01 xe-1/0/0"]
        );
        // The diagram draws the hop as a vertical A↔Z chain: device emphasized,
        // port below, A on top and Z on the bottom.
        assert_eq!(
            view.diagram,
            vec![
                " A  edge01",
                "    xe-0/0/0",
                "    │",
                "    ┿ Cable #3",
                "    │",
                " Z  core01",
                "    xe-1/0/0",
            ]
        );
        // Plain output renders the diagram under the Cable Path heading.
        let plain = view.to_plain();
        assert!(plain.contains("Cable Path"));
        assert!(plain.contains("┿ Cable #3"));
        // The TUI summary body omits the cable path (it lives in the Path tab).
        assert!(!view.to_summary_plain().contains("Cable Path"));
    }

    #[test]
    fn cable_diagram_includes_type_length_and_status() {
        let iface: Interface =
            serde_json::from_value(json!({"id": 1, "url": "u", "name": "xe-0/0/0"})).unwrap();
        let trace = vec![json!([
            [{"display": "xe-0/0/0", "device": {"display": "edge01"}}],
            {
                "display": "#3",
                "type": {"value": "cat6", "label": "CAT6"},
                "status": {"value": "connected", "label": "Connected"},
                "length": 5, "length_unit": {"value": "m", "label": "Meters"}
            },
            [{"display": "xe-1/0/0", "device": {"display": "core01"}}]
        ])];
        let view = InterfaceView::build(iface, vec![], trace);
        // The cable segment carries label · type · length(unit) · status.
        assert!(
            view.diagram
                .iter()
                .any(|l| l == "    ┿ #3 · CAT6 · 5m · Connected"),
            "got: {:?}",
            view.diagram
        );
    }

    #[test]
    fn cable_diagram_falls_back_to_cable_id_when_label_absent() {
        // The trace serializer sends `display_url` (not `display`) and an empty
        // `label`, with status as a plain string — the real NetBox shape. The cable
        // name must fall back to `#<id>` rather than vanishing.
        let iface: Interface =
            serde_json::from_value(json!({"id": 1, "url": "u", "name": "swp25"})).unwrap();
        // The far device carries an asset-tagged display plus a bare name.
        let trace = vec![json!([
            [{"display": "swp25", "device": {"display": "edge01"}}],
            {"id": 4120, "display_url": "u", "label": "", "type": null, "status": "connected"},
            [{"display": "1/1/c13/1", "device": {"display": "core01 (m999)", "name": "core01"}}]
        ])];
        let view = InterfaceView::build(iface, vec![], trace);
        // Cable id falls back to `#<id>`; the bare status value is title-cased.
        assert!(
            view.diagram.iter().any(|l| l == "    ┿ #4120 · Connected"),
            "got: {:?}",
            view.diagram
        );
        // The far device shows its stable name (A/Z), not the asset-tagged display.
        assert!(
            view.diagram.iter().any(|l| l == " Z  core01"),
            "{:?}",
            view.diagram
        );
        assert!(
            !view.diagram.iter().any(|l| l.contains("(m999)")),
            "asset tag leaked into the diagram: {:?}",
            view.diagram
        );
        assert!(
            view.diagram.iter().any(|l| l == "    1/1/c13/1"),
            "{:?}",
            view.diagram
        );
    }

    #[test]
    fn cable_diagram_collapses_a_patch_panel_to_one_pass_through() {
        let iface: Interface =
            serde_json::from_value(json!({"id": 1, "url": "u", "name": "xe-0/0/0"})).unwrap();
        // Two hops through a patch panel: hop 1 lands on the panel front, hop 2
        // leaves from its rear — the two faces share the panel device.
        let trace = vec![
            json!([
                [{"display": "xe-0/0/0", "device": {"display": "edge01"}}],
                {"display": "#3"},
                [{"display": "front1", "device": {"display": "panel-a"}}]
            ]),
            json!([
                [{"display": "rear1", "device": {"display": "panel-a"}}],
                {"display": "#4"},
                [{"display": "xe-1/0/0", "device": {"display": "core01"}}]
            ]),
        ];
        let view = InterfaceView::build(iface, vec![], trace);
        // The panel is one mid stop: its device named once, both faces on one line.
        assert_eq!(
            view.diagram
                .iter()
                .filter(|l| l.as_str() == "    panel-a")
                .count(),
            1,
            "panel not collapsed: {:?}",
            view.diagram
        );
        assert!(
            view.diagram.iter().any(|l| l == "    front1 → rear1"),
            "pass-through not shown: {:?}",
            view.diagram
        );
        // Endpoints are the A (near) and Z (far) devices; both cables are drawn.
        assert_eq!(view.diagram.first().map(String::as_str), Some(" A  edge01"));
        assert!(view.diagram.iter().any(|l| l == " Z  core01"));
        assert!(view.diagram.iter().any(|l| l.contains("┿ #3")));
        assert!(view.diagram.iter().any(|l| l.contains("┿ #4")));
    }
}
