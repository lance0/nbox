//! REST endpoint paths, grouped by NetBox application area.

/// A NetBox REST list/detail endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    Devices,
    Interfaces,
    Sites,
    Regions,
    SiteGroups,
    Locations,
    Racks,
    IpAddresses,
    Prefixes,
    Vlans,
    VlanGroups,
    Vrfs,
    Tenants,
    Contacts,
    VirtualMachines,
    Circuits,
    Providers,
    Aggregates,
    Asns,
    Services,
    IpRanges,
    JournalEntries,
    Tags,
}

impl Endpoint {
    /// The API path for this endpoint, with leading and trailing slashes.
    pub fn path(self) -> &'static str {
        match self {
            Endpoint::Devices => "/api/dcim/devices/",
            Endpoint::Interfaces => "/api/dcim/interfaces/",
            Endpoint::Sites => "/api/dcim/sites/",
            Endpoint::Regions => "/api/dcim/regions/",
            Endpoint::SiteGroups => "/api/dcim/site-groups/",
            Endpoint::Locations => "/api/dcim/locations/",
            Endpoint::Racks => "/api/dcim/racks/",
            Endpoint::IpAddresses => "/api/ipam/ip-addresses/",
            Endpoint::Prefixes => "/api/ipam/prefixes/",
            Endpoint::Vlans => "/api/ipam/vlans/",
            Endpoint::VlanGroups => "/api/ipam/vlan-groups/",
            Endpoint::Vrfs => "/api/ipam/vrfs/",
            Endpoint::Tenants => "/api/tenancy/tenants/",
            Endpoint::Contacts => "/api/tenancy/contacts/",
            Endpoint::VirtualMachines => "/api/virtualization/virtual-machines/",
            Endpoint::Circuits => "/api/circuits/circuits/",
            Endpoint::Providers => "/api/circuits/providers/",
            Endpoint::Aggregates => "/api/ipam/aggregates/",
            Endpoint::Asns => "/api/ipam/asns/",
            Endpoint::Services => "/api/ipam/services/",
            Endpoint::IpRanges => "/api/ipam/ip-ranges/",
            Endpoint::JournalEntries => "/api/extras/journal-entries/",
            Endpoint::Tags => "/api/extras/tags/",
        }
    }

    /// Whether this endpoint renders a (potentially large) config context that
    /// should be excluded by default for performance.
    pub fn has_config_context(self) -> bool {
        matches!(self, Endpoint::Devices | Endpoint::VirtualMachines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_well_formed() {
        for ep in [Endpoint::Devices, Endpoint::IpAddresses, Endpoint::Vlans] {
            assert!(ep.path().starts_with("/api/"));
            assert!(ep.path().ends_with('/'));
        }
    }

    #[test]
    fn only_devices_and_vms_carry_config_context() {
        assert!(Endpoint::Devices.has_config_context());
        assert!(Endpoint::VirtualMachines.has_config_context());
        assert!(!Endpoint::Sites.has_config_context());
    }
}
