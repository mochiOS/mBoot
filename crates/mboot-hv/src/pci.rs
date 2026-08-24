use core::arch::asm;

const CONFIG_ADDRESS: u16 = 0x0cf8;
const CONFIG_DATA: u16 = 0x0cfc;
const COMMAND_BUS_MASTER: u16 = 1 << 2;
const MAX_ACTIVE_REQUESTERS: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QuarantineReport {
    pub functions: u32,
    pub bus_masters_disabled: u32,
    pub bus_masters_active: u32,
    pub first_active_requester: Option<u16>,
    active_requesters: [u16; MAX_ACTIVE_REQUESTERS],
    active_requester_count: usize,
}

impl QuarantineReport {
    pub fn active_requesters(&self) -> &[u16] {
        &self.active_requesters[..self.active_requester_count]
    }

    pub const fn recorded_every_active_requester(&self) -> bool {
        self.active_requester_count as u32 == self.bus_masters_active
    }
}

/// Stops PCI functions in segment zero from initiating new DMA transactions.
///
/// # Safety
/// The caller must execute at CPL0 after firmware device I/O has finished. No
/// other agent may access PCI configuration mechanism 1 concurrently.
pub unsafe fn quarantine_segment_zero() -> QuarantineReport {
    let mut report = QuarantineReport::default();
    let mut queue = [0_u8; 256];
    let mut queued = [false; 256];
    let mut head = 0;
    let mut tail = 1;
    queued[0] = true;
    while head < tail {
        let bus = queue[head];
        head += 1;
        // SAFETY: The public function contract gives this loop exclusive access
        // to PCI configuration mechanism 1.
        unsafe { quarantine_bus(bus, &mut report, &mut queue, &mut queued, &mut tail) };
    }
    report
}

unsafe fn quarantine_bus(
    bus: u8,
    report: &mut QuarantineReport,
    queue: &mut [u8; 256],
    queued: &mut [bool; 256],
    tail: &mut usize,
) {
    for device in 0_u8..32 {
        // SAFETY: quarantine_segment_zero serialized configuration-space access.
        if unsafe { read_u16(bus, device, 0, 0) } == 0xffff {
            continue;
        }
        // SAFETY: Function zero exists and configuration-space access is serialized.
        let header_type = unsafe { read_u8(bus, device, 0, 0x0e) };
        let functions = if header_type & 0x80 != 0 { 8 } else { 1 };
        for function in 0_u8..functions {
            // SAFETY: Configuration-space access remains serialized.
            if unsafe { read_u16(bus, device, function, 0) } == 0xffff {
                continue;
            }
            // SAFETY: The vendor read proved that this function exists.
            if let Some(next_bus) = unsafe { quarantine_function(bus, device, function, report) } {
                enqueue_bus(next_bus, queue, queued, tail);
            }
        }
    }
}

unsafe fn quarantine_function(
    bus: u8,
    device: u8,
    function: u8,
    report: &mut QuarantineReport,
) -> Option<u8> {
    report.functions = report.functions.saturating_add(1);
    // SAFETY: The caller established exclusive configuration-space access and
    // verified this function exists.
    let class = unsafe { read_u8(bus, device, function, 0x0b) };
    // SAFETY: The same serialized, existing function is being read.
    let subclass = unsafe { read_u8(bus, device, function, 0x0a) };
    let next_bus = if class == 0x06 && subclass == 0x04 {
        // SAFETY: PCI-to-PCI bridge headers define byte 0x19 as Secondary Bus.
        Some(unsafe { read_u8(bus, device, function, 0x19) })
    } else if bus == 0 && class == 0x06 && subclass == 0x00 && function != 0 {
        Some(function)
    } else {
        None
    };
    // SAFETY: The command register exists for every PCI function.
    let command = unsafe { read_u16(bus, device, function, 4) };
    let quarantined = without_bus_master(command);
    if quarantined != command {
        // SAFETY: This writes only the command halfword and preserves all other bits.
        unsafe { write_u16(bus, device, function, 4, quarantined) };
        // SAFETY: Read-back verifies that the device accepted the quarantine.
        if unsafe { read_u16(bus, device, function, 4) } & COMMAND_BUS_MASTER == 0 {
            report.bus_masters_disabled = report.bus_masters_disabled.saturating_add(1);
        } else {
            report.bus_masters_active = report.bus_masters_active.saturating_add(1);
            if report.first_active_requester.is_none() {
                report.first_active_requester = Some(requester_id(bus, device, function));
            }
            if report.active_requester_count < report.active_requesters.len() {
                report.active_requesters[report.active_requester_count] =
                    requester_id(bus, device, function);
                report.active_requester_count += 1;
            }
        }
    }
    next_bus.filter(|bus| *bus != 0)
}

const fn requester_id(bus: u8, device: u8, function: u8) -> u16 {
    ((bus as u16) << 8) | ((device as u16) << 3) | function as u16
}

const fn without_bus_master(command: u16) -> u16 {
    command & !COMMAND_BUS_MASTER
}

fn enqueue_bus(bus: u8, queue: &mut [u8; 256], queued: &mut [bool; 256], tail: &mut usize) {
    if !queued[bus as usize] {
        queued[bus as usize] = true;
        queue[*tail] = bus;
        *tail += 1;
    }
}

const fn config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xfc)
}

unsafe fn select(bus: u8, device: u8, function: u8, offset: u8) {
    let address = config_address(bus, device, function, offset);
    // SAFETY: The caller owns PCI configuration ports and executes at CPL0.
    unsafe {
        asm!("out dx, eax", in("dx") CONFIG_ADDRESS, in("eax") address, options(nomem, nostack));
    }
}

unsafe fn read_u8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    // SAFETY: Forwarded from this function's serialized configuration access.
    unsafe { select(bus, device, function, offset) };
    let value: u8;
    let port = CONFIG_DATA + u16::from(offset & 3);
    // SAFETY: The selected PCI configuration byte is readable through this port.
    unsafe { asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack)) };
    value
}

unsafe fn read_u16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    // SAFETY: Forwarded from this function's serialized configuration access.
    unsafe { select(bus, device, function, offset) };
    let value: u16;
    let port = CONFIG_DATA + u16::from(offset & 2);
    // SAFETY: The selected PCI configuration halfword is readable through this port.
    unsafe { asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack)) };
    value
}

unsafe fn write_u16(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    // SAFETY: Forwarded from this function's serialized configuration access.
    unsafe { select(bus, device, function, offset) };
    let port = CONFIG_DATA + u16::from(offset & 2);
    // SAFETY: The selected PCI command halfword is writable through this port.
    unsafe { asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_address_keeps_bdf_and_aligned_offset() {
        assert_eq!(config_address(2, 3, 4, 0x0f), 0x8002_1c0c);
    }

    #[test]
    fn quarantine_changes_only_bus_master_enable() {
        assert_eq!(without_bus_master(0xffff), 0xfffb);
        assert_eq!(without_bus_master(0x0403), 0x0403);
    }

    #[test]
    fn requester_id_uses_the_pci_bdf_layout() {
        assert_eq!(requester_id(0x12, 0x1f, 7), 0x12ff);
    }
}
