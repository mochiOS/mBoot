//! Read-only reference-board firmware clock. The actual PL031 stays private
//! to EL2; neither Domain receives its registers, alarm IRQ or write authority.
use super::*;

pub(super) struct Realtime {
    base: u64,
}

impl Realtime {
    pub(super) fn prepare(tree: &fdt::Fdt<'_>) -> Option<Self> {
        let (mut compatible, mut primecell, mut enabled, mut region) = (false, false, true, None);
        let (mut address_cells, mut size_cells) = (0, 0);
        let mut found = None;
        for token in tree.tokens() {
            match token.expect("firmware RTC tree") {
                fdt::Token::BeginNode { depth: 1, .. } => {
                    (compatible, primecell, enabled, region) = (false, false, true, None);
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
                        compatible = value.split(|b| *b == 0).any(|s| s == b"arm,pl031");
                        primecell = value.split(|b| *b == 0).any(|s| s == b"arm,primecell");
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
                        primecell && address_cells == 2 && size_cells == 2 && found.is_none(),
                        "ambiguous reference RTC",
                    );
                    let (base, size) = region.expect("firmware RTC region");
                    check(
                        base == 0x0901_0000 && size == 4096,
                        "unsupported reference RTC binding",
                    );
                    found = Some(base);
                }
                _ => {}
            }
        }
        let base = found?;
        // SAFETY: This fixed reference frontend owns the admitted cold PL031
        // range. No source-supplied arbitrary address is dereferenced.
        let id = [0x31, 0x10, 0x14, 0, 0x0d, 0xf0, 0x05, 0xb1];
        for (i, expected) in id.iter().enumerate() {
            let actual = unsafe { ((base + 0xfe0 + 4 * i as u64) as *const u32).read_volatile() };
            check(actual == *expected, "firmware RTC identity");
        }
        let enabled = unsafe { ((base + 12) as *const u32).read_volatile() };
        check(enabled & 1 != 0, "firmware RTC stopped");
        emit("Actual reference firmware RTC admitted read-only at EL2\n");
        Some(Self { base })
    }

    pub(super) fn seconds(&self) -> u64 {
        // SAFETY: The owner retains the admitted, private, read-only controller.
        // PL031 DR is the actual UTC seconds counter. No fabricated epoch or
        // build timestamp is substituted if a platform has no clock source.
        u64::from(unsafe { (self.base as *const u32).read_volatile() })
    }
}
