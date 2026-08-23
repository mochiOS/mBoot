use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    crate::serial::print(format_args!("[mBoot-HV] panic: {info}\n"));
    // SAFETY: A panic is terminal and the UEFI entry point runs at CPL0.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
    loop {
        // SAFETY: Interrupts are disabled and halting is the terminal fallback.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}
