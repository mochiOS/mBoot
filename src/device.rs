use mnu_abi::hypervisor::{
    PciDeviceInfo, PCI_DEVICE_FLAG_CLAIMABLE, PCI_DEVICE_FLAG_EPHEMERAL,
    PCI_DEVICE_FLAG_PARTITIONED, PCI_DEVICE_FLAG_READ_ONLY, PCI_DEVICE_STATE_ACTIVE,
    PCI_DEVICE_STATE_CLAIMED_DISABLED, PCI_DEVICE_STATE_FIRMWARE_DEFERRED,
    PCI_DEVICE_STATE_QUARANTINED,
};

use crate::manifest::{ManifestDevice, ManifestDeviceKind, AUTO_REQUESTER};
use crate::pci::PciFunction;

const MAX_DEVICES: usize = 256;

const EMPTY_INFO: PciDeviceInfo = PciDeviceInfo {
    requester: 0,
    class: 0,
    subclass: 0,
    state: 0,
    _reserved0: [0; 3],
    owner_domain: 0,
    flags: 0,
    storage_disk_guid: [0; 16],
    storage_partition_type_guid: [0; 16],
    storage_partition_guid: [0; 16],
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DeviceRecord {
    info: PciDeviceInfo,
    allowed_domain: u32,
}

const EMPTY_RECORD: DeviceRecord = DeviceRecord {
    info: EMPTY_INFO,
    allowed_domain: 0,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceError {
    InvalidPolicy,
    DeviceUnavailable,
    AmbiguousDevice(PciFunction, PciFunction),
    PermissionDenied,
    InvalidState,
}

pub struct DeviceTable {
    devices: [DeviceRecord; MAX_DEVICES],
    count: usize,
}

impl DeviceTable {
    pub fn from_pci(
        functions: &[PciFunction],
        deferred_display: Option<u16>,
        policies: &[ManifestDevice],
    ) -> Result<Self, DeviceError> {
        let mut table = Self {
            devices: [EMPTY_RECORD; MAX_DEVICES],
            count: 0,
        };
        for function in functions.iter().take(MAX_DEVICES) {
            table.devices[table.count].info = PciDeviceInfo {
                requester: function.requester,
                class: function.class,
                subclass: function.subclass,
                state: if deferred_display == Some(function.requester) {
                    PCI_DEVICE_STATE_FIRMWARE_DEFERRED
                } else {
                    PCI_DEVICE_STATE_QUARANTINED
                },
                _reserved0: [0; 3],
                owner_domain: 0,
                flags: 0,
                storage_disk_guid: [0; 16],
                storage_partition_type_guid: [0; 16],
                storage_partition_guid: [0; 16],
            };
            table.count += 1;
        }
        for policy in policies {
            if policy.segment != 0 {
                return Err(DeviceError::InvalidPolicy);
            }
            if policy.requester == AUTO_REQUESTER
                && matches!(
                    policy.kind,
                    ManifestDeviceKind::Display
                        | ManifestDeviceKind::Nvme
                        | ManifestDeviceKind::Vmd
                        | ManifestDeviceKind::Usb
                        | ManifestDeviceKind::Network
                )
            {
                let mut matched = 0;
                for record in &mut table.devices[..table.count] {
                    if !kind_matches(record.info, policy.kind)
                        || (policy.kind == ManifestDeviceKind::Network && record.info.subclass != 0)
                    {
                        continue;
                    }
                    if record.allowed_domain != 0 {
                        return Err(DeviceError::InvalidPolicy);
                    }
                    record.allowed_domain = policy.domain_id;
                    if policy.is_ephemeral() {
                        record.info.flags |= PCI_DEVICE_FLAG_EPHEMERAL;
                    }
                    if policy.is_read_only() {
                        record.info.flags |= PCI_DEVICE_FLAG_READ_ONLY;
                    }
                    if policy.is_partitioned() {
                        return Err(DeviceError::InvalidPolicy);
                    }
                    matched += 1;
                }
                if matched == 0 && policy.is_required() {
                    return Err(DeviceError::DeviceUnavailable);
                }
                continue;
            }
            let record = if policy.requester == AUTO_REQUESTER {
                let mut candidate = None;
                let mut first_candidate = None;
                for (index, record) in table.devices[..table.count].iter().enumerate() {
                    if !kind_matches(record.info, policy.kind) {
                        continue;
                    }
                    if let Some(first) = first_candidate {
                        return Err(DeviceError::AmbiguousDevice(first, functions[index]));
                    }
                    first_candidate = Some(functions[index]);
                    candidate = Some(index);
                }
                let Some(index) = candidate else {
                    if policy.is_required() {
                        return Err(DeviceError::DeviceUnavailable);
                    }
                    continue;
                };
                &mut table.devices[index]
            } else {
                let Some(record) = table.devices[..table.count]
                    .iter_mut()
                    .find(|record| record.info.requester == policy.requester)
                else {
                    if policy.is_required() {
                        return Err(DeviceError::DeviceUnavailable);
                    }
                    continue;
                };
                record
            };
            if record.allowed_domain != 0 || !kind_matches(record.info, policy.kind) {
                return Err(DeviceError::InvalidPolicy);
            }
            record.allowed_domain = policy.domain_id;
            if policy.is_ephemeral() {
                record.info.flags |= PCI_DEVICE_FLAG_EPHEMERAL;
            }
            if policy.is_read_only() {
                record.info.flags |= PCI_DEVICE_FLAG_READ_ONLY;
            }
            if policy.is_partitioned() {
                record.info.flags |= PCI_DEVICE_FLAG_PARTITIONED;
                record.info.storage_disk_guid = policy.storage_disk_guid;
                record.info.storage_partition_type_guid = policy.storage_partition_type_guid;
                record.info.storage_partition_guid = policy.storage_partition_guid;
            }
        }
        Ok(table)
    }

    pub fn query(&self, domain_id: u32, index: usize) -> Option<PciDeviceInfo> {
        let record = self.devices.get(index).filter(|_| index < self.count)?;
        let mut info = record.info;
        if record.allowed_domain == domain_id
            && matches!(
                info.state,
                PCI_DEVICE_STATE_QUARANTINED | PCI_DEVICE_STATE_FIRMWARE_DEFERRED
            )
        {
            info.flags |= PCI_DEVICE_FLAG_CLAIMABLE;
        }
        if self.is_config_dependency(domain_id, info.requester) {
            info.flags |= PCI_DEVICE_FLAG_READ_ONLY;
        }
        Some(info).filter(PciDeviceInfo::validate)
    }

    /// Allows a hardware domain to inspect the host bridge configuration needed
    /// by its assigned display controller without assigning the bridge itself.
    pub fn can_read_config(&self, domain_id: u32, requester: u16) -> bool {
        self.record(requester).is_ok_and(|record| {
            record.info.owner_domain == domain_id
                || self.is_config_dependency(domain_id, requester)
        })
    }

    pub fn claim(&mut self, domain_id: u32, requester: u16) -> Result<(), DeviceError> {
        self.can_claim(domain_id, requester)?;
        let record = self
            .devices
            .get_mut(..self.count)
            .and_then(|devices| {
                devices
                    .iter_mut()
                    .find(|record| record.info.requester == requester)
            })
            .ok_or(DeviceError::DeviceUnavailable)?;
        record.info.state = PCI_DEVICE_STATE_CLAIMED_DISABLED;
        record.info.owner_domain = domain_id;
        Ok(())
    }

    pub fn release(&mut self, domain_id: u32, requester: u16) -> Result<(), DeviceError> {
        self.can_release(domain_id, requester)?;
        let record = self
            .devices
            .get_mut(..self.count)
            .and_then(|devices| {
                devices
                    .iter_mut()
                    .find(|record| record.info.requester == requester)
            })
            .ok_or(DeviceError::DeviceUnavailable)?;
        record.info.state = PCI_DEVICE_STATE_QUARANTINED;
        record.info.owner_domain = 0;
        Ok(())
    }

    pub fn activate(&mut self, domain_id: u32, requester: u16) -> Result<(), DeviceError> {
        self.can_activate(domain_id, requester)?;
        let record = self.record_mut(requester)?;
        record.info.state = PCI_DEVICE_STATE_ACTIVE;
        Ok(())
    }

    pub fn can_activate(&self, domain_id: u32, requester: u16) -> Result<(), DeviceError> {
        let record = self.record(requester)?;
        if record.info.owner_domain != domain_id {
            return Err(DeviceError::PermissionDenied);
        }
        if record.info.state != PCI_DEVICE_STATE_CLAIMED_DISABLED {
            return Err(DeviceError::InvalidState);
        }
        Ok(())
    }

    pub fn can_claim(&self, domain_id: u32, requester: u16) -> Result<(), DeviceError> {
        let record = self.record(requester)?;
        if record.allowed_domain != domain_id {
            return Err(DeviceError::PermissionDenied);
        }
        if !matches!(
            record.info.state,
            PCI_DEVICE_STATE_QUARANTINED | PCI_DEVICE_STATE_FIRMWARE_DEFERRED
        ) || record.info.owner_domain != 0
        {
            return Err(DeviceError::InvalidState);
        }
        Ok(())
    }

    pub fn is_firmware_deferred(&self, requester: u16) -> bool {
        self.record(requester).is_ok_and(|record| {
            record.info.state == PCI_DEVICE_STATE_FIRMWARE_DEFERRED
        })
    }

    pub fn can_release(&self, domain_id: u32, requester: u16) -> Result<(), DeviceError> {
        let record = self.record(requester)?;
        if record.info.owner_domain != domain_id {
            return Err(DeviceError::PermissionDenied);
        }
        if !matches!(
            record.info.state,
            PCI_DEVICE_STATE_CLAIMED_DISABLED | PCI_DEVICE_STATE_ACTIVE
        ) {
            return Err(DeviceError::InvalidState);
        }
        Ok(())
    }

    pub fn claimed_requesters(&self, domain_id: u32) -> impl Iterator<Item = u16> + '_ {
        self.devices[..self.count]
            .iter()
            .filter(move |record| record.info.owner_domain == domain_id)
            .map(|record| record.info.requester)
    }

    fn record(&self, requester: u16) -> Result<&DeviceRecord, DeviceError> {
        self.devices[..self.count]
            .iter()
            .find(|record| record.info.requester == requester)
            .ok_or(DeviceError::DeviceUnavailable)
    }

    fn record_mut(&mut self, requester: u16) -> Result<&mut DeviceRecord, DeviceError> {
        self.devices[..self.count]
            .iter_mut()
            .find(|record| record.info.requester == requester)
            .ok_or(DeviceError::DeviceUnavailable)
    }

    pub fn release_domain(&mut self, domain_id: u32) -> usize {
        let mut released = 0;
        for record in &mut self.devices[..self.count] {
            if record.info.owner_domain == domain_id {
                record.info.state = PCI_DEVICE_STATE_QUARANTINED;
                record.info.owner_domain = 0;
                released += 1;
            }
        }
        released
    }

    pub const fn len(&self) -> usize {
        self.count
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn is_config_dependency(&self, domain_id: u32, requester: u16) -> bool {
        requester == 0
            && self.record(requester).is_ok_and(|record| {
                record.info.class == 0x06 && record.info.subclass == 0x00
            })
            && self.devices[..self.count].iter().any(|record| {
                record.allowed_domain == domain_id && record.info.class == 0x03
            })
    }
}

const fn kind_matches(info: PciDeviceInfo, kind: ManifestDeviceKind) -> bool {
    match kind {
        ManifestDeviceKind::Other => true,
        ManifestDeviceKind::Display => info.class == 0x03,
        ManifestDeviceKind::Block => info.class == 0x01,
        ManifestDeviceKind::Network => info.class == 0x02,
        ManifestDeviceKind::Usb => info.class == 0x0c && info.subclass == 0x03,
        ManifestDeviceKind::Audio => info.class == 0x04,
        ManifestDeviceKind::Nvme => info.class == 0x01 && info.subclass == 0x08,
        ManifestDeviceKind::Vmd => info.class == 0x01 && info.subclass == 0x04,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{
        DEVICE_FLAG_EPHEMERAL, DEVICE_FLAG_PARTITIONED, DEVICE_FLAG_READ_ONLY, DEVICE_FLAG_REQUIRED,
    };

    fn policy(requester: u16, kind: ManifestDeviceKind) -> ManifestDevice {
        ManifestDevice {
            segment: 0,
            requester,
            kind,
            flags: DEVICE_FLAG_REQUIRED,
            domain_id: 2,
            storage_disk_guid: [0; 16],
            storage_partition_type_guid: [0; 16],
            storage_partition_guid: [0; 16],
        }
    }

    #[test]
    fn claim_and_release_follow_the_signed_policy() {
        let functions = [PciFunction {
            requester: 0x00a0,
            class: 0x0c,
            subclass: 0x03,
            ..PciFunction::default()
        }];
        let mut table =
            DeviceTable::from_pci(&functions, None, &[policy(0x00a0, ManifestDeviceKind::Usb)])
                .unwrap();
        assert_ne!(
            table.query(2, 0).unwrap().flags & PCI_DEVICE_FLAG_CLAIMABLE,
            0
        );
        assert_eq!(table.claim(3, 0x00a0), Err(DeviceError::PermissionDenied));
        table.claim(2, 0x00a0).unwrap();
        assert_eq!(
            table.query(2, 0).unwrap().state,
            PCI_DEVICE_STATE_CLAIMED_DISABLED
        );
        table.activate(2, 0x00a0).unwrap();
        assert_eq!(table.query(2, 0).unwrap().state, PCI_DEVICE_STATE_ACTIVE);
        assert_eq!(table.release_domain(2), 1);
        assert_eq!(
            table.query(2, 0).unwrap().state,
            PCI_DEVICE_STATE_QUARANTINED
        );
    }

    #[test]
    fn automatic_network_policy_assigns_only_ethernet_to_its_domain() {
        let functions = [
            PciFunction { requester: 0x0100, class: 2, subclass: 0, ..PciFunction::default() },
            PciFunction { requester: 0x0200, class: 2, subclass: 0x80, ..PciFunction::default() },
            PciFunction { requester: 0x0010, class: 3, subclass: 0, ..PciFunction::default() },
        ];
        let mut table = DeviceTable::from_pci(
            &functions, None, &[policy(AUTO_REQUESTER, ManifestDeviceKind::Network)],
        ).unwrap();
        assert!(table.can_claim(2, 0x0100).is_ok());
        assert_eq!(table.can_claim(3, 0x0100), Err(DeviceError::PermissionDenied));
        assert_eq!(table.can_claim(2, 0x0200), Err(DeviceError::PermissionDenied));
        assert_eq!(table.can_claim(2, 0x0010), Err(DeviceError::PermissionDenied));
        table.claim(2, 0x0100).unwrap();
        assert_eq!(table.query(2, 0).unwrap().owner_domain, 2);
    }

    #[test]
    fn required_automatic_network_rejects_wifi_only_machine() {
        let functions = [PciFunction {
            requester: 0x0200, class: 2, subclass: 0x80, ..PciFunction::default()
        }];
        assert!(matches!(DeviceTable::from_pci(
            &functions, None, &[policy(AUTO_REQUESTER, ManifestDeviceKind::Network)],
        ), Err(DeviceError::DeviceUnavailable)));
    }

    #[test]
    fn firmware_display_is_claimable_only_by_its_policy_domain() {
        let functions = [PciFunction {
            requester: 0x0010,
            class: 0x03,
            subclass: 0,
            ..PciFunction::default()
        }];
        let mut table = DeviceTable::from_pci(
            &functions,
            Some(0x0010),
            &[policy(0x0010, ManifestDeviceKind::Display)],
        )
        .unwrap();
        assert_ne!(
            table.query(2, 0).unwrap().flags & PCI_DEVICE_FLAG_CLAIMABLE,
            0
        );
        assert_eq!(table.claim(3, 0x0010), Err(DeviceError::PermissionDenied));
        table.claim(2, 0x0010).unwrap();
    }

    #[test]
    fn ephemeral_policy_survives_claim_and_activation() {
        let functions = [PciFunction {
            requester: 0x0018,
            class: 0x01,
            subclass: 0,
            ..PciFunction::default()
        }];
        let mut ephemeral = policy(0x0018, ManifestDeviceKind::Block);
        ephemeral.flags |= DEVICE_FLAG_EPHEMERAL;
        let mut table = DeviceTable::from_pci(&functions, None, &[ephemeral]).unwrap();
        table.claim(2, 0x0018).unwrap();
        assert_eq!(table.query(2, 0).unwrap().flags, PCI_DEVICE_FLAG_EPHEMERAL);
        table.activate(2, 0x0018).unwrap();
        assert_eq!(table.query(2, 0).unwrap().flags, PCI_DEVICE_FLAG_EPHEMERAL);
    }

    #[test]
    fn required_missing_or_wrong_class_devices_fail_closed() {
        let functions = [PciFunction {
            requester: 0x0010,
            class: 0x03,
            subclass: 0,
            ..PciFunction::default()
        }];
        assert!(matches!(
            DeviceTable::from_pci(
                &functions,
                None,
                &[policy(0x0018, ManifestDeviceKind::Block)]
            ),
            Err(DeviceError::DeviceUnavailable)
        ));
        assert!(matches!(
            DeviceTable::from_pci(
                &functions,
                None,
                &[policy(0x0010, ManifestDeviceKind::Block)]
            ),
            Err(DeviceError::InvalidPolicy)
        ));
    }

    #[test]
    fn automatic_nvme_policy_selects_one_matching_controller_read_only() {
        let functions = [
            PciFunction {
                requester: 0x001f,
                class: 0x01,
                subclass: 0x06,
                ..PciFunction::default()
            },
            PciFunction {
                requester: 0x0100,
                class: 0x01,
                subclass: 0x08,
                ..PciFunction::default()
            },
        ];
        let mut nvme = policy(AUTO_REQUESTER, ManifestDeviceKind::Nvme);
        nvme.flags |= DEVICE_FLAG_READ_ONLY;
        let table = DeviceTable::from_pci(&functions, None, &[nvme]).unwrap();
        assert_eq!(table.query(2, 0).unwrap().flags, 0);
        assert_eq!(
            table.query(2, 1).unwrap().flags,
            PCI_DEVICE_FLAG_CLAIMABLE | PCI_DEVICE_FLAG_READ_ONLY
        );
    }

    #[test]
    fn automatic_nvme_policy_assigns_every_matching_controller() {
        let functions = [
            PciFunction {
                requester: 0x0100,
                vendor: 0x144d,
                device: 0xa808,
                class: 0x01,
                subclass: 0x08,
            },
            PciFunction {
                requester: 0x0200,
                vendor: 0x8086,
                device: 0xf1a8,
                class: 0x01,
                subclass: 0x08,
            },
        ];
        let mut nvme = policy(AUTO_REQUESTER, ManifestDeviceKind::Nvme);
        nvme.flags |= DEVICE_FLAG_READ_ONLY;
        let table = DeviceTable::from_pci(&functions, None, &[nvme]).unwrap();
        for index in 0..2 {
            assert_ne!(
                table.query(2, index).unwrap().flags & PCI_DEVICE_FLAG_CLAIMABLE,
                0
            );
            assert_ne!(
                table.query(2, index).unwrap().flags & PCI_DEVICE_FLAG_READ_ONLY,
                0
            );
        }
    }

    #[test]
    fn automatic_display_policy_assigns_every_gpu_to_the_hardware_domain() {
        let functions = [
            PciFunction {
                requester: 0x0010,
                class: 0x03,
                subclass: 0,
                ..PciFunction::default()
            },
            PciFunction {
                requester: 0x0100,
                class: 0x03,
                subclass: 2,
                ..PciFunction::default()
            },
            PciFunction {
                requester: 0x001f,
                class: 0x04,
                subclass: 3,
                ..PciFunction::default()
            },
        ];
        let table = DeviceTable::from_pci(
            &functions,
            None,
            &[policy(AUTO_REQUESTER, ManifestDeviceKind::Display)],
        )
        .unwrap();
        assert_ne!(
            table.query(2, 0).unwrap().flags & PCI_DEVICE_FLAG_CLAIMABLE,
            0
        );
        assert_ne!(
            table.query(2, 1).unwrap().flags & PCI_DEVICE_FLAG_CLAIMABLE,
            0
        );
        assert_eq!(table.query(2, 2).unwrap().flags, 0);
    }

    #[test]
    fn display_domain_gets_read_only_host_bridge_configuration() {
        let functions = [
            PciFunction {
                requester: 0,
                class: 0x06,
                subclass: 0x00,
                ..PciFunction::default()
            },
            PciFunction {
                requester: 0x0010,
                class: 0x03,
                subclass: 0x00,
                ..PciFunction::default()
            },
        ];
        let table = DeviceTable::from_pci(
            &functions,
            None,
            &[policy(0x0010, ManifestDeviceKind::Display)],
        )
        .unwrap();

        let host = table.query(2, 0).unwrap();
        assert_eq!(host.flags, PCI_DEVICE_FLAG_READ_ONLY);
        assert!(table.can_read_config(2, 0));
        assert!(!table.can_read_config(3, 0));
        assert!(!table.can_read_config(2, 0x0010));
    }

    #[test]
    fn automatic_vmd_policy_does_not_select_a_sata_controller() {
        let functions = [
            PciFunction {
                requester: 0x0070,
                vendor: 0x8086,
                device: 0x9a0b,
                class: 0x01,
                subclass: 0x04,
            },
            PciFunction {
                requester: 0x0080,
                vendor: 0x8086,
                device: 0xa0d3,
                class: 0x01,
                subclass: 0x06,
            },
        ];
        let mut vmd = policy(AUTO_REQUESTER, ManifestDeviceKind::Vmd);
        vmd.flags |= DEVICE_FLAG_READ_ONLY;
        let table = DeviceTable::from_pci(&functions, None, &[vmd]).unwrap();
        assert_eq!(
            table.query(2, 0).unwrap().flags,
            PCI_DEVICE_FLAG_CLAIMABLE | PCI_DEVICE_FLAG_READ_ONLY
        );
        assert_eq!(table.query(2, 1).unwrap().flags, 0);
    }

    #[test]
    fn partition_policy_is_copied_into_the_guest_device_record() {
        let functions = [PciFunction {
            requester: 0x0070,
            class: 0x01,
            subclass: 0x04,
            ..PciFunction::default()
        }];
        let mut vmd = policy(0x0070, ManifestDeviceKind::Vmd);
        vmd.flags |= DEVICE_FLAG_PARTITIONED;
        vmd.storage_disk_guid = [1; 16];
        vmd.storage_partition_type_guid = [2; 16];
        vmd.storage_partition_guid = [3; 16];
        let table = DeviceTable::from_pci(&functions, None, &[vmd]).unwrap();
        let info = table.query(2, 0).unwrap();
        assert_ne!(info.flags & PCI_DEVICE_FLAG_PARTITIONED, 0);
        assert_eq!(info.storage_disk_guid, [1; 16]);
        assert_eq!(info.storage_partition_type_guid, [2; 16]);
        assert_eq!(info.storage_partition_guid, [3; 16]);
    }
}
