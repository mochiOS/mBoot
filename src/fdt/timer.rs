//! Architected per-CPU timer wiring from the primary interrupt domain. This
//! metadata does not grant access to physical timer registers or PPIs.
use super::{gic, Fdt, Token, MAX_DEPTH};

#[derive(Clone, Copy, Debug)]
pub struct Timers {
    pub virtual_timer: gic::Ppi,
    pub hypervisor: gic::Ppi,
    /// A legacy firmware declaration to check against CNTFRQ_EL0 at admission.
    pub counter_frequency: Option<u32>,
}

impl Timers {
    /// Resolve the architected timer and its inherited interrupt parent.
    /// Interrupt remapping and timer erratum workarounds require separate support.
    pub fn from_fdt(tree: &Fdt<'_>, gic: &gic::GicV2) -> Result<Self, gic::Error> {
        let mut nodes = [Properties::default(); MAX_DEPTH];
        let mut disabled = false;
        let mut result = None;
        for token in tree.tokens() {
            match token.map_err(gic::Error::Tree)? {
                Token::BeginNode { depth, .. } => nodes[depth] = Properties::default(),
                Token::Property { depth, name, value } => nodes[depth].set(name, value)?,
                Token::EndNode { depth } => {
                    let node = &nodes[depth];
                    if depth == 0
                        || !matches!(
                            node.compatible,
                            Some(b"arm,armv8-timer\0" | b"arm,armv8-timer\0arm,armv7-timer\0")
                        )
                    {
                        continue;
                    }
                    let path = &nodes[..=depth];
                    if path.iter().any(|node| !node.enabled()) {
                        disabled = true;
                        continue;
                    }
                    if result.is_some() {
                        return Err(gic::Error::AmbiguousBinding);
                    }
                    let parent = path
                        .iter()
                        .rev()
                        .find_map(|node| node.parent)
                        .ok_or(gic::Error::MissingBinding)?;
                    if parent != gic.phandle.to_be_bytes()
                        || path.iter().any(|node| node.interrupt_nexus)
                    {
                        return Err(gic::Error::UnsupportedController);
                    }
                    result = Some(node.timers()?);
                }
            }
        }
        result.ok_or(if disabled {
            gic::Error::Disabled
        } else {
            gic::Error::MissingBinding
        })
    }
}

#[derive(Clone, Copy, Default)]
struct Properties<'a> {
    compatible: Option<&'a [u8]>,
    parent: Option<&'a [u8]>,
    interrupts: Option<&'a [u8]>,
    names: Option<&'a [u8]>,
    status: Option<&'a [u8]>,
    frequency: Option<&'a [u8]>,
    interrupt_nexus: bool,
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
            "clock-frequency" => &mut self.frequency,
            "#interrupt-cells"
            | "interrupt-controller"
            | "interrupt-map"
            | "interrupt-map-mask" => {
                self.interrupt_nexus = true;
                return Ok(());
            }
            "interrupts-extended" | "reg" => {
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

    fn enabled(&self) -> bool {
        matches!(self.status, None | Some(b"ok\0" | b"okay\0"))
    }

    fn timers(&self) -> Result<Timers, gic::Error> {
        if self.unsupported {
            return Err(gic::Error::UnsupportedController);
        }
        let (virt, hyp, count) = match self.names {
            None | Some(b"sec-phys\0phys\0virt\0hyp-phys\0") => (2, 3, 4),
            Some(b"phys\0virt\0hyp-phys\0") => (1, 2, 3),
            _ => return Err(gic::Error::UnsupportedController),
        };
        let interrupts = self.interrupts.ok_or(gic::Error::MissingBinding)?;
        let counter_frequency = self.frequency.map(gic::cell).transpose()?;
        if interrupts.len() != count * 12 || counter_frequency == Some(0) {
            return Err(gic::Error::InvalidProperty);
        }
        // The GIC can select edge or level. PPI polarity is a board wiring
        // property, so keep it in the descriptor and accept either level sense.
        let mut used = 0u32;
        for bytes in interrupts.chunks_exact(12) {
            let ppi = gic::decode_ppi(bytes)?;
            if ppi.trigger == gic::Trigger::RisingEdge || used & (1 << ppi.id) != 0 {
                return Err(gic::Error::InvalidProperty);
            }
            used |= 1 << ppi.id;
        }
        Ok(Timers {
            virtual_timer: gic::decode_ppi(&interrupts[virt * 12..virt * 12 + 12])?,
            hypervisor: gic::decode_ppi(&interrupts[hyp * 12..hyp * 12 + 12])?,
            counter_frequency,
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::super::tests::{words, Tree};
    use super::*;

    fn nested_timer(
        bus_properties: &[(&str, &[u8])],
        frequency: u32,
        flags: u32,
    ) -> Result<Timers, gic::Error> {
        use super::super::memory::Region;

        let mut tree = Tree::new();
        tree.begin("");
        tree.property("interrupt-parent", &1u32.to_be_bytes());
        tree.begin("soc");
        for (name, value) in bus_properties {
            tree.property(name, value);
        }
        tree.begin("timer");
        tree.property("compatible", b"arm,armv8-timer\0");
        tree.property(
            "interrupts",
            &words(&[1, 2, flags, 1, 3, flags, 1, 4, flags, 1, 1, flags]),
        );
        tree.property("clock-frequency", &frequency.to_be_bytes());
        tree.end();
        tree.end();
        tree.end();
        let bytes = tree.finish();
        let controller = gic::GicV2 {
            phandle: 1,
            distributor: Region::new(0x1000, 0x1000).unwrap(),
            cpu: Region::new(0x3000, 0x1000).unwrap(),
            virtualization: None,
            maintenance: None,
        };
        Timers::from_fdt(&Fdt::new(&bytes).unwrap(), &controller)
    }

    #[test]
    fn nested_timer_preserves_legacy_level_low_wiring_and_frequency() {
        let timers = nested_timer(&[], 19_200_000, 0xff08).unwrap();
        assert_eq!(timers.virtual_timer.id, 20);
        assert_eq!(timers.virtual_timer.cpu_mask, 0xff);
        assert_eq!(timers.virtual_timer.trigger, gic::Trigger::LowLevel);
        assert_eq!(timers.hypervisor.id, 17);
        assert_eq!(timers.hypervisor.trigger, gic::Trigger::LowLevel);
        assert_eq!(timers.counter_frequency, Some(19_200_000));
    }

    #[test]
    fn nested_timer_checks_the_ancestor_route_and_declaration() {
        for (property, data, error) in [
            (
                "interrupt-parent",
                &[0, 0, 0, 2][..],
                gic::Error::UnsupportedController,
            ),
            ("interrupt-map", &[][..], gic::Error::UnsupportedController),
            ("status", &b"disabled\0"[..], gic::Error::Disabled),
        ] {
            assert_eq!(
                nested_timer(&[(property, data)], 19_200_000, 0xff08).unwrap_err(),
                error
            );
        }
        assert_eq!(
            nested_timer(&[], 0, 0xff08).unwrap_err(),
            gic::Error::InvalidProperty
        );
        assert_eq!(
            nested_timer(&[], 19_200_000, 0xff01).unwrap_err(),
            gic::Error::InvalidProperty
        );
    }

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
