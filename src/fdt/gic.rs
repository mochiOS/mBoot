//! Primary GICv2 firmware binding. Descriptors are not MMIO mappings, device
//! ownership, or permission to expose registers to a Domain. Non-banked and
//! cascaded controllers need separate admission and are rejected here.
// Binding: Documentation/devicetree/bindings/interrupt-controller/arm,gic.yaml
// in Linux; address translation follows DTSpec reg/ranges, not node names.
use super::memory::Region;
use super::{Fdt, Token, MAX_DEPTH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Tree(super::Error),
    DuplicateProperty,
    InvalidProperty,
    UnsupportedCells,
    MissingBinding,
    AmbiguousBinding,
    Disabled,
    UnsupportedController,
    InvalidRegion,
    UnmappedRegion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    RisingEdge,
    HighLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaintenanceInterrupt {
    /// Architectural INTID, including the PPI base of 16.
    pub id: u16,
    pub trigger: Trigger,
    /// DT wiring metadata, not a logical vCPU assignment or routing authority.
    pub cpu_mask: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Virtualization {
    pub control: Region,
    pub cpu: Region,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GicV2 {
    pub phandle: u32,
    pub distributor: Region,
    pub cpu: Region,
    pub virtualization: Option<Virtualization>,
    pub maintenance: Option<MaintenanceInterrupt>,
}

impl GicV2 {
    /// Resolve the root's interrupt-parent, never the first similarly named
    /// node. Every translated range must fit one explicit ancestor window.
    /// Missing ranges are not assumed to mean identity translation.
    pub fn from_fdt(fdt: &Fdt<'_>) -> Result<Self, Error> {
        let mut path = [Properties::default(); MAX_DEPTH];
        let mut binding = None;
        let mut parent = None;
        for token in fdt.tokens() {
            match token.map_err(Error::Tree)? {
                Token::BeginNode { depth, .. } => path[depth] = Properties::default(),
                Token::Property { depth, name, value } => {
                    path[depth].set(name, value)?;
                    if depth == 0 && name == "interrupt-parent" {
                        parent = Some(phandle(value)?);
                    }
                }
                Token::EndNode { depth } => {
                    let node = &path[depth];
                    let handle = node.handle()?;
                    if handle.is_some() && handle == parent {
                        if binding.is_some() || depth == 0 {
                            return Err(Error::AmbiguousBinding);
                        }
                        if path[..=depth].iter().any(|p| !p.enabled()) {
                            return Err(Error::Disabled);
                        }
                        binding = Some(Self::resolve(&path[..=depth], handle.unwrap())?);
                    }
                }
            }
        }
        binding.ok_or(Error::MissingBinding)
    }

    fn resolve(path: &[Properties<'_>], id: u32) -> Result<Self, Error> {
        let depth = path.len() - 1;
        if depth == 0 {
            return Err(Error::UnsupportedController);
        }
        let interrupt_parent = path
            .iter()
            .rev()
            .find_map(|node| node.interrupt_parent)
            .map(phandle)
            .transpose()?;
        let node = &path[depth];
        let compatible = node.compatible.ok_or(Error::UnsupportedController)?;
        let supported = compatible.last() == Some(&0)
            && compatible[..compatible.len() - 1]
                .split(|&byte| byte == 0)
                .any(|value| {
                    matches!(
                        value,
                        b"arm,gic-400"
                            | b"arm,cortex-a15-gic"
                            | b"arm,cortex-a7-gic"
                            | b"qcom,msm-qgic2"
                    )
                });
        if !supported
            || node.controller != Some(&[])
            || node.interrupt_cells.map(cell).transpose()? != Some(3)
            || node.cpu_offset.map(cell).transpose()?.unwrap_or(0) != 0
            || node.interrupts_extended.is_some()
            || interrupt_parent != Some(id)
            || path.iter().any(|node| node.interrupt_map.is_some())
        {
            return Err(Error::UnsupportedController);
        }
        // Validate all strings, including unknown fallback names.
        if compatible[..compatible.len() - 1]
            .split(|&byte| byte == 0)
            .any(|value| value.is_empty() || core::str::from_utf8(value).is_err())
        {
            return Err(Error::InvalidProperty);
        }
        let cells = path[depth - 1].cells()?;
        let reg = node.reg.ok_or(Error::InvalidRegion)?;
        let stride = (cells.0 + cells.1) * 4;
        let count = reg.len() / stride;
        if reg.len() % stride != 0 || !matches!(count, 2 | 4) {
            return Err(Error::InvalidRegion);
        }
        let mut regions = [None; 4];
        for (index, entry) in reg.chunks_exact(stride).enumerate() {
            let address = number(&entry[..cells.0 * 4])?;
            let size = number(&entry[cells.0 * 4..])?;
            let mut region = region(address, size, cells.0)?;
            for ancestor in (1..depth).rev() {
                region = translate(region, &path[ancestor], &path[ancestor - 1])?;
            }
            if regions[..index]
                .iter()
                .flatten()
                .any(|prior: &Region| prior.start() < region.end() && region.start() < prior.end())
            {
                return Err(Error::InvalidRegion);
            }
            regions[index] = Some(region);
        }
        let maintenance = node.interrupts.map(maintenance).transpose()?;
        if count == 2 && maintenance.is_some() {
            return Err(Error::UnsupportedController);
        }
        Ok(Self {
            phandle: id,
            distributor: regions[0].unwrap(),
            cpu: regions[1].unwrap(),
            virtualization: (count == 4).then(|| Virtualization {
                control: regions[2].unwrap(),
                cpu: regions[3].unwrap(),
            }),
            maintenance,
        })
    }
}

pub(super) fn maintenance(bytes: &[u8]) -> Result<MaintenanceInterrupt, Error> {
    if bytes.len() != 12 || cell(&bytes[..4])? != 1 {
        return Err(Error::InvalidProperty);
    }
    let number = cell(&bytes[4..8])?;
    let flags = cell(&bytes[8..])?;
    if number >= 16 || flags & !0xff0f != 0 {
        return Err(Error::InvalidProperty);
    }
    let trigger = match flags & 15 {
        1 => Trigger::RisingEdge,
        4 => Trigger::HighLevel,
        _ => return Err(Error::InvalidProperty),
    };
    Ok(MaintenanceInterrupt {
        id: 16 + number as u16,
        trigger,
        cpu_mask: (flags >> 8) as u8,
    })
}

fn translate(
    input: Region,
    bus: &Properties<'_>,
    parent: &Properties<'_>,
) -> Result<Region, Error> {
    let (address, size) = bus.cells()?;
    let (parent_address, _) = parent.cells()?;
    let ranges = bus.ranges.ok_or(Error::UnmappedRegion)?;
    if ranges.is_empty() {
        return region(input.start(), input.size(), parent_address);
    }
    let stride = (address + parent_address + size) * 4;
    if ranges.len() % stride != 0 {
        return Err(Error::InvalidRegion);
    }
    let mut translated = None;
    for entry in ranges.chunks_exact(stride) {
        let source = number(&entry[..address * 4])?;
        let target = number(&entry[address * 4..(address + parent_address) * 4])?;
        let length = number(&entry[(address + parent_address) * 4..])?;
        let window = region(source, length, address)?;
        region(target, length, parent_address)?;
        if input.start() < window.end() && window.start() < input.end() {
            if translated.is_some() || input.start() < window.start() || input.end() > window.end()
            {
                return Err(Error::InvalidRegion);
            }
            translated = Some(region(
                target + (input.start() - source),
                input.size(),
                parent_address,
            )?);
        }
    }
    translated.ok_or(Error::UnmappedRegion)
}

fn region(start: u64, size: u64, cells: usize) -> Result<Region, Error> {
    let region = Region::new(start, size).map_err(|_| Error::InvalidRegion)?;
    if cells == 1 && region.end() > 1 << 32 {
        return Err(Error::InvalidRegion);
    }
    Ok(region)
}

#[derive(Clone, Copy, Default)]
struct Properties<'a> {
    address_cells: Option<&'a [u8]>,
    size_cells: Option<&'a [u8]>,
    interrupt_cells: Option<&'a [u8]>,
    interrupt_parent: Option<&'a [u8]>,
    controller: Option<&'a [u8]>,
    compatible: Option<&'a [u8]>,
    phandle: Option<&'a [u8]>,
    linux_phandle: Option<&'a [u8]>,
    reg: Option<&'a [u8]>,
    ranges: Option<&'a [u8]>,
    cpu_offset: Option<&'a [u8]>,
    interrupts: Option<&'a [u8]>,
    interrupts_extended: Option<&'a [u8]>,
    interrupt_map: Option<&'a [u8]>,
    status: Option<&'a [u8]>,
}

impl<'a> Properties<'a> {
    fn set(&mut self, name: &str, value: &'a [u8]) -> Result<(), Error> {
        let slot = match name {
            "#address-cells" => &mut self.address_cells,
            "#size-cells" => &mut self.size_cells,
            "#interrupt-cells" => &mut self.interrupt_cells,
            "interrupt-parent" => &mut self.interrupt_parent,
            "interrupt-controller" => &mut self.controller,
            "compatible" => &mut self.compatible,
            "phandle" => &mut self.phandle,
            "linux,phandle" => &mut self.linux_phandle,
            "reg" => &mut self.reg,
            "ranges" => &mut self.ranges,
            "cpu-offset" => &mut self.cpu_offset,
            "interrupts" => &mut self.interrupts,
            "interrupts-extended" => &mut self.interrupts_extended,
            "interrupt-map" => &mut self.interrupt_map,
            "status" => &mut self.status,
            _ => return Ok(()),
        };
        if slot.replace(value).is_some() {
            return Err(Error::DuplicateProperty);
        }
        Ok(())
    }

    fn enabled(&self) -> bool {
        matches!(self.status, None | Some(b"ok\0" | b"okay\0"))
    }

    fn cells(&self) -> Result<(usize, usize), Error> {
        let address = self.address_cells.map(cell).transpose()?.unwrap_or(2);
        let size = self.size_cells.map(cell).transpose()?.unwrap_or(1);
        if !matches!(address, 1 | 2) || !matches!(size, 1 | 2) {
            return Err(Error::UnsupportedCells);
        }
        Ok((address as usize, size as usize))
    }

    fn handle(&self) -> Result<Option<u32>, Error> {
        let current = self.phandle.map(phandle).transpose()?;
        let legacy = self.linux_phandle.map(phandle).transpose()?;
        if matches!((current, legacy), (Some(a), Some(b)) if a != b) {
            return Err(Error::AmbiguousBinding);
        }
        Ok(current.or(legacy))
    }
}

fn cell(bytes: &[u8]) -> Result<u32, Error> {
    Ok(u32::from_be_bytes(
        bytes.try_into().map_err(|_| Error::InvalidProperty)?,
    ))
}
fn phandle(bytes: &[u8]) -> Result<u32, Error> {
    let id = cell(bytes)?;
    if matches!(id, 0 | u32::MAX) {
        return Err(Error::InvalidProperty);
    }
    Ok(id)
}
fn number(bytes: &[u8]) -> Result<u64, Error> {
    match bytes.len() {
        4 => Ok(u64::from(cell(bytes)?)),
        8 => Ok(u64::from_be_bytes(bytes.try_into().unwrap())),
        _ => Err(Error::UnsupportedCells),
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::super::tests::{words, Tree};
    use super::*;
    use std::vec::Vec;

    const REGS: [u32; 8] = [
        0x1000, 0x1000, 0x3000, 0x2000, 0x6000, 0x1000, 0x8000, 0x2000,
    ];
    const RANGES: [u32; 4] = [0, 1, 0x2000_0000, 0x10000];

    fn bus(ranges: Option<&[u32]>) -> Tree {
        let mut tree = Tree::new();
        tree.begin("");
        tree.property("#address-cells", &2u32.to_be_bytes());
        tree.property("#size-cells", &2u32.to_be_bytes());
        tree.property("interrupt-parent", &1u32.to_be_bytes());
        tree.begin("soc");
        tree.property("#address-cells", &1u32.to_be_bytes());
        tree.property("#size-cells", &1u32.to_be_bytes());
        if let Some(ranges) = ranges {
            tree.property("ranges", &words(ranges));
        }
        tree
    }

    fn controller(tree: &mut Tree, reg: &[u32], extra: impl FnOnce(&mut Tree)) {
        tree.begin("interrupt-controller@1000");
        tree.property("compatible", b"arm,gic-400\0arm,cortex-a15-gic\0");
        tree.property("interrupt-controller", &[]);
        tree.property("#interrupt-cells", &3u32.to_be_bytes());
        tree.property("phandle", &1u32.to_be_bytes());
        tree.property("reg", &words(reg));
        extra(tree);
        tree.end();
    }

    fn finish(mut tree: Tree) -> Vec<u8> {
        tree.end();
        tree.end();
        tree.finish()
    }
    fn discover(bytes: &[u8]) -> Result<GicV2, Error> {
        GicV2::from_fdt(&Fdt::new(bytes).unwrap())
    }

    #[test]
    fn translated_banks_and_maintenance_keep_physical_and_interrupt_identities() {
        let mut tree = bus(Some(&RANGES));
        controller(&mut tree, &REGS, |tree| {
            tree.property("linux,phandle", &1u32.to_be_bytes());
            tree.property("interrupts", &words(&[1, 9, 0xf04]));
        });
        let gic = discover(&finish(tree)).unwrap();
        assert_eq!(gic.distributor, Region::new(0x1_2000_1000, 4096).unwrap());
        assert_eq!(gic.cpu, Region::new(0x1_2000_3000, 8192).unwrap());
        let virtualized = gic.virtualization.unwrap();
        assert_eq!(virtualized.control.start(), 0x1_2000_6000);
        assert_eq!(virtualized.cpu.start(), 0x1_2000_8000);
        assert_eq!(
            gic.maintenance,
            Some(MaintenanceInterrupt {
                id: 25,
                trigger: Trigger::HighLevel,
                cpu_mask: 15,
            })
        );
        let mut tree = bus(Some(&[]));
        controller(&mut tree, &REGS[..4], |_| {});
        let physical_only = discover(&finish(tree)).unwrap();
        assert_eq!(physical_only.distributor.start(), 0x1000);
        assert!(physical_only.virtualization.is_none());

        let mut tree = bus(Some(&RANGES));
        tree.begin("bridge");
        tree.property("#address-cells", &1u32.to_be_bytes());
        tree.property("#size-cells", &1u32.to_be_bytes());
        tree.property("ranges", &words(&[0, 0x2000, 0xc000]));
        controller(&mut tree, &REGS, |_| {});
        tree.end();
        let nested = discover(&finish(tree)).unwrap();
        assert_eq!(nested.distributor.start(), 0x1_2000_3000);
        assert_eq!(nested.virtualization.unwrap().cpu.start(), 0x1_2000_a000);
    }

    #[test]
    fn missing_ambiguous_partial_overflowed_or_aliased_windows_are_rejected() {
        for (ranges, regs, expected) in [
            (None, &REGS[..], Error::UnmappedRegion),
            (
                Some(&[0, 1, 0x2000_0000, 0x1800][..]),
                &REGS[..],
                Error::InvalidRegion,
            ),
            (
                Some(&[0, 1, 0, 0x10000, 0, 2, 0, 0x10000][..]),
                &REGS[..],
                Error::InvalidRegion,
            ),
            (
                Some(&[0, u32::MAX, 0xffff_0000, 0x20000][..]),
                &REGS[..],
                Error::InvalidRegion,
            ),
            (Some(&RANGES[..]), &REGS[..6], Error::InvalidRegion),
            (
                Some(&RANGES[..]),
                &[0x1000, 0x2000, 0x2000, 0x1000][..],
                Error::InvalidRegion,
            ),
        ] {
            let mut tree = bus(ranges);
            controller(&mut tree, regs, |_| {});
            assert_eq!(discover(&finish(tree)), Err(expected));
        }
    }

    #[test]
    fn duplicate_disabled_or_non_banked_bindings_are_not_guessed() {
        let mut tree = bus(Some(&RANGES));
        controller(&mut tree, &REGS, |_| {});
        tree.begin("decoy");
        tree.property("phandle", &1u32.to_be_bytes());
        tree.end();
        assert_eq!(discover(&finish(tree)), Err(Error::AmbiguousBinding));
        for (property, value, expected) in [
            ("status", &b"disabled\0"[..], Error::Disabled),
            (
                "linux,phandle",
                &2u32.to_be_bytes()[..],
                Error::AmbiguousBinding,
            ),
            (
                "cpu-offset",
                &4096u32.to_be_bytes()[..],
                Error::UnsupportedController,
            ),
            ("interrupts", &words(&[0, 9, 4])[..], Error::InvalidProperty),
            (
                "interrupt-parent",
                &2u32.to_be_bytes()[..],
                Error::UnsupportedController,
            ),
            ("interrupt-map", &[][..], Error::UnsupportedController),
        ] {
            let mut tree = bus(Some(&RANGES));
            controller(&mut tree, &REGS, |tree| tree.property(property, value));
            assert_eq!(discover(&finish(tree)), Err(expected));
        }
        for (property, value, expected) in [
            ("status", &b"disabled\0"[..], Error::Disabled),
            (
                "interrupt-parent",
                &2u32.to_be_bytes()[..],
                Error::UnsupportedController,
            ),
            ("interrupt-map", &[][..], Error::UnsupportedController),
        ] {
            let mut tree = bus(Some(&RANGES));
            tree.property(property, value);
            controller(&mut tree, &REGS, |_| {});
            assert_eq!(discover(&finish(tree)), Err(expected));
        }
    }

    #[test]
    fn real_firmware_does_not_invent_absent_virtualization_banks() {
        if let Some(path) = std::env::var_os("MBOOT_FDT_FIXTURE") {
            let bytes = std::fs::read(path).unwrap();
            let gic = discover(&bytes).unwrap();
            assert_eq!(gic.distributor.start(), 0x0800_0000);
            let virtualized = gic.virtualization.unwrap();
            assert_eq!(virtualized.control.start(), 0x0803_0000);
            assert_eq!(virtualized.cpu.start(), 0x0804_0000);
            assert_eq!(gic.maintenance.unwrap().id, 25);
        }
        if let Some(path) = std::env::var_os("MBOOT_STOCK_FDT_FIXTURE") {
            let bytes = std::fs::read(path).unwrap();
            let gic = discover(&bytes).unwrap();
            assert_eq!(gic.distributor.start(), 0x0b00_0000);
            assert_eq!(gic.cpu.start(), 0x0b00_2000);
            assert!(gic.virtualization.is_none());
            assert!(gic.maintenance.is_none());
        }
    }
}
