//! Optional QEMU virt reference firmware, outside the CPU/kernel interfaces.
//! fw_cfg remains private to EL2. Only the actual scanout RAM is delegated;
//! neither Domain can program the firmware DMA engine.
//! Wire layout: https://www.qemu.org/docs/master/specs/fw_cfg.html
use super::*;
use image::ImageMemory;

pub(super) const WIDTH: u32 = 1280;
pub(super) const HEIGHT: u32 = 800;
pub(super) const BYTES: u64 = WIDTH as u64 * HEIGHT as u64 * 4;

pub(super) struct Framebuffer<'pool> {
    pub pixels: pages::Pages<'pool>,
    // Retain command storage as well as scanout until the reference VM ends.
    _command: pages::Pages<'pool>,
}

fn binding(tree: &fdt::Fdt<'_>) -> u64 {
    let (mut compatible, mut region, mut enabled) = (false, None, true);
    let mut found = None;
    let (mut address_cells, mut size_cells) = (0, 0);
    for token in tree.tokens() {
        match token.expect("firmware tree") {
            fdt::Token::BeginNode { depth: 1, .. } => {
                (compatible, region, enabled) = (false, None, true);
            }
            fdt::Token::Property {
                depth: 0,
                name,
                value,
            } => match name {
                "#address-cells" if value.len() == 4 => {
                    address_cells = u32::from_be_bytes(value.try_into().unwrap())
                }
                "#size-cells" if value.len() == 4 => {
                    size_cells = u32::from_be_bytes(value.try_into().unwrap())
                }
                _ => {}
            },
            fdt::Token::Property {
                depth: 1,
                name,
                value,
            } => match name {
                "compatible" => {
                    compatible = value.split(|b| *b == 0).any(|s| s == b"qemu,fw-cfg-mmio")
                }
                "status" => enabled = matches!(value, b"okay\0" | b"ok\0"),
                "reg" if value.len() == 16 => {
                    region = Some((
                        u64::from_be_bytes(value[..8].try_into().unwrap()),
                        u64::from_be_bytes(value[8..].try_into().unwrap()),
                    ))
                }
                _ => {}
            },
            fdt::Token::EndNode { depth: 1 } if compatible && enabled => {
                check(
                    address_cells == 2 && size_cells == 2,
                    "unsupported fw_cfg bus",
                );
                let (base, size) = region.expect("fw_cfg registers");
                // Admission is deliberately limited to this fixed virt-8.2
                // reference board, never an arbitrary firmware-supplied pointer.
                check(
                    base == 0x0902_0000 && size == 24 && found.is_none(),
                    "unexpected fw_cfg binding",
                );
                found = Some(base);
            }
            _ => {}
        }
    }
    found.expect("QEMU fw_cfg missing")
}

