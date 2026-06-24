//! The tagged-objects view: a flat list of objects carrying a tag, for
//! `nbox tagged <tag>` (CLI + MCP). Backed by NetBox 4.3's
//! `/api/extras/tagged-objects/?tag_id=<id>` — a cross-kind reverse lookup ("what
//! has tag X") that the per-endpoint `search --tag` can't answer in one call.

use serde::Serialize;

use crate::netbox::models::common::BriefObject;
use crate::netbox::models::extras::TaggedObject;
use crate::output::plain::KeyValues;

/// One object carrying a tag, flattened from the polymorphic
/// [`TaggedObject`] wire row. The `kind` label and dotted `object_type` let a
/// human scan and an agent branch without re-parsing `display`.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct TaggedObjectView {
    /// Friendly kind label nbox uses elsewhere (`device`, `ip`, `rack`, …) for
    /// known content types; the dotted type's last segment for an unknown one
    /// (so a cable/service still renders instead of being dropped).
    pub kind: String,
    /// The dotted content type of the tagged object (`dcim.device`,
    /// `ipam.ipaddress`, …) — structured, for agents.
    pub object_type: String,
    /// The tagged object's numeric id.
    pub id: u64,
    /// The tagged object's display label (its name, or NetBox's `display`).
    pub display: String,
    /// The tagged object's API URL.
    pub url: String,
}

impl TaggedObjectView {
    /// Flatten a wire [`TaggedObject`] row. The polymorphic `object` brief
    /// carries the object's own display/name/url (always present in practice),
    /// so prefer it; `label()` always resolves to at least `#<id>`.
    #[must_use]
    pub fn from_model(row: TaggedObject) -> Self {
        let object_type = row.object_type;
        let kind = kind_label(&object_type);
        // `object` is present in practice; fall back to the row's `display`/`url`
        // (NetBox's "X tagged with Y" sentence / the tagged-object URL) only if a
        // non-standard row omits the brief.
        let display = row
            .object
            .as_ref()
            .map(BriefObject::label)
            .or(row.display)
            .unwrap_or_default();
        let url = row
            .object
            .as_ref()
            .and_then(|o| o.url.clone())
            .or(row.url)
            .unwrap_or_default();
        Self {
            kind,
            object_type,
            id: row.object_id,
            display,
            url,
        }
    }

    /// Plain-text rendering (`-o plain`): one `kind  display` row per object.
    #[must_use]
    pub fn to_key_values(&self) -> KeyValues {
        let mut kv = KeyValues::new();
        kv.push("kind", self.kind.clone())
            .push("object_type", self.object_type.clone())
            .push("id", self.id.to_string())
            .push("display", self.display.clone())
            .push_opt(
                "url",
                if self.url.is_empty() {
                    None
                } else {
                    Some(self.url.clone())
                },
            );
        kv
    }
}

/// The report `nbox tagged` emits: the resolved tag plus the objects carrying
/// it. Carrying the tag (id/name/slug) lets an agent confirm which tag the
/// results are for after a name/slug resolution.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct TaggedReport {
    /// The tag these objects carry (the resolved reference).
    pub tag: ResolvedTag,
    /// Objects carrying the tag, across kinds.
    pub results: Vec<TaggedObjectView>,
}

/// The resolved tag a `nbox tagged` result set is for.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ResolvedTag {
    pub id: u64,
    pub name: String,
    pub slug: String,
}

impl ResolvedTag {
    #[must_use]
    pub fn from_info(t: crate::netbox::models::extras::TagInfo) -> Self {
        Self {
            id: t.id,
            name: t.name,
            slug: t.slug,
        }
    }
}

