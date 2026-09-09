//! Physical host AP startup. Resources remain owned by mBoot for its lifetime.
use alloc::{boxed::Box, vec::Vec};
use core::{
    arch::{asm, global_asm},
    cell::UnsafeCell,
    sync::atomic::{AtomicU32, Ordering},
};
use mboot::arch::x86_64::{descriptor, timer};
use uefi::{
    proto::pi::mp::MpServices,
    table::boot::{AllocateType, BootServices, MemoryType},
    Status,
};

const STACK_PAGES: usize = 16;
const PARAMS: usize = 0x800;

// Separate from the guest trampoline: this restores the host paging features
// and EFER before entering Rust. EBX holds the SIPI page's physical base.
global_asm!(
    r#"
    .section .text
    .global mboot_ap_start
    .global mboot_ap_end
    .global mboot_ap_pm
    .global mboot_ap_lm
mboot_ap_start:
    .code16
    cli
    cld
    mov ax, cs
    mov ds, ax
    xor ebx, ebx
    mov bx, ax
    shl ebx, 4
    lgdt [0x800]
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    .byte 0x66, 0xff, 0x2e
    .word 0x806
    .code32
mboot_ap_pm:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov eax, [ebx + 0x830]
    mov cr4, eax
    mov eax, [ebx + 0x828]
    mov cr3, eax
    mov ecx, 0xc0000080
    mov eax, [ebx + 0x838]
    xor edx, edx
    wrmsr
    mov eax, [ebx + 0x820]
    mov cr0, eax
    // m16:32 indirect far jump through the relocated long-mode pointer.
    .byte 0xff, 0xab
    .long 0x80c
    .code64
mboot_ap_lm:
    fninit
    ldmxcsr [rbx + 0x812]
    mov rsp, [rbx + 0x840]
    sub rsp, 8
    mov rdi, [rbx + 0x848]
    mov rax, [rbx + 0x818]
    jmp rax
mboot_ap_end:
"#
);

unsafe extern "C" {
    static mboot_ap_start: u8;
    static mboot_ap_end: u8;
    static mboot_ap_pm: u8;
    static mboot_ap_lm: u8;
}

struct Ap {
    id: u32,
    page: u64,
    stack_top: u64,
    tables: *mut descriptor::HostTables,
    resources: mboot::VirtualizationResources,
    // Accessed only on this AP. Backend state must never migrate to the BSP.
    virtualization: UnsafeCell<Option<mboot::Virtualization>>,
    online: AtomicU32,
    work_state: AtomicU32,
    work: UnsafeCell<Option<Work>>,
    tsc_hz: u64,
}

const IDLE: u32 = 0;
const REQUESTED: u32 = 1;
const COMPLETE: u32 = 2;
const RUNNING: u32 = 3;
const RESERVED: u32 = 4;

#[derive(Clone, Copy)]
struct Work {
    entry: unsafe fn(usize),
    argument: usize,
}

// SAFETY: CPU tables/backend are accessed only by their owning AP. Work is
// handed off with work_state Release/Acquire transitions; no shared mutable
// reference escapes that protocol. All resources have permanent addresses.
unsafe impl Sync for Ap {}

impl Ap {
    /// # Safety
    /// Called from the BSP at CPL0 with interrupts disabled, outside locks used
    /// by interrupt handlers. The callback must return and cannot call execute.
    unsafe fn execute<F: FnOnce() -> R + Send, R: Send>(
        &self,
        action: F,
    ) -> Result<R, mboot::Error> {
        // SAFETY: The same ownership contract applies, with no companion work.
        unsafe { self.execute_with(action, || ()) }.map(|(result, ())| result)
    }

