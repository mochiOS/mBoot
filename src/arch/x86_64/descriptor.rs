use core::arch::{asm, global_asm};
use core::mem::size_of;
use core::ptr::{addr_of, addr_of_mut};

const KERNEL_CODE_SELECTOR: u16 = 0x08;
const KERNEL_DATA_SELECTOR: u16 = 0x10;
const TSS_SELECTOR: u16 = 0x18;
const INTERRUPT_GATE: u8 = 0x8e;

/// Each host CPU owns a permanent instance; LTR marks its GDT's TSS busy.
pub struct HostTables {
    gdt: [u64; 5],
    idt: [IdtEntry; 256],
    tss: [u8; 104],
}

impl HostTables {
    pub const fn new() -> Self {
        Self {
            gdt: [0, 0x00af_9a00_0000_ffff, 0x00af_9200_0000_ffff, 0, 0],
            idt: [IdtEntry::missing(); 256],
            tss: [0; 104],
        }
    }
}

global_asm!(
    ".global mboot_exception_stub",
    "mboot_exception_stub:",
    "cli",
    "2:",
    "hlt",
    "jmp 2b",
);

global_asm!(
    ".global mboot_device_interrupt_stub",
    "mboot_device_interrupt_stub:",
    "push rax",
    "push rcx",
    "push rdx",
    "push rsi",
    "push rdi",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "mov r11, rsp",
    "sub rsp, 528",
    "and rsp, -16",
    "mov [rsp + 512], r11",
    "fxsave64 [rsp]",
    "call mboot_device_interrupt",
    "fxrstor64 [rsp]",
    "mov rsp, [rsp + 512]",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop rcx",
    "pop rax",
    "iretq",
);

global_asm!(
    ".global mboot_timer_stub",
    "mboot_timer_stub:",
    "push rax",
    "push rcx",
    "push rdx",
    "push rsi",
    "push rdi",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "mov r11, rsp",
    "sub rsp, 528",
    "and rsp, -16",
    "mov [rsp + 512], r11",
    "fxsave64 [rsp]",
    "call mboot_timer_interrupt",
    "fxrstor64 [rsp]",
    "mov rsp, [rsp + 512]",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop rcx",
    "pop rax",
    "iretq",
);

unsafe extern "C" {
    fn mboot_exception_stub();
    fn mboot_timer_stub();
    fn mboot_device_interrupt_stub();
}

#[unsafe(no_mangle)]
extern "C" fn mboot_timer_interrupt() {
    super::timer::acknowledge();
}