impl<'pool> Framebuffer<'pool> {
    pub(super) fn prepare(pool: &'pool pages::PagePool, tree: &fdt::Fdt<'_>) -> Self {
        let base = binding(tree);
        let control: u64;
        unsafe {
            asm!("mrs {}, sctlr_el2", out(reg) control, options(nomem, nostack));
        }
        check(control & 5 == 0, "fw_cfg fixture requires uncached EL2 RAM");
        // SAFETY: This explicit reference frontend owns the cold firmware
        // channel. It is absent from both Domain DTBs and Stage-2 MMIO maps.
        let select = |key: u16| unsafe {
            ((base + 8) as *mut u16).write_volatile(key.to_be());
        };
        let byte = || unsafe { (base as *const u8).read_volatile() };
        select(0);
        check(
            core::array::from_fn::<_, 4, _>(|_| byte()) == *b"QEMU",
            "fw_cfg signature",
        );
        select(1);
        let features = u32::from_le_bytes(core::array::from_fn(|_| byte()));
        check(features & 2 != 0, "fw_cfg DMA unavailable");
        select(0x19);
        let count = u32::from_be_bytes(core::array::from_fn(|_| byte()));
        check(count <= 4096, "fw_cfg directory limit");
        let mut selector = None;
        for _ in 0..count {
            let entry: [u8; 64] = core::array::from_fn(|_| byte());
            if entry[8..].split(|b| *b == 0).next() == Some(b"etc/ramfb") {
                check(
                    selector.is_none()
                        && entry[6..8] == [0, 0]
                        && u32::from_be_bytes(entry[..4].try_into().unwrap()) == 28,
                    "invalid RAM framebuffer firmware entry",
                );
                let key = u16::from_be_bytes(entry[4..6].try_into().unwrap());
                check(
                    (0x20..0x4000).contains(&key),
                    "invalid RAM framebuffer selector",
                );
                selector = Some(key);
            }
        }
        let key = selector.expect("actual ramfb device missing");
        let pixels = pool.allocate(BYTES / 4096, 4096).expect("scanout RAM");
        let command = pool.allocate(1, 4096).expect("firmware command RAM");
        let mut config = [0u8; 28];
        config[..8].copy_from_slice(&pixels.physical_start().to_be_bytes());
        config[8..12].copy_from_slice(&0x3432_5258u32.to_be_bytes()); // DRM XRGB8888
        config[16..20].copy_from_slice(&WIDTH.to_be_bytes());
        config[20..24].copy_from_slice(&HEIGHT.to_be_bytes());
        config[24..].copy_from_slice(&(WIDTH * 4).to_be_bytes());
        let mut request = [0u8; 16];
        request[..4].copy_from_slice(&((u32::from(key) << 16) | 8 | 16).to_be_bytes());
        request[4..8].copy_from_slice(&28u32.to_be_bytes());
        request[8..].copy_from_slice(&(command.physical_start() + 64).to_be_bytes());
        let address = command.host_address(0, 4096).unwrap();
        // EL2 caching is off in this fixture; publish owned, initialized DMA
        // bytes before the doorbell. No guest pointer reaches this engine.
        unsafe {
            core::ptr::copy_nonoverlapping(
                config.as_ptr(),
                (address + 64) as *mut u8,
                config.len(),
            );
            core::ptr::copy_nonoverlapping(request.as_ptr(), address as *mut u8, request.len());
            asm!("dsb sy", options(nostack));
            ((base + 16) as *mut u64).write_volatile(command.physical_start().to_be());
        }
        let mut complete = false;
        for _ in 0..100_000 {
            let status = u32::from_be(unsafe { (address as *const u32).read_volatile() });
            check(status & 1 == 0, "firmware framebuffer DMA failed");
            if status == 0 {
                complete = true;
                break;
            }
            core::hint::spin_loop();
        }
        check(complete, "firmware framebuffer DMA deadline");
        unsafe {
            asm!("dsb sy", options(nostack));
        }
        emit("Actual QEMU RAM framebuffer configured by private EL2 firmware channel\n");
        Self {
            pixels,
            _command: command,
        }
    }

    /// SAFETY: The source System vCPU is stopped; there are no peer CPUs or
    /// assigned DMA requesters. Source and scanout own disjoint retained RAM.
    pub(super) unsafe fn present(
        &mut self,
        source: &pages::Pages<'_>,
        position: u64,
        size: u64,
        address: u64,
    ) -> bool {
        let (x, y) = (position as u32, (position >> 32) as u32);
        let (width, height) = (size as u32, (size >> 32) as u32);
        let Some(right) = x.checked_add(width) else {
            return false;
        };
        let Some(bottom) = y.checked_add(height) else {
            return false;
        };
        let Some(length) = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|n| n.checked_mul(4))
        else {
            return false;
        };
        if width == 0
            || height == 0
            || right > WIDTH
            || bottom > HEIGHT
            || length > mnu_abi::hypervisor::FIRMWARE_FRAMEBUFFER_MAX_TRANSFER as u64
        {
            return false;
        }
        let Some(input) = source.guest_host_address(address, length) else {
            return false;
        };
        // EL1 may have produced the snapshot through a cacheable view. Clean
        // the owned source to PoC before this fixture's uncached EL2 read.
        let cache: u64;
        unsafe { asm!("mrs {}, ctr_el0", out(reg) cache, options(nomem, nostack)); }
        let line = 4u64 << ((cache >> 16) & 15);
        if line > 4096 { return false; }
        let Some(end) = input.checked_add(length - 1) else { return false; };
        let mut cursor = input & !(line - 1);
        let last = end & !(line - 1);
        loop {
            unsafe { asm!("dc cvac, {}", in(reg) cursor, options(nostack)); }
            if cursor == last { break; }
            cursor += line;
        }
        unsafe { asm!("dsb sy", options(nostack)); }
        let row_bytes = width as usize * 4;
        for row in 0..height as usize {
            let offset = ((y as u64 + row as u64) * WIDTH as u64 + x as u64) * 4;
            let output = self.pixels.host_address(offset, row_bytes as u64).unwrap();
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (input as *const u8).add(row * row_bytes),
                    output as *mut u8,
                    row_bytes,
                );
            }
        }
        true
    }
}
