//! UI-ready view models, normalized from the `netbox` wire models.
//!
//! Wire models (`crate::netbox::models`) mirror NetBox serializers — nested,
//! nullable, brief/complete. View models flatten those into the fields nbx
//! actually renders (plain text and JSON), so the presentation layer never
//! reaches into raw API shapes.

pub mod device_view;
pub mod ip_view;
pub mod prefix_view;
