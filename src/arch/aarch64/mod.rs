//! ARM EL2 mechanisms. Domain policy, device discovery and DMA admission are
//! owned by the hypervisor's resource managers, not register/table helpers.
pub mod stage2;
pub mod vcpu;
pub mod vgic;
