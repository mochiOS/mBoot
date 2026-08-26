use mnu_abi::hypervisor::{
    PciDeviceInfo, PCI_DEVICE_FLAG_CLAIMABLE, PCI_DEVICE_FLAG_EPHEMERAL, PCI_DEVICE_STATE_ACTIVE,
    PCI_DEVICE_STATE_CLAIMED_DISABLED, PCI_DEVICE_STATE_FIRMWARE_DEFERRED,
    PCI_DEVICE_STATE_QUARANTINED,
};

use crate::manifest::{ManifestDevice, ManifestDeviceKind};
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
            };
            table.count += 1;
        }
        for policy in policies {
            if policy.segment != 0 {
                return Err(DeviceError::InvalidPolicy);
            }
            let Some(record) = table.devices[..table.count]
                .iter_mut()
                .find(|record| record.info.requester == policy.requester)
            else {
                if policy.is_required() {
                    return Err(DeviceError::DeviceUnavailable);
                }
                continue;
            };
            if record.allowed_domain != 0 || !kind_matches(record.info, policy.kind) {
                return Err(DeviceError::InvalidPolicy);
            }
            record.allowed_domain = policy.domain_id;
            if policy.is_ephemeral() {
                record.info.flags |= PCI_DEVICE_FLAG_EPHEMERAL;
            }
        }
        Ok(table)
    }

    pub fn query(&self, domain_id: u32, index: usize) -> Option<PciDeviceInfo> {
        let record = self.devices.get(index).filter(|_| index < self.count)?;
        let mut info = record.info;
        if record.allowed_domain == domain_id && info.state == PCI_DEVICE_STATE_QUARANTINED {
            info.flags |= PCI_DEVICE_FLAG_CLAIMABLE;
        }
        Some(info).filter(PciDeviceInfo::validate)
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
        if record.info.state != PCI_DEVICE_STATE_QUARANTINED || record.info.owner_domain != 0 {
            return Err(DeviceError::InvalidState);
        }
        Ok(())
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
}

const fn kind_matches(info: PciDeviceInfo, kind: ManifestDeviceKind) -> bool {
    match kind {
        ManifestDeviceKind::Other => true,
        ManifestDeviceKind::Display => info.class == 0x03,
        ManifestDeviceKind::Block => info.class == 0x01,
        ManifestDeviceKind::Network => info.class == 0x02,
        ManifestDeviceKind::Usb => info.class == 0x0c && info.subclass == 0x03,
        ManifestDeviceKind::Audio => info.class == 0x04,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{DEVICE_FLAG_EPHEMERAL, DEVICE_FLAG_REQUIRED};

    fn policy(requester: u16, kind: ManifestDeviceKind) -> ManifestDevice {
        ManifestDevice {
            segment: 0,
            requester,
            kind,
            flags: DEVICE_FLAG_REQUIRED,
            domain_id: 2,
        }
    }

    #[test]
    fn claim_and_release_follow_the_signed_policy() {
        let functions = [PciFunction {
            requester: 0x00a0,
            class: 0x0c,
            subclass: 0x03,
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
    fn firmware_display_cannot_be_claimed() {
        let functions = [PciFunction {
            requester: 0x0010,
            class: 0x03,
            subclass: 0,
        }];
        let mut table = DeviceTable::from_pci(
            &functions,
            Some(0x0010),
            &[policy(0x0010, ManifestDeviceKind::Display)],
        )
        .unwrap();
        assert_eq!(table.claim(2, 0x0010), Err(DeviceError::InvalidState));
    }

    #[test]
    fn ephemeral_policy_survives_claim_and_activation() {
        let functions = [PciFunction {
            requester: 0x0018,
            class: 0x01,
            subclass: 0,
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
}