    /// Like execute, but runs independent BSP work before joining the AP.
    /// Both callbacks must return without unwinding; the AP is joined even if
    /// the companion returns an error. Neither may access the other's state.
    unsafe fn execute_with<F: FnOnce() -> R + Send, R: Send, S>(
        &self,
        action: F,
        companion: impl FnOnce() -> S,
    ) -> Result<(R, S), mboot::Error> {
        struct Job<F, R> {
            action: Option<F>,
            result: Option<R>,
        }
        unsafe fn invoke<F: FnOnce() -> R, R>(argument: usize) {
            // SAFETY: The BSP retains this job until COMPLETE is acquired.
            let job = unsafe { &mut *(argument as *mut Job<F, R>) };
            if let Some(action) = job.action.take() {
                job.result = Some(action());
            }
        }
        if self.online.load(Ordering::Acquire) != 1
            || self
                .work_state
                .compare_exchange(IDLE, RESERVED, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
        {
            return Err(mboot::Error::InvalidState);
        }
        let mut job = Job {
            action: Some(action),
            result: None,
        };
        // SAFETY: RESERVED gives the BSP exclusive access to the command slot.
        unsafe {
            *self.work.get() = Some(Work {
                entry: invoke::<F, R>,
                argument: &mut job as *mut Job<F, R> as usize,
            });
        }
        self.work_state.store(REQUESTED, Ordering::Release);
        // SAFETY: The destination was enumerated and booted by mBoot. The timer
        // vector's handler only acknowledges the local interrupt on this AP.
        if unsafe { send_ipi(rdmsr(0x1b), self.id, u32::from(timer::VECTOR), self.tsc_hz) }.is_err()
            && self
                .work_state
                .compare_exchange(REQUESTED, IDLE, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            return Err(mboot::Error::ControlInstructionFailed);
        }
        let companion_result = companion();
        while self.work_state.load(Ordering::Acquire) != COMPLETE {
            // SAFETY: The BSP owns its IDT. Allow physical device IRQs to be
            // recorded while the guest executes on its AP; return with IF clear.
            unsafe {
                asm!("sti; nop; cli", options(nomem, nostack));
            }
            core::hint::spin_loop();
        }
        self.work_state.store(IDLE, Ordering::Release);
        job.result
            .take()
            .map(|result| (result, companion_result))
            .ok_or(mboot::Error::InvalidState)
    }
}

pub struct HostCpus {
    aps: Vec<&'static Ap>,
    tsc_hz: u64,
}

/// Backend operations are dispatched to the CPU that enabled virtualization.
/// Guest memory and vCPU state remain exclusively borrowed until each command
/// completes. Paired entries join both CPUs before returning to host management.
pub struct PinnedVirtualization {
    backend: mboot::Virtualization,
    owner: Option<&'static Ap>,
}

macro_rules! backend_methods {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $result:ty;)*) => {$ (
        pub unsafe fn $name(&mut self, $($arg: $ty),*) -> Result<$result, mboot::Error> {
            // SAFETY: Caller maintains the original backend method's guest
            // state contract; dispatch guarantees execution on its owner CPU.
            unsafe { self.with_mut(|backend| backend.$name($($arg),*)) }
        }
    )*};
}

#[derive(Clone, Copy)]
pub enum ResumeKind {
    Hypercall,
    WithoutAdvance,
    Halted,
    MsrRead(u64),
    MsrWrite,
    Cpuid(mboot::CpuidResult),
    ControlRegisterWrite { register: u8, value: u64 },
    GeneralProtection,
}

pub struct Entry {
    pub start: Option<mboot::GuestConfig>,
    pub resume: ResumeKind,
    pub result: u64,
}

type ExitResult = Result<mboot::VmExit, mboot::Error>;

impl Entry {
    /// The backend is stopped, exclusively borrowed, and on its owning CPU.
    unsafe fn run(self, backend: &mut mboot::Virtualization) -> ExitResult {
        // SAFETY: The request completes this vCPU's previous exit, or starts it.
        let mut exit = unsafe {
            if let Some(config) = self.start {
                backend.run(config)
            } else {
                match self.resume {
                    ResumeKind::Hypercall => backend.resume(self.result),
                    ResumeKind::WithoutAdvance => backend.resume_preempted(),
                    ResumeKind::Halted => backend.resume_halted(),
                    ResumeKind::MsrRead(value) => backend.resume_msr_read(value),
                    ResumeKind::MsrWrite => backend.resume_msr_write(),
                    ResumeKind::Cpuid(result) => backend.resume_cpuid(result),
                    ResumeKind::ControlRegisterWrite { register, value } => {
                        backend.resume_control_register_write(register, value)
                    }
                    ResumeKind::GeneralProtection => {
                        backend.inject_general_protection()?;
                        backend.resume_preempted()
                    }
                }
            }
        }?;
        if exit.reason == mboot::VmExitReason::Preempted && exit.fault_info & (1 << 31) != 0 {
            mboot::pci::acknowledge_vmexit_interrupt(exit.fault_info as u8);
            exit.fault_info &= !(1 << 31);
        }
        Ok(exit)
    }
}

impl PinnedVirtualization {
    pub unsafe fn enable(resources: mboot::VirtualizationResources) -> Result<Self, mboot::Error> {
        // SAFETY: The caller supplies exclusive control pages on the BSP.
        unsafe { mboot::Virtualization::enable(resources) }.map(|backend| Self {
            backend,
            owner: None,
        })
    }

