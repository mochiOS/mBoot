//! Architected per-CPU timer wiring from the primary interrupt domain. This
//! metadata does not grant access to physical timer registers or PPIs.
use super::{gic, Fdt, Token};

#[derive(Clone, Copy, Debug)]
pub struct Timers {
    pub virtual_timer: gic::Ppi,
    pub hypervisor: gic::Ppi,
}

impl Timers {
    /// Baseline root-level ARMv8 timer binding. Unrecognized hierarchies,
    /// names, counter overrides and erratum requirements fail closed.
    pub fn from_fdt(tree: &Fdt<'_>, gic: &gic::GicV2) -> Result<Self, gic::Error> {
        let mut root_parent = None;
        let mut node = Properties::default();
        let mut result = None;
        for token in tree.tokens() {
            match token.map_err(gic::Error::Tree)? {
                Token::BeginNode { depth: 1, .. } => node = Properties::default(),
                Token::Property {
                    depth: 0,
                    name: "interrupt-parent",
                    value,
                } => {
                    root_parent = Some(value);
                }
                Token::Property {
                    depth: 1,
                    name,
                    value,
                } => node.set(name, value)?,
                Token::EndNode { depth: 1 } => {
                    if !node.compatible.is_some_and(|v| {
                        matches!(
                            v,
                            b"arm,armv8-timer\0" | b"arm,armv8-timer\0arm,armv7-timer\0"
                        )
                    }) {
                        continue;
                    }
                    if result.is_some() {
                        return Err(gic::Error::AmbiguousBinding);
                    }
                    if !matches!(node.status, None | Some(b"ok\0" | b"okay\0")) {
                        return Err(gic::Error::Disabled);
                    }
                    let parent = node
                        .parent
                        .or(root_parent)
                        .ok_or(gic::Error::MissingBinding)?;
                    if parent != gic.phandle.to_be_bytes() || node.unsupported {
                        return Err(gic::Error::UnsupportedController);
                    }
                    let (virt, hyp, count) = match node.names {
                        None | Some(b"sec-phys\0phys\0virt\0hyp-phys\0") => (2, 3, 4),
                        Some(b"phys\0virt\0hyp-phys\0") => (1, 2, 3),
                        _ => return Err(gic::Error::UnsupportedController),
                    };
                    let interrupts = node.interrupts.ok_or(gic::Error::MissingBinding)?;
                    if interrupts.len() != count * 12 {
                        return Err(gic::Error::InvalidProperty);
                    }
                    // Validate every declared timer, not just the chosen pair.
                    let mut used = 0u32;
                    for bytes in interrupts.chunks_exact(12) {
                        let ppi = gic::decode_ppi(bytes)?;
                        if ppi.trigger != gic::Trigger::HighLevel || used & (1 << ppi.id) != 0 {
                            return Err(gic::Error::InvalidProperty);
                        }
                        used |= 1 << ppi.id;
                    }
                    result = Some(Self {
                        virtual_timer: gic::decode_ppi(&interrupts[virt * 12..virt * 12 + 12])?,
                        hypervisor: gic::decode_ppi(&interrupts[hyp * 12..hyp * 12 + 12])?,
                    });
                }
                _ => {}
            }
        }
        result.ok_or(gic::Error::MissingBinding)
    }
}

#[derive(Default)]
struct Properties<'a> {
    compatible: Option<&'a [u8]>,
    parent: Option<&'a [u8]>,
    interrupts: Option<&'a [u8]>,
    names: Option<&'a [u8]>,
    status: Option<&'a [u8]>,
    unsupported: bool,
}
impl<'a> Properties<'a> {
    fn set(&mut self, name: &str, value: &'a [u8]) -> Result<(), gic::Error> {
        let slot = match name {
            "compatible" => &mut self.compatible,
            "interrupt-parent" => &mut self.parent,
            "interrupts" => &mut self.interrupts,
            "interrupt-names" => &mut self.names,
            "status" => &mut self.status,
            "interrupts-extended" | "clock-frequency" | "reg" => {
                self.unsupported = true;
                return Ok(());
            }
            _ if name.contains("erratum") => {
                self.unsupported = true;
                return Ok(());
            }
            _ => return Ok(()),
        };
        if slot.replace(value).is_some() {
            return Err(gic::Error::DuplicateProperty);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn actual_qemu_timer_wiring_is_not_guessed() {
        if let Some(path) = std::env::var_os("MBOOT_FDT_FIXTURE") {
            let bytes = std::fs::read(path).unwrap();
            let tree = Fdt::new(&bytes).unwrap();
            let controller = gic::GicV2::from_fdt(&tree).unwrap();
            let timers = Timers::from_fdt(&tree, &controller).unwrap();
            assert_eq!(timers.virtual_timer.id, 27);
            assert_eq!(timers.hypervisor.id, 26);
            assert!(Timers::from_fdt(
                &tree,
                &gic::GicV2 {
                    phandle: 123,
                    ..controller
                }
            )
            .is_err());
        }
    }
}
