use core::arch::asm;
use core::fmt::{self, Write};

const COM1: u16 = 0x3f8;

pub fn init() {
    // SAFETY: mBoot owns the legacy COM1 port while running at CPL0.
    unsafe {
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x80);
        outb(COM1, 0x03);
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03);
        outb(COM1 + 2, 0xc7);
        outb(COM1 + 4, 0x0b);
    }
}

pub fn print(args: fmt::Arguments<'_>) {
    let _ = Serial.write_fmt(args);
}

struct Serial;

impl Write for Serial {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
        Ok(())
    }
}

impl Serial {
    fn write_byte(&mut self, byte: u8) {
        // SAFETY: mBoot owns COM1 and reads its line-status register at CPL0.
        while unsafe { inb(COM1 + 5) } & 0x20 == 0 {
            core::hint::spin_loop();
        }
        // SAFETY: mBoot owns the COM1 data register at CPL0.
        unsafe { outb(COM1, byte) };
    }
}

unsafe fn outb(port: u16, value: u8) {
    // SAFETY: The caller guarantees ownership of the specified I/O port and CPL0.
    unsafe { asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack)) };
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: The caller guarantees ownership of the specified I/O port and CPL0.
    unsafe { asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack)) };
    value
}