    pub fn kind(&self) -> mboot::BackendKind {
        self.backend.kind()
    }

    unsafe fn with_mut<R: Send>(
        &mut self,
        action: impl FnOnce(&mut mboot::Virtualization) -> Result<R, mboot::Error> + Send,
    ) -> Result<R, mboot::Error> {
        if let Some(ap) = self.owner {
            // SAFETY: Borrow stays live until the AP completes; the backend is
            // not accessible from the BSP or another vCPU during that time.
            unsafe { ap.execute(|| action(&mut self.backend)) }?
        } else {
            action(&mut self.backend)
        }
    }

    unsafe fn with_ref<R: Send>(
        &self,
        action: impl FnOnce(&mboot::Virtualization) -> Result<R, mboot::Error> + Send,
    ) -> Result<R, mboot::Error> {
        if let Some(ap) = self.owner {
            // SAFETY: Stopped backend state is read only on its owner CPU.
            unsafe { ap.execute(|| action(&self.backend)) }?
        } else {
            action(&self.backend)
        }
    }

    pub unsafe fn create_vcpu(&self, page: u64, asid: u32) -> Result<Self, mboot::Error> {
        // SAFETY: Caller reserves an exclusive page and a unique ASID.
        let backend = unsafe { self.with_ref(|backend| backend.create_vcpu(page, asid)) }?;
        Ok(Self {
            backend,
            owner: self.owner,
        })
    }

    pub fn owner(&self) -> Option<u32> {
        self.owner.map(|ap| ap.id)
    }

    /// All host mappings and resources touched by this guest remain stable.
    pub unsafe fn enter(&mut self, entry: Entry) -> ExitResult {
        unsafe { self.with_mut(|backend| entry.run(backend)) }
    }

    /// Runs two distinct vCPUs and joins both before returning. Host code must
    /// not change either guest's memory, mappings, or device ownership meanwhile.
    pub unsafe fn enter_pair(
        &mut self,
        entry: Entry,
        other: &mut Self,
        other_entry: Entry,
    ) -> Result<(ExitResult, ExitResult), mboot::Error> {
        if self.owner() == other.owner() {
            return Err(mboot::Error::InvalidState);
        }
        if let Some(ap) = self.owner {
            // SAFETY: Separate mutable borrows cover both stopped vCPUs. The
            // companion can dispatch to another AP, but never back to this AP.
            unsafe { ap.execute_with(|| entry.run(&mut self.backend), || other.enter(other_entry)) }
        } else {
            // Only the other vCPU can own an AP; keep return order unchanged.
            unsafe { other.enter_pair(other_entry, self, entry) }
                .map(|(second, first)| (first, second))
        }
    }
    backend_methods! {
        reset_vcpu() -> ();
        inject_interrupt(vector: u8) -> ();
        can_inject_interrupt() -> bool;
        set_interrupt_window(enabled: bool) -> ();
        flush_nested(root: u64) -> ();
        write_guest_msr(msr: u32, value: u64) -> ();
    }

    pub unsafe fn guest_instruction_pointer(&self) -> Result<u64, mboot::Error> {
        // SAFETY: Caller supplies a stopped vCPU.
        unsafe { self.with_ref(|backend| backend.guest_instruction_pointer()) }
    }
    pub unsafe fn guest_paging_state(&self) -> Result<(u64, u64), mboot::Error> {
        // SAFETY: Caller supplies a stopped vCPU.
        unsafe { self.with_ref(|backend| backend.guest_paging_state()) }
    }
    pub unsafe fn read_guest_msr(&self, msr: u32) -> Result<u64, mboot::Error> {
        // SAFETY: Caller supplies a stopped vCPU and supported MSR.
        unsafe { self.with_ref(|backend| backend.read_guest_msr(msr)) }
    }
}