#[unsafe(no_mangle)]
extern "C" fn mboot_device_interrupt() {
    crate::pci::acknowledge_device_interrupt();
}

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    offset_middle: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            attributes: 0,
            offset_middle: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    fn interrupt(handler: u64) -> Self {
        Self {
            offset_low: handler as u16,
            selector: KERNEL_CODE_SELECTOR,
            ist: 0,
            attributes: INTERRUPT_GATE,
            offset_middle: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

/// Installs mBoot-owned descriptor tables after UEFI boot services are gone.
///
/// # Safety
/// This must run at CPL0 with interrupts disabled. The caller must not rely on
/// firmware interrupt handlers after this function returns. Each CPU must use
/// its own tables; the installed instance must never be moved or reclaimed.
pub unsafe fn install(tables: &'static mut HostTables) {
    let handler = mboot_exception_stub as *const () as usize as u64;
    let idt_ptr = addr_of_mut!(tables.idt).cast::<IdtEntry>();
    for index in 0..256 {
        // SAFETY: IDT is exclusively initialized here before it is loaded.
        unsafe { idt_ptr.add(index).write(IdtEntry::interrupt(handler)) };
    }
    unsafe {
        idt_ptr
            .add(super::timer::VECTOR as usize)
            .write(IdtEntry::interrupt(
                mboot_timer_stub as *const () as usize as u64,
            ));
    }
    for vector in crate::pci::DEVICE_VECTOR_FIRST..=crate::pci::DEVICE_VECTOR_LAST {
        unsafe {
            idt_ptr.add(vector as usize).write(IdtEntry::interrupt(
                mboot_device_interrupt_stub as *const () as usize as u64,
            ))
        };
    }

    let tss_base = addr_of!(tables.tss) as u64;
    let tss_limit = (size_of::<[u8; 104]>() - 1) as u64;
    let tss_low = (tss_limit & 0xffff)
        | ((tss_base & 0x00ff_ffff) << 16)
        | (0x89 << 40)
        | (((tss_limit >> 16) & 0xf) << 48)
        | (((tss_base >> 24) & 0xff) << 56);
    let gdt_ptr = addr_of_mut!(tables.gdt).cast::<u64>();
    let tss_ptr = addr_of_mut!(tables.tss).cast::<u8>();
    // SAFETY: The GDT and TSS are exclusively initialized before LGDT/LTR.
    unsafe {
        gdt_ptr.add(3).write(tss_low);
        gdt_ptr.add(4).write(tss_base >> 32);
        tss_ptr
            .add(102)
            .cast::<u16>()
            .write_unaligned((size_of::<[u8; 104]>()) as u16);
    }

    let gdt_pointer = DescriptorTablePointer {
        limit: (size_of::<[u64; 5]>() - 1) as u16,
        base: addr_of!(tables.gdt) as u64,
    };
    let idt_pointer = DescriptorTablePointer {
        limit: (size_of::<[IdtEntry; 256]>() - 1) as u16,
        base: addr_of!(tables.idt) as u64,
    };

    // SAFETY: The contract requires CPL0 and disabled interrupts. Both tables
    // are static, fully initialized, and remain alive after they are loaded.
    unsafe {
        asm!("lgdt [{}]", in(reg) &gdt_pointer, options(readonly, nostack, preserves_flags));
        asm!(
            "mov ds, {data:x}",
            "mov es, {data:x}",
            "mov ss, {data:x}",
            "push {code}",
            "lea rax, [rip + 2f]",
            "push rax",
            "retfq",
            "2:",
            data = in(reg) KERNEL_DATA_SELECTOR,
            code = const KERNEL_CODE_SELECTOR,
            out("rax") _,
        );
        asm!("lidt [{}]", in(reg) &idt_pointer, options(readonly, nostack, preserves_flags));
        asm!("ltr {selector:x}", selector = in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
    }
}

/// Reads the TSS selected on this CPU, not another CPU's bootstrap TSS.
///
/// # Safety
/// The current GDT must be mapped and contain a live 64-bit TSS descriptor.
pub(crate) unsafe fn tss_base() -> Result<u64, crate::Error> {
    let mut gdt = DescriptorTablePointer { limit: 0, base: 0 };
    let selector: u16;
    unsafe {
        asm!("sgdt [{}]", in(reg) &mut gdt, options(nostack, preserves_flags));
        asm!("str {0:x}", out(reg) selector, options(nomem, nostack, preserves_flags));
    }
    let offset = usize::from(selector & !7);
    if selector & 4 != 0 || offset == 0 || offset + 15 > usize::from(gdt.limit) {
        return Err(crate::Error::InvalidState);
    }
    let descriptor = (gdt.base as usize + offset) as *const u64;
    let (low, high) = unsafe {
        (descriptor.read_unaligned(), descriptor.add(1).read_unaligned())
    };
    decode_tss_base(low, high)
}

fn decode_tss_base(low: u64, high: u64) -> Result<u64, crate::Error> {
    if (low >> 40) & 0xff != 0x8b || high >> 32 != 0 {
        return Err(crate::Error::InvalidState);
    }
    Ok(((low >> 16) & 0xff_ffff) | ((low >> 32) & 0xff00_0000) | (high << 32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_tss_preserves_all_address_bits() {
        // Busy, present 64-bit TSS at 0x1234_5678_9abc_def0.
        assert_eq!(decode_tss_base(0x9a00_8bbc_def0_0067, 0x1234_5678),
            Ok(0x1234_5678_9abc_def0));
    }

    #[test]
    fn rejects_non_tss_and_reserved_high_bits() {
        assert!(decode_tss_base(0x00af_9a00_0000_ffff, 0).is_err());
        assert!(decode_tss_base(0x0000_8b00_0000_0067, 1 << 32).is_err());
    }
}
