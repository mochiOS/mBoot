use core::ptr::write_volatile;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};
use uefi::table::boot::BootServices;

static ADDRESS: AtomicUsize = AtomicUsize::new(0);
static WIDTH: AtomicUsize = AtomicUsize::new(0);
static HEIGHT: AtomicUsize = AtomicUsize::new(0);
static STRIDE: AtomicUsize = AtomicUsize::new(0);
static ORDER: AtomicU8 = AtomicU8::new(0);

#[path = "console_font.rs"]
mod console_font;

pub fn text_console_begin() { fill(0); }

pub fn text_console_cell(column: usize, row: usize, byte: u8) {
    let scale = if WIDTH.load(Ordering::Relaxed) >= 1568 && HEIGHT.load(Ordering::Relaxed) >= 800 { 2 } else { 1 };
    let glyph = console_font::GLYPHS[(byte.clamp(b' ', b'~') - b' ') as usize];
    for (y, bits) in glyph.iter().enumerate() {
        for x in 0..8 {
            let color = if bits & (0x80 >> x) != 0 { 0x00cc_cccc } else { 0 };
            rectangle(8 + (column * 8 + x) * scale, 8 + (row * 16 + y) * scale, scale, scale, color);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FramebufferInfo {
    pub address: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
}

pub fn initialize(boot_services: &BootServices) -> bool {
    let Ok(handle) = boot_services.get_handle_for_protocol::<GraphicsOutput>() else {
        return false;
    };
    let Ok(mut output) = boot_services.open_protocol_exclusive::<GraphicsOutput>(handle) else {
        return false;
    };
    let info = output.current_mode_info();
    let order = match info.pixel_format() {
        PixelFormat::Rgb => 1,
        PixelFormat::Bgr => 2,
        PixelFormat::Bitmask | PixelFormat::BltOnly => return false,
    };
    let (width, height) = info.resolution();
    let stride = info.stride();
    let mut frame_buffer = output.frame_buffer();
    if frame_buffer.size() < stride.saturating_mul(height).saturating_mul(4) {
        return false;
    }
    ADDRESS.store(frame_buffer.as_mut_ptr() as usize, Ordering::Release);
    WIDTH.store(width, Ordering::Relaxed);
    HEIGHT.store(height, Ordering::Relaxed);
    STRIDE.store(stride, Ordering::Relaxed);
    ORDER.store(order, Ordering::Relaxed);
    true
}

/// Ends mBoot framebuffer output before the firmware display controller is
/// transferred to mDriver. Serial logging remains available after handoff.
pub fn handoff() {
    ADDRESS.store(0, Ordering::Release);
    WIDTH.store(0, Ordering::Relaxed);
    HEIGHT.store(0, Ordering::Relaxed);
    STRIDE.store(0, Ordering::Relaxed);
    ORDER.store(0, Ordering::Relaxed);
}

pub fn framebuffer_info() -> Option<FramebufferInfo> {
    let address = ADDRESS.load(Ordering::Acquire) as u64;
    let width = u32::try_from(WIDTH.load(Ordering::Relaxed)).ok()?;
    let height = u32::try_from(HEIGHT.load(Ordering::Relaxed)).ok()?;
    let stride = u32::try_from(STRIDE.load(Ordering::Relaxed)).ok()?;
    let format = u32::from(ORDER.load(Ordering::Relaxed));
    let size = u64::from(stride)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?;
    (address != 0 && size != 0).then_some(FramebufferInfo {
        address,
        size,
        width,
        height,
        stride,
        format,
    })
}

/// Copies a bounded, tightly packed pixel rectangle into the firmware scanout.
/// The caller validates that `pixels` belongs to the stopped System Domain.
pub fn present_firmware_frame(
    info: FramebufferInfo,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> bool {
    let Some(right) = x.checked_add(width) else {
        return false;
    };
    let Some(bottom) = y.checked_add(height) else {
        return false;
    };
    let Some(row_bytes) = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(4))
    else {
        return false;
    };
    let Some(expected) = row_bytes.checked_mul(height as usize) else {
        return false;
    };
    if width == 0
        || height == 0
        || right > info.width
        || bottom > info.height
        || pixels.len() != expected
    {
        return false;
    }
    let framebuffer = info.address as *mut u8;
    let destination_row = info.stride as usize * 4;
    for row in 0..height as usize {
        let destination = (y as usize + row) * destination_row + x as usize * 4;
        let source = row * row_bytes;
        for column in 0..width as usize {
            let source = source + column * 4;
            let destination = destination + column * 4;
            unsafe {
                if info.format == 2 {
                    framebuffer
                        .add(destination)
                        .cast::<u32>()
                        .write_volatile(u32::from_ne_bytes([
                            pixels[source],
                            pixels[source + 1],
                            pixels[source + 2],
                            0,
                        ]));
                } else {
                    framebuffer.add(destination).write_volatile(pixels[source + 2]);
                    framebuffer
                        .add(destination + 1)
                        .write_volatile(pixels[source + 1]);
                    framebuffer
                        .add(destination + 2)
                        .write_volatile(pixels[source]);
                    framebuffer.add(destination + 3).write_volatile(0);
                }
            }
        }
    }
    true
}

pub fn gpu_dma_transition(_requester: u16, _stage: &[u8]) {}

pub fn iommu_register_failure(requester: u16, status: u32, fault: u32, root: u64) {
    iommu_register_report(requester, b"ENABLE ERROR", status, fault, root, 0x006B_2028);
}

pub fn iommu_register_state(_requester: u16, _status: u32, _fault: u32, _root: u64) {}

pub fn iommu_initialization_failure(detail: &[u8]) {
    show(0x006B_2028, b"MBOOT", b"IOMMU ERROR");
    draw_centered(detail, line_y(2));
}

fn iommu_register_report(
    requester: u16,
    title: &[u8],
    status: u32,
    fault: u32,
    root: u64,
    color: u32,
) {
    show(color, b"IOMMU", title);
    draw_hex(u64::from(requester), line_y(2));
    let mut status_line = *b"GSTS 00000000";
    write_hex_u32(&mut status_line[5..13], status);
    draw_centered(&status_line, line_y(3));
    let mut fault_line = *b"FSTS 00000000";
    write_hex_u32(&mut fault_line[5..13], fault);
    draw_centered(&fault_line, line_y(4));
    let mut root_line = *b"RTLO 00000000";
    write_hex_u32(&mut root_line[5..13], root as u32);
    draw_centered(&root_line, line_y(5));
}

pub fn domain_crash(domain_id: u32, raw_reason: u64, address: u64, info: u64) {
    show(0x006B_2028, b"DOMAIN", b"CRASH");
    let mut detail = *b"00 0000";
    write_hex_u8(&mut detail[0..2], domain_id as u8);
    write_hex_u16(&mut detail[3..7], raw_reason as u16);
    draw_centered(&detail, line_y(2));
    let mut location = *b"ADDR 00000000";
    write_hex_u32(&mut location[5..13], address as u32);
    draw_centered(&location, line_y(3));
    if raw_reason == 2 {
        let mut cr0 = *b"CR0  00000000";
        write_hex_u32(&mut cr0[5..13], (info >> 32) as u32);
        draw_centered(&cr0, line_y(4));
        let mut cr3 = *b"CR3  00000000";
        write_hex_u32(&mut cr3[5..13], info as u32);
        draw_centered(&cr3, line_y(5));
    } else {
        let mut context = *b"INFO 00000000";
        write_hex_u32(&mut context[5..13], info as u32);
        draw_centered(&context, line_y(4));
    }
}

pub fn backend(_intel: bool) {}

pub fn bootstrap_success() {}

pub fn isolation_success() {}

pub fn mochios_ready() {}

pub fn hardware_ready() {}

pub fn mdriver_query(_index: u16, _requester: Option<u16>) {}

pub fn mdriver_claim(_requester: u16) {}

pub fn mdriver_claim_failure(requester: u16, stage: &[u8]) {
    show(0x006B_2028, b"MDRIVER", stage);
    draw_hex(u64::from(requester), line_y(2));
}

pub fn mdriver_bar_value_failure(requester: u16, failure: crate::pci::BarProbeFailure) {
    show(0x006B_2028, b"MDRIVER", b"BAR VALUE ERROR");
    draw_hex(u64::from(requester), line_y(2));
    let mut bar = *b"BAR 00 REASON 00";
    write_hex_u8(&mut bar[4..6], failure.index);
    write_hex_u8(&mut bar[14..16], failure.reason);
    draw_centered(&bar, line_y(3));
    let mut value = *b"VALUE 00000000 00000000";
    write_hex_u32(&mut value[6..14], failure.high);
    write_hex_u32(&mut value[15..23], failure.low);
    draw_centered(&value, line_y(4));
    let mut mask = *b"MASK  00000000 00000000";
    write_hex_u32(&mut mask[6..14], failure.mask_high);
    write_hex_u32(&mut mask[15..23], failure.mask_low);
    draw_centered(&mask, line_y(5));
}

pub fn mdriver_dma_map_failure(requester: u16, detail: &[u8]) {
    show(0x006B_2028, b"MDRIVER", b"DMA MAP ERROR");
    draw_hex(u64::from(requester), line_y(2));
    draw_centered(detail, line_y(3));
}

pub fn failure(code: u8) {
    show(0x006B_2028, b"MBOOT", b"ERROR");
    let digits = [b'0' + (code / 10) % 10, b'0' + code % 10];
    draw_centered(&digits, line_y(2));
}

pub fn pci_dma_failure(requester: u16) {
    show(0x006B_2028, b"MBOOT", b"PCI DMA");
    draw_hex(u64::from(requester), line_y(2));
}

pub fn nvme_candidates(first: (u16, u16, u16), second: (u16, u16, u16)) {
    show(0x006B_2028, b"NVME", b"");
    draw_pci_identity(first, line_y(1));
    draw_pci_identity(second, line_y(2));
}

pub fn storage_candidates(
    first: Option<(u16, u16, u16, u8)>,
    second: Option<(u16, u16, u16, u8)>,
) {
    show(0x006B_2028, b"STORAGE", b"");
    let Some((requester, vendor, device, subclass)) = first else {
        draw_centered(b"NONE", line_y(1));
        return;
    };
    draw_pci_identity((requester, vendor, device), line_y(1));
    if let Some((requester, vendor, device, _)) = second {
        draw_pci_identity((requester, vendor, device), line_y(2));
    } else {
        let mut text = *b"CLASS 0100";
        write_hex_u8(&mut text[8..10], subclass);
        draw_centered(&text, line_y(2));
    }
}

pub fn console_page(message: &[u8]) -> bool {
    if ADDRESS.load(Ordering::Acquire) == 0 || message.is_empty() || message.len() > 4096 {
        return false;
    }
    // Boot services have ended: use the same fixed-size text style as printk,
    // not the old centered, green diagnostic page.
    let mut log = mboot::boot_log::BootLog::new();
    log.append(message);
    log.render_console(text_console_begin, text_console_cell);
    core::sync::atomic::fence(Ordering::SeqCst);
    true
}

fn draw_pci_identity((requester, vendor, device): (u16, u16, u16), y: usize) {
    let mut text = [b' '; 14];
    write_hex_u16(&mut text[0..4], requester);
    write_hex_u16(&mut text[5..9], vendor);
    write_hex_u16(&mut text[10..14], device);
    draw_centered(&text, y);
}

fn write_hex_u16(output: &mut [u8], value: u16) {
    for (index, digit) in output.iter_mut().enumerate() {
        let nibble = ((value >> ((3 - index) * 4)) & 0xf) as u8;
        *digit = if nibble < 10 {
            b'0' + nibble
        } else {
            b'A' + nibble - 10
        };
    }
}

fn write_hex_u8(output: &mut [u8], value: u8) {
    for (index, digit) in output.iter_mut().enumerate() {
        let nibble = (value >> ((1 - index) * 4)) & 0xf;
        *digit = if nibble < 10 {
            b'0' + nibble
        } else {
            b'A' + nibble - 10
        };
    }
}

fn write_hex_u32(output: &mut [u8], value: u32) {
    for (index, digit) in output.iter_mut().enumerate() {
        let nibble = ((value >> ((7 - index) * 4)) & 0xf) as u8;
        *digit = if nibble < 10 {
            b'0' + nibble
        } else {
            b'A' + nibble - 10
        };
    }
}

pub fn vmcs_failure(field: u64) {
    show(0x006B_2028, b"MBOOT", b"VMCS");
    draw_hex(field, line_y(2));
}

pub fn vm_entry_failure(instruction_error: u64) {
    show(0x006B_2028, b"MBOOT", b"VMX");
    draw_hex(instruction_error, line_y(2));
}

fn draw_hex(value: u64, y: usize) {
    let mut digits = [b'0'; 4];
    for (index, digit) in digits.iter_mut().enumerate() {
        let shift = (3 - index) * 4;
        let nibble = ((value >> shift) & 0xf) as u8;
        *digit = if nibble < 10 {
            b'0' + nibble
        } else {
            b'A' + nibble - 10
        };
    }
    draw_centered(&digits, y);
}

fn show(background: u32, title: &[u8], status: &[u8]) {
    if ADDRESS.load(Ordering::Acquire) == 0 {
        return;
    }
    fill(background);
    draw_centered(title, line_y(0));
    draw_centered(status, line_y(1));
}

fn fill(color: u32) {
    let address = ADDRESS.load(Ordering::Acquire);
    let width = WIDTH.load(Ordering::Relaxed);
    let height = HEIGHT.load(Ordering::Relaxed);
    let stride = STRIDE.load(Ordering::Relaxed);
    let pixel = native_color(color);
    for y in 0..height {
        for x in 0..width {
            // SAFETY: `initialize` checked the framebuffer extent. The GOP
            // framebuffer remains mapped after ExitBootServices.
            unsafe { write_volatile((address as *mut u32).add(y * stride + x), pixel) };
        }
    }
}

fn draw_centered(text: &[u8], y: usize) {
    let width = WIDTH.load(Ordering::Relaxed);
    let scale = if width >= 800 { 8 } else { 4 };
    let text_width = text.len().saturating_mul(6 * scale).saturating_sub(scale);
    let x = width.saturating_sub(text_width) / 2;
    draw_text(text, x, y, scale);
}

fn draw_text(text: &[u8], mut x: usize, y: usize, scale: usize) {
    for &character in text {
        let glyph = glyph(character);
        for (row, bits) in glyph.iter().copied().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                rectangle(
                    x + column * scale,
                    y + row * scale,
                    scale,
                    scale,
                    0x00F4_F6FA,
                );
            }
        }
        x += 6 * scale;
    }
}

fn rectangle(x: usize, y: usize, width: usize, height: usize, color: u32) {
    let address = ADDRESS.load(Ordering::Acquire);
    let screen_width = WIDTH.load(Ordering::Relaxed);
    let screen_height = HEIGHT.load(Ordering::Relaxed);
    let stride = STRIDE.load(Ordering::Relaxed);
    let pixel = native_color(color);
    for py in y..y.saturating_add(height).min(screen_height) {
        for px in x..x.saturating_add(width).min(screen_width) {
            // SAFETY: Both axes are clipped to the framebuffer dimensions.
            unsafe { write_volatile((address as *mut u32).add(py * stride + px), pixel) };
        }
    }
}

fn native_color(rgb: u32) -> u32 {
    if ORDER.load(Ordering::Relaxed) == 1 {
        let red = (rgb >> 16) & 0xff;
        let green = rgb & 0x00ff00;
        let blue = rgb & 0xff;
        red | green | (blue << 16)
    } else {
        rgb
    }
}

fn line_y(line: usize) -> usize {
    let height = HEIGHT.load(Ordering::Relaxed);
    let scale = if WIDTH.load(Ordering::Relaxed) >= 800 {
        8
    } else {
        4
    };
    let total_height = 3 * 7 * scale + 2 * 3 * scale;
    height.saturating_sub(total_height) / 2 + line * 10 * scale
}

#[rustfmt::skip]
fn glyph(character: u8) -> [u8; 7] {
    // Reports retain their original case in the log. The boot font uses
    // capitals so lowercase PCI aliases and errno labels remain readable.
    match character.to_ascii_uppercase() {
        b'0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        b'1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        b'2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        b'3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        b'4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        b'5' => [0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110],
        b'6' => [0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        b'7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        b'8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        b'9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110],
        b'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        b'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        b'C' => [0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111],
        b'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        b'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        b'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        b'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110],
        b'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        b'I' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111],
        b'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100],
        b'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        b'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        b'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        b'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        b'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        b'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        b'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        b'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        b'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        b'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        b'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        b'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        b'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010],
        b'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        b'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        b'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        b'=' => [0, 0, 0b11111, 0, 0b11111, 0, 0],
        b':' => [0, 0b00100, 0b00100, 0, 0b00100, 0b00100, 0],
        b'.' => [0, 0, 0, 0, 0, 0b00100, 0b00100],
        b'_' => [0, 0, 0, 0, 0, 0, 0b11111],
        b'/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
        b'(' => [0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010],
        b')' => [0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000],
        b'>' => [0, 0b10000, 0b01000, 0b00100, 0b01000, 0b10000, 0],
        b'-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        _ => [0; 7],
    }
}
