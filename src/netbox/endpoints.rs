//! REST endpoint paths, grouped by NetBox application area.

/// A NetBox REST list/detail endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    Devices,
    Interfaces,
    Sites,
    Racks,
    IpAddresses,
    Prefixes,
    Vlans,
    Vrfs,
    Tenants,
    VirtualMachines,
    Circuits,
    Aggregates,
    Asns,
    Services,
}

impl Endpoint {
    /// The API path for this endpoint, with leading and trailing slashes.
    pub fn path(self) -> &'static str {
        match self {
            Endpoint::Devices => "/api/dcim/devices/",
            Endpoint::Interfaces => "/api/dcim/interfaces/",
            Endpoint::Sites => "/api/dcim/sites/",
            Endpoint::Racks => "/api/dcim/racks/",
            Endpoint::IpAddresses => "/api/ipam/ip-addresses/",
            Endpoint::Prefixes => "/api/ipam/prefixes/",
            Endpoint::Vlans => "/api/ipam/vlans/",
            Endpoint::Vrfs => "/api/ipam/vrfs/",
            Endpoint::Tenants => "/api/tenancy/tenants/",
            Endpoint::VirtualMachines => "/api/virtualization/virtual-machines/",
            Endpoint::Circuits => "/api/circuits/circuits/",
            Endpoint::Aggregates => "/api/ipam/aggregates/",
            Endpoint::Asns => "/api/ipam/asns/",
            Endpoint::Services => "/api/ipam/services/",
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
