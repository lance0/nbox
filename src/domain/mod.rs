//! UI-ready view models, normalized from the `netbox` wire models.
//!
//! Wire models (`crate::netbox::models`) mirror NetBox serializers — nested,
//! nullable, brief/complete. View models flatten those into the fields nbox
//! actually renders (plain text and JSON), so the presentation layer never
//! reaches into raw API shapes.

pub mod aggregate_view;
pub mod asn_view;
pub mod circuit_view;
pub mod custom;
pub mod detail;
pub mod device_detail;
pub mod device_view;
pub mod interface_view;
pub mod ip_range_view;
pub mod ip_view;
pub mod journal_view;
pub mod prefix_view;
pub mod rack_view;
pub mod site_view;
pub mod vlan_view;