impl HostCpus {
    /// # Safety
    /// Called on the BSP after start(), with an exclusive vCPU control page.
    /// Domain zero stays on the BSP; each subsequent Domain uses a separate AP
    /// when one exists. There is no live migration between host CPUs.
    pub unsafe fn create_vcpu(
        &self,
        index: usize,
        page: u64,
    ) -> Option<Result<PinnedVirtualization, mboot::Error>> {
        let ap = *self.aps.get(index.checked_sub(1)?)?;
        // SAFETY: execute borrows the callback until completion. The primary
        // backend is private to this AP, and each Domain gets a unique ASID.
        Some(
            unsafe {
                ap.execute(|| {
                    let backend = (&*ap.virtualization.get())
                        .as_ref()
                        .ok_or(mboot::Error::InvalidState)?;
                    backend.create_vcpu(page, index as u32 + 1)
                })
            }
            .and_then(|result| result)
            .map(|backend| PinnedVirtualization {
                backend,
                owner: Some(ap),
            }),
        )
    }
    pub fn prepare(bs: &BootServices) -> Result<Self, Status> {
        let mut aps = Vec::new();
        let handle = match bs.get_handle_for_protocol::<MpServices>() {
            Ok(handle) => handle,
            Err(error) if error.status() == Status::NOT_FOUND => {
                return Ok(Self { aps, tsc_hz: 0 })
            }
            Err(error) => return Err(error.status()),
        };
        let mp = bs
            .open_protocol_exclusive::<MpServices>(handle)
            .map_err(|e| e.status())?;
        let count = mp.get_number_of_processors().map_err(|e| e.status())?;
        let tsc_hz = timer::tsc_frequency_hz().unwrap_or_else(|| {
            let start = ticks();
            bs.stall(10_000);
            ticks().wrapping_sub(start).saturating_mul(100)
        });
        for index in 0..count.total {
            let info = mp.get_processor_info(index).map_err(|e| e.status())?;
            if info.is_bsp() || !info.is_enabled() || !info.is_healthy() {
                continue;
            }
            let id = u32::try_from(info.processor_id).map_err(|_| Status::UNSUPPORTED)?;
            let page = bs
                .allocate_pages(
                    AllocateType::MaxAddress(0xfffff),
                    MemoryType::LOADER_CODE,
                    1,
                )
                .map_err(|e| e.status())?;
            let stack = bs
                .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, STACK_PAGES)
                .map_err(|e| e.status())?;
            let control = bs
                .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, 2)
                .map_err(|e| e.status())?;
            let ap = Box::leak(Box::new(Ap {
                id,
                page,
                stack_top: stack + (STACK_PAGES * 4096) as u64,
                tables: Box::into_raw(Box::new(descriptor::HostTables::new())),
                resources: mboot::VirtualizationResources {
                    host_control_page: control,
                    vcpu_control_page: control + 4096,
                },
                virtualization: UnsafeCell::new(None),
                online: AtomicU32::new(0),
                work_state: AtomicU32::new(IDLE),
                work: UnsafeCell::new(None),
                tsc_hz,
            }));
            aps.push(&*ap);
        }
        Ok(Self { aps, tsc_hz })
    }

    /// # Safety
    /// Boot Services must have exited. The host mappings must identity-map the
    /// reserved pages and remain alive. APs must not already execute OS code.
    pub unsafe fn start(&self) -> Result<usize, Status> {
        if self.aps.is_empty() {
            return Ok(1);
        }
        let hz = self.tsc_hz;
        if hz == 0 {
            return Err(Status::UNSUPPORTED);
        }
        let (cr0, cr3, cr4): (u64, u64, u64);
        // SAFETY: Caller executes at host CPL0 after firmware teardown.
        let (apic, efer) = unsafe {
            asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack));
            asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
            asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
            (rdmsr(0x1b), rdmsr(0xc0000080))
        };
        // This bootstrap enters four-level paging through a 32-bit CR3 load.
        if cr3 > u32::MAX as u64 || cr4 & ((1 << 12) | (1 << 17)) != 0 || apic & (1 << 11) == 0 {
            return Err(Status::UNSUPPORTED);
        }
        let start = core::ptr::addr_of!(mboot_ap_start) as usize;
        let size = core::ptr::addr_of!(mboot_ap_end) as usize - start;
        if size > PARAMS {
            return Err(Status::BAD_BUFFER_SIZE);
        }
        for ap in &self.aps {
            if ap.online.load(Ordering::Acquire) != 0 {
                return Err(Status::ALREADY_STARTED);
            }
            if apic & (1 << 10) == 0 && ap.id > 255 {
                return Err(Status::UNSUPPORTED);
            }
            // SAFETY: Each AP has an exclusive reserved SIPI page, not yet used.
            let page = unsafe { core::slice::from_raw_parts_mut(ap.page as *mut u8, 4096) };
            page.fill(0);
            // SAFETY: The linked bootstrap is immutable and fits before PARAMS.
            page[..size]
                .copy_from_slice(unsafe { core::slice::from_raw_parts(start as *const u8, size) });
            page[0x800..0x802].copy_from_slice(&31u16.to_le_bytes());
            page[0x802..0x806].copy_from_slice(&((ap.page + 0x850) as u32).to_le_bytes());
            for (offset, symbol, selector) in [
                (0x806, core::ptr::addr_of!(mboot_ap_pm) as usize, 8u16),
                (0x80c, core::ptr::addr_of!(mboot_ap_lm) as usize, 24u16),
            ] {
                page[offset..offset + 4]
                    .copy_from_slice(&((ap.page as usize + symbol - start) as u32).to_le_bytes());
                page[offset + 4..offset + 6].copy_from_slice(&selector.to_le_bytes());
            }
            page[0x812..0x816].copy_from_slice(&0x1f80u32.to_le_bytes());
            for (offset, value) in [
                (0x818, ap_entry as *const () as u64),
                (0x820, (cr0 | (1 << 1)) & !((1 << 2) | (1 << 3))),
                (0x828, cr3),
                (0x830, (cr4 | (1 << 5) | (1 << 9) | (1 << 10)) & !(1 << 18)),
                (0x838, efer & !(1 << 10)),
                (0x840, ap.stack_top),
                (0x848, *ap as *const Ap as u64),
                (0x850, 0),
                (0x858, 0x00cf9a000000ffff),
                (0x860, 0x00cf92000000ffff),
                (0x868, 0x00af9a000000ffff),
            ] {
                page[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            }
            // SAFETY: Destination is a firmware-enumerated AP, never the BSP.
            unsafe {
                send_ipi(apic, ap.id, 0x4500, hz)?;
                delay(hz / 100);
                send_ipi(apic, ap.id, 0x600 | (ap.page >> 12) as u32, hz)?;
                delay(hz / 5000);
                if ap.online.load(Ordering::Acquire) == 0 {
                    send_ipi(apic, ap.id, 0x600 | (ap.page >> 12) as u32, hz)?;
                }
            }
            let deadline = ticks().wrapping_add(hz);
            while ap.online.load(Ordering::Acquire) == 0 {
                if ticks().wrapping_sub(deadline) as i64 >= 0 {
                    return Err(Status::TIMEOUT);
                }
                core::hint::spin_loop();
            }
            if ap.online.load(Ordering::Acquire) != 1 {
                return Err(Status::DEVICE_ERROR);
            }
        }
        Ok(self.aps.len() + 1)
    }
}

