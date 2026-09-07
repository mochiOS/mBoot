use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    crate::serial::print(format_args!("[mBoot] panic: {info}\n"));
    crate::display::failure(99);
    // Formatting into fixed storage must not call the unavailable UEFI allocator.
    use core::fmt::Write;
    let mut report = mboot::boot_log::ProbeReport::new();
    let _ = write!(report, "MBOOT ERROR 99\n{info}");
    let _ = crate::display::console_page(report.text().as_bytes());
    // SAFETY: A panic is terminal and the UEFI entry point runs at CPL0.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
    loop {
        // SAFETY: Interrupts are disabled and halting is the terminal fallback.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}
