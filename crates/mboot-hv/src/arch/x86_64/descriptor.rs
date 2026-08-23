use core::arch::{asm, global_asm};
use core::mem::size_of;
use core::ptr::{addr_of, addr_of_mut};

const KERNEL_CODE_SELECTOR: u16 = 0x08;
const KERNEL_DATA_SELECTOR: u16 = 0x10;
const INTERRUPT_GATE: u8 = 0x8e;

static mut GDT: [u64; 3] = [0, 0x00af_9a00_0000_ffff, 0x00af_9200_0000_ffff];
static mut IDT: [IdtEntry; 256] = [IdtEntry::missing(); 256];

global_asm!(
    ".global mboot_hv_exception_stub",
    "mboot_hv_exception_stub:",
    "cli",
    "2:",
    "hlt",
    "jmp 2b",
);

unsafe extern "C" {
    fn mboot_hv_exception_stub();
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
/// firmware interrupt handlers after this function returns.
pub unsafe fn install() {
    let handler = mboot_hv_exception_stub as *const () as usize as u64;
    let idt_ptr = addr_of_mut!(IDT).cast::<IdtEntry>();
    for index in 0..256 {
        // SAFETY: IDT is exclusively initialized here before it is loaded.
        unsafe { idt_ptr.add(index).write(IdtEntry::interrupt(handler)) };
    }

    let gdt_pointer = DescriptorTablePointer {
        limit: (size_of::<[u64; 3]>() - 1) as u16,
        base: addr_of!(GDT) as u64,
    };
    let idt_pointer = DescriptorTablePointer {
        limit: (size_of::<[IdtEntry; 256]>() - 1) as u16,
        base: addr_of!(IDT) as u64,
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
    }
}