/// Map a dotted NetBox content type (`dcim.device`, `ipam.ipaddress`, …) to the
/// friendly kind label nbox uses in its `ObjectKind::as_str` — so a tagged row
/// reads `device`/`ip`/`rack` exactly like a search hit. An unknown content
/// type falls back to its last segment (e.g. `dcim.cable` → `cable`) so the row
/// is still shown rather than dropped — the polymorphic endpoint can return
/// object types nbox doesn't model.
#[must_use]
pub fn kind_label(object_type: &str) -> String {
    let known: Option<&str> = match object_type {
        "dcim.device" => Some("device"),
        "dcim.site" => Some("site"),
        "dcim.rack" => Some("rack"),
        "dcim.interface" => Some("interface"),
        "dcim.macaddress" => Some("mac"),
        "ipam.ipaddress" => Some("ip"),
        "ipam.prefix" => Some("prefix"),
        "ipam.vlan" => Some("vlan"),
        "ipam.aggregate" => Some("aggregate"),
        "ipam.asn" => Some("asn"),
        "ipam.iprange" => Some("ip-range"),
        "ipam.vrf" => Some("vrf"),
        "ipam.routetarget" => Some("route-target"),
        "tenancy.tenant" => Some("tenant"),
        "tenancy.contact" => Some("contact"),
        "circuits.circuit" => Some("circuit"),
        "circuits.provider" => Some("provider"),
        "virtualization.virtualmachine" => Some("vm"),
        "virtualization.cluster" => Some("cluster"),
        _ => None,
    };
    // Unknown content type: the last segment of the dotted type, as-is. The
    // polymorphic endpoint can return object types nbox doesn't model; show
    // them with a derived label rather than drop the row.
    known
        .unwrap_or_else(|| object_type.rsplit('.').next().unwrap_or(object_type))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netbox::models::extras::TaggedObject;
    use serde_json::json;

    fn row(object_type: &str, obj: serde_json::Value) -> TaggedObject {
        serde_json::from_value(json!({
            "id": 99,
            "url": "http://nb/api/extras/tagged-objects/99/",
            "object_type": object_type,
            "object_id": 7,
            "object": obj,
            "tag": {"id": 42, "name": "prod", "slug": "prod"},
            "display": "edge01 tagged with prod"
        }))
        .unwrap()
    }

    #[test]
    fn from_model_reads_the_object_brief_for_known_kinds() {
        let v = TaggedObjectView::from_model(row(
            "dcim.device",
            json!({"id": 7, "name": "edge01", "display": "edge01", "url": "http://nb/api/dcim/devices/7/"}),
        ));
        assert_eq!(v.kind, "device");
        assert_eq!(v.object_type, "dcim.device");
        assert_eq!(v.id, 7);
        assert_eq!(v.display, "edge01");
        assert_eq!(v.url, "http://nb/api/dcim/devices/7/");
    }

    #[test]
    fn kind_label_maps_every_known_content_type() {
        assert_eq!(kind_label("ipam.ipaddress"), "ip");
        assert_eq!(kind_label("ipam.iprange"), "ip-range");
        assert_eq!(kind_label("ipam.routetarget"), "route-target");
        assert_eq!(kind_label("virtualization.virtualmachine"), "vm");
        assert_eq!(kind_label("dcim.macaddress"), "mac");
    }

    #[test]
    fn kind_label_falls_back_to_the_last_segment_for_unknown_types() {
        // A cable isn't an nbox kind; the row still renders with a derived label
        // rather than being dropped from the polymorphic result set.
        assert_eq!(kind_label("dcim.cable"), "cable");
        assert_eq!(kind_label("ipam.service"), "service");
    }

    #[test]
    fn from_model_uses_the_object_brief_label_when_present() {
        // The object brief carries the object's own display — prefer it over the
        // row's `display`, which is NetBox's "X tagged with Y" sentence, not the
        // object's name.
        let v = TaggedObjectView::from_model(row(
            "dcim.cable",
            json!({"id": 7, "display": "Cable #7", "url": "http://nb/api/dcim/cables/7/"}),
        ));
        assert_eq!(v.kind, "cable");
        assert_eq!(v.display, "Cable #7");
        assert_eq!(v.url, "http://nb/api/dcim/cables/7/");
    }

    #[test]
    fn from_model_falls_back_to_id_label_when_the_brief_has_only_an_id() {
        // A brief with no display/name still resolves to `#<id>` (always useful —
        // it's the object's own id), not the row's "tagged with" sentence.
        let v = TaggedObjectView::from_model(row("dcim.cable", json!({"id": 7})));
        assert_eq!(v.display, "#7");
    }

    #[test]
    fn from_model_falls_back_to_row_display_when_object_brief_is_absent() {
        // A non-standard row with no `object` brief: `object` deserializes to
        // `None` (`#[serde(default)]`), so use the row's own `display`/`url` to
        // keep the object identifiable (rare in practice — the endpoint embeds it).
        let row: TaggedObject = serde_json::from_value(json!({
            "id": 99, "url": "http://nb/api/extras/tagged-objects/99/",
            "object_type": "dcim.cable", "object_id": 7,
            "tag": {"id": 42, "name": "prod", "slug": "prod"},
            "display": "edge01 tagged with prod"
        }))
        .unwrap();
        let v = TaggedObjectView::from_model(row);
        assert_eq!(v.display, "edge01 tagged with prod");
        assert_eq!(v.url, "http://nb/api/extras/tagged-objects/99/");
    }

    #[test]
    fn to_key_values_renders_kind_and_display() {
        let v = TaggedObjectView::from_model(row(
            "ipam.prefix",
            json!({"id": 7, "display": "10.0.0.0/24", "url": "http://nb/api/ipam/prefixes/7/"}),
        ));
        let rendered = v.to_key_values().render();
        assert!(rendered.contains("kind: prefix"), "{rendered}");
        assert!(rendered.contains("display: 10.0.0.0/24"), "{rendered}");
    }

    #[test]
    fn resolved_tag_from_info_carries_id_name_slug() {
        let info = crate::netbox::models::extras::TagInfo {
            id: 42,
            url: None,
            name: "prod:us-east".into(),
            slug: "produs-east".into(),
            color: None,
            description: None,
            tagged_items: None,
        };
        let r = ResolvedTag::from_info(info);
        assert_eq!(r.id, 42);
        assert_eq!(r.name, "prod:us-east");
        assert_eq!(r.slug, "produs-east");
    }

    #[test]
    fn brief_object_label_handles_a_name_only_brief() {
        // Some object briefs carry `name` but no `display` — label() must still
        // resolve it so the row isn't blank.
        let v =
            TaggedObjectView::from_model(row("tenancy.tenant", json!({"id": 7, "name": "Acme"})));
        assert_eq!(v.display, "Acme");
    }
}
