use mnu_abi::hypervisor::{
    PciDeviceInfo, PCI_DEVICE_STATE_FIRMWARE_DEFERRED, PCI_DEVICE_STATE_QUARANTINED,
};

use crate::pci::PciFunction;

const MAX_DEVICES: usize = 256;

const EMPTY_DEVICE: PciDeviceInfo = PciDeviceInfo {
    requester: 0,
    class: 0,
    subclass: 0,
    state: 0,
    _reserved0: [0; 3],
    owner_domain: 0,
    _reserved1: 0,
};

pub struct DeviceTable {
    devices: [PciDeviceInfo; MAX_DEVICES],
    count: usize,
}

impl DeviceTable {
    pub fn from_pci(functions: &[PciFunction], deferred_display: Option<u16>) -> Self {
        let mut table = Self {
            devices: [EMPTY_DEVICE; MAX_DEVICES],
            count: 0,
        };
        for function in functions.iter().take(MAX_DEVICES) {
            table.devices[table.count] = PciDeviceInfo {
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
                _reserved1: 0,
            };
            table.count += 1;
        }
        table
    }

    pub fn query(&self, index: usize) -> Option<PciDeviceInfo> {
        self.devices
            .get(index)
            .copied()
            .filter(PciDeviceInfo::validate)
    }

    pub const fn len(&self) -> usize {
        self.count
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_stays_firmware_owned_while_other_devices_are_quarantined() {
        let functions = [
            PciFunction {
                requester: 0x0010,
                class: 0x03,
                subclass: 0,
            },
            PciFunction {
                requester: 0x00a0,
                class: 0x0c,
                subclass: 0x03,
            },
        ];
        let table = DeviceTable::from_pci(&functions, Some(0x0010));
        assert_eq!(table.len(), 2);
        assert_eq!(
            table.query(0).unwrap().state,
            PCI_DEVICE_STATE_FIRMWARE_DEFERRED
        );
        assert_eq!(table.query(1).unwrap().state, PCI_DEVICE_STATE_QUARANTINED);
        assert_eq!(table.query(2), None);
    }
}
