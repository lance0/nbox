//! UI-ready view models, normalized from the `netbox` wire models.
//!
//! Wire models (`crate::netbox::models`) mirror NetBox serializers — nested,
//! nullable, brief/complete. View models flatten those into the fields nbox
//! actually renders (plain text and JSON), so the presentation layer never
//! reaches into raw API shapes.

pub mod aggregate_view;
pub mod asn_view;
pub mod circuit_view;
pub mod cluster_view;
pub mod contact_view;
pub mod custom;
pub mod detail;
pub mod device_detail;
pub mod device_view;
pub mod interface_view;
pub mod ip_range_view;
pub mod ip_view;
pub mod journal_view;
pub mod mac_view;
pub mod prefix_view;
pub mod provider_view;
pub mod rack_group_view;
pub mod rack_view;
pub mod route_target_view;
pub mod site_view;
pub mod tag_view;
pub mod tagged_view;
pub mod tenant_view;
pub mod util;
pub mod virtual_circuit_view;
pub mod vlan_view;
pub mod vm_type_view;
pub mod vm_view;
pub mod vrf_view;

use serde::Serialize;

use crate::domain::journal_view::JournalEntryRow;

/// A detail view augmented with its recent journal entries, emitted only when a
/// detail command is run with `--journal`. The inner view serializes exactly as
/// it does without the flag (it is flattened), so JSON gains a single top-level
/// `journal` array and is otherwise byte-identical to the bare view.
#[derive(Debug, Clone, Serialize)]
pub struct WithJournal<T> {
    #[serde(flatten)]
    pub inner: T,
    pub journal: Vec<JournalEntryRow>,
}

impl<T> WithJournal<T> {
    /// Wrap a view with its journal rows.
    pub fn new(inner: T, journal: Vec<JournalEntryRow>) -> Self {
        Self { inner, journal }
    }
}