unsafe extern "sysv64" fn ap_entry(ap: *const Ap) -> ! {
    // SAFETY: Bootstrap passes its unique persistent context and private tables.
    unsafe {
        let ap = &*ap;
        descriptor::install(&mut *ap.tables);
        match mboot::Virtualization::enable(ap.resources) {
            Ok(backend) => {
                *ap.virtualization.get() = Some(backend);
                ap.online.store(1, Ordering::Release);
            }
            Err(_) => ap.online.store(2, Ordering::Release),
        }
        let _ = timer::initialize();
        loop {
            if ap
                .work_state
                .compare_exchange(REQUESTED, RUNNING, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                if let Some(work) = (*ap.work.get()).take() {
                    (work.entry)(work.argument);
                }
                ap.work_state.store(COMPLETE, Ordering::Release);
            } else {
                // IF stays clear from the state check through STI/HLT. A work
                // IPI arriving in that interval remains pending and wakes HLT.
                asm!("sti; hlt; cli", options(nomem, nostack));
            }
        }
    }
}

fn ticks() -> u64 {
    // SAFETY: RDTSC is available on the x86-64 host.
    unsafe { core::arch::x86_64::_rdtsc() }
}
fn delay(duration: u64) {
    let start = ticks();
    while ticks().wrapping_sub(start) < duration {
        core::hint::spin_loop();
    }
}
unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: Caller supplies a supported host MSR at CPL0.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    (u64::from(hi) << 32) | u64::from(lo)
}
unsafe fn send_ipi(apic: u64, id: u32, command: u32, hz: u64) -> Result<(), Status> {
    // SAFETY: Caller owns host LAPIC, validates destination and maps xAPIC MMIO.
    unsafe {
        if apic & (1 << 10) != 0 {
            asm!("wrmsr", in("ecx") 0x830u32, in("eax") command, in("edx") id, options(nostack));
        } else {
            let base = (apic & 0xfffff000) as *mut u32;
            let deadline = ticks().wrapping_add(hz / 100);
            while base.add(0x300 / 4).read_volatile() & (1 << 12) != 0 {
                if ticks().wrapping_sub(deadline) as i64 >= 0 {
                    return Err(Status::TIMEOUT);
                }
            }
            base.add(0x310 / 4).write_volatile(id << 24);
            base.add(0x300 / 4).write_volatile(command);
        }
    }
    Ok(())
}
