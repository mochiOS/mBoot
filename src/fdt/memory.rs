//! Physical RAM discovery and reservation planning. These are bounded metadata
//! views, not ownership of pages, MMIO or DMA. The boot frontend must supply a
//! trusted firmware tree and exclude its live image, stack, DTB and allocations
//! before planning Domain RAM. No board address or RAM-size fallback is used.
use super::{Fdt, Token};

const PAGE: u64 = 4096;
const MAX_RAM: usize = 16;
const MAX_RESERVED: usize = 128;
const MAX_REQUESTS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Tree(super::Error),
    InvalidProperty,
    DuplicateProperty,
    UnsupportedCells,
    UnsupportedTranslation,
    UnsupportedHierarchy,
    UnsupportedMemoryConstraint,
    InvalidRange,
    MissingRam,
    OverlappingRam,
    TooManyRegions,
    UnresolvedReservations,
    InvalidAllocation,
    NoMemory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    start: u64,
    end: u64,
}

impl Region {
    pub fn new(start: u64, size: u64) -> Result<Self, Error> {
        let end = start.checked_add(size).ok_or(Error::InvalidRange)?;
        if start == end {
            return Err(Error::InvalidRange);
        }
        Ok(Self { start, end })
    }
    pub fn start(self) -> u64 {
        self.start
    }
    pub fn size(self) -> u64 {
        self.end - self.start
    }
    pub fn end(self) -> u64 {
        self.end
    }
    fn contains(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }
    fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Reservation {
    pub region: Region,
    pub no_map: bool,
    pub reusable: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Cells {
    address: usize,
    size: usize,
}
impl Cells {
    fn new(address: &[u8], size: &[u8]) -> Result<Self, Error> {
        let address = integer(address, 1)? as usize;
        let size = integer(size, 1)? as usize;
        if !matches!(address, 1 | 2) || !matches!(size, 1 | 2) {
            return Err(Error::UnsupportedCells);
        }
        Ok(Self { address, size })
    }
    fn ranges(self, bytes: &[u8]) -> Result<Ranges<'_>, Error> {
        let stride = (self.address + self.size) * 4;
        if bytes.is_empty() || bytes.len() % stride != 0 {
            return Err(Error::InvalidProperty);
        }
        let ranges = Ranges { bytes, cells: self };
        for entry in bytes.chunks_exact(stride) {
            Region::new(
                integer(&entry[..self.address * 4], self.address)?,
                integer(&entry[self.address * 4..], self.size)?,
            )?;
        }
        Ok(ranges)
    }
}

/// Construction validates every tuple, including ranges not chosen by a plan.
#[derive(Clone, Copy)]
pub struct Ranges<'a> {
    bytes: &'a [u8],
    cells: Cells,
}
impl Iterator for Ranges<'_> {
    type Item = Region;
    fn next(&mut self) -> Option<Region> {
        let stride = (self.cells.address + self.cells.size) * 4;
        if self.bytes.is_empty() {
            return None;
        }
        let (entry, rest) = self.bytes.split_at(stride);
        self.bytes = rest;
        Some(
            Region::new(
                integer(&entry[..self.cells.address * 4], self.cells.address).unwrap(),
                integer(&entry[self.cells.address * 4..], self.cells.size).unwrap(),
            )
            .unwrap(),
        )
    }
}

#[derive(Clone, Copy)]
pub struct AllocationRequest<'a> {
    pub name: &'a str,
    pub size: u64,
    pub alignment: u64,
    pub no_map: bool,
    pub reusable: bool,
    ranges: Option<Ranges<'a>>,
    allocation: Option<Region>,
}
impl<'a> AllocationRequest<'a> {
    pub fn allowed_ranges(self) -> Option<Ranges<'a>> {
        self.ranges
    }
    pub fn allocation(self) -> Option<Region> {
        self.allocation
    }
}

/// Copying this value copies a proposed layout, never physical page ownership.
#[derive(Clone)]
pub struct MemoryMap<'a> {
    ram: [Region; MAX_RAM],
    ram_count: usize,
    reserved: [Reservation; MAX_RESERVED],
    reserved_count: usize,
    requests: [Option<AllocationRequest<'a>>; MAX_REQUESTS],
    request_count: usize,
}

impl<'a> MemoryMap<'a> {
    pub fn from_fdt(tree: &Fdt<'a>) -> Result<Self, Error> {
        const EMPTY: Region = Region { start: 0, end: 0 };
        let mut map = Self {
            ram: [EMPTY; MAX_RAM],
            ram_count: 0,
            reserved: [Reservation {
                region: EMPTY,
                no_map: false,
                reusable: false,
            }; MAX_RESERVED],
            reserved_count: 0,
            requests: [None; MAX_REQUESTS],
            request_count: 0,
        };
        for entry in tree.reservations() {
            map.reserve(Region::new(entry.address, entry.size)?)?;
        }
        let mut root = Properties::default();
        let mut container = Properties::default();
        let mut node = Properties::default();
        let mut root_kind = 0;
        let mut reserved_seen = false;
        for token in tree.tokens() {
            match token.map_err(Error::Tree)? {
                Token::BeginNode { depth: 1, name } => {
                    root_kind = if name == "reserved-memory" {
                        2
                    } else if name == "memory" || name.starts_with("memory@") {
                        1
                    } else {
                        0
                    };
                    container = Properties::default();
                    if root_kind == 2 {
                        if reserved_seen {
                            return Err(Error::DuplicateProperty);
                        }
                        reserved_seen = true;
                    }
                }
                Token::BeginNode { depth: 2, name } if root_kind == 2 => {
                    if !container.enabled()? {
                        return Err(Error::InvalidProperty);
                    }
                    if container.ranges != Some(b"".as_slice()) {
                        return Err(Error::UnsupportedTranslation);
                    }
                    if container.cells()? != root.cells()? {
                        return Err(Error::UnsupportedCells);
                    }
                    node = Properties {
                        name,
                        ..Properties::default()
                    };
                }
                Token::BeginNode { depth: 3, .. } if root_kind == 2 => {
                    return Err(Error::UnsupportedHierarchy);
                }
                Token::Property {
                    depth: 0,
                    name,
                    value,
                } => root.set(name, value)?,
                Token::Property {
                    depth: 1,
                    name,
                    value,
                } => container.set(name, value)?,
                Token::Property {
                    depth: 2,
                    name,
                    value,
                } if root_kind == 2 => node.set(name, value)?,
                Token::EndNode { depth: 2 } if root_kind == 2 => {
                    if node.enabled()? {
                        map.add_reservation(&node, container.cells()?)?;
                    }
                }
                Token::EndNode { depth: 1 } if root_kind == 1 => {
                    if container.device_type != Some(b"memory\0".as_slice()) {
                        return Err(Error::InvalidProperty);
                    }
                    if container.enabled()? {
                        for region in root
                            .cells()?
                            .ranges(container.reg.ok_or(Error::InvalidProperty)?)?
                        {
                            let slot = map
                                .ram
                                .get_mut(map.ram_count)
                                .ok_or(Error::TooManyRegions)?;
                            *slot = region;
                            map.ram_count += 1;
                        }
                    }
                }
                Token::EndNode { depth: 1 } => root_kind = 0,
                _ => {}
            }
        }
        if map.ram_count == 0 {
            return Err(Error::MissingRam);
        }
        map.ram[..map.ram_count].sort_unstable_by_key(|region| region.start);
        if map.ram().windows(2).any(|pair| pair[0].overlaps(pair[1])) {
            return Err(Error::OverlappingRam);
        }
        Ok(map)
    }

    pub fn ram(&self) -> &[Region] {
        &self.ram[..self.ram_count]
    }
    pub fn reservations(&self) -> &[Reservation] {
        &self.reserved[..self.reserved_count]
    }
    pub fn requests(&self) -> impl Iterator<Item = &AllocationRequest<'a>> {
        self.requests[..self.request_count].iter().flatten()
    }

    /// Exclude live boot objects before making a placement plan. Reservation
    /// records may overlap or lie outside RAM; neither adds any usable memory.
    pub fn reserve(&mut self, region: Region) -> Result<(), Error> {
        self.push_reservation(Reservation {
            region,
            no_map: false,
            reusable: false,
        })
    }

    /// Plan dynamic carveouts by first fit in declaration order, without
    /// touching physical memory or editing the DTB. The frontend must retain
    /// and publish this plan before using its pages.
    /// Failure leaves the original map unchanged, including earlier requests.
    pub fn plan_reservations(&self) -> Result<Self, Error> {
        let mut plan = self.clone();
        for index in 0..plan.request_count {
            let request = plan.requests[index].unwrap();
            if request.allocation.is_some() {
                continue;
            }
            let size = align_up(request.size, PAGE).ok_or(Error::InvalidRange)?;
            let alignment = request.alignment.max(PAGE);
            let mut selected = None;
            for free in plan.free_pages() {
                let consider = |window: Region| {
                    let start = align_up(free.start.max(window.start), alignment)?;
                    let region = Region::new(start, size).ok()?;
                    (free.contains(region) && window.contains(region)).then_some(region)
                };
                selected = if let Some(ranges) = request.ranges {
                    ranges.filter_map(consider).next()
                } else {
                    consider(free)
                };
                if selected.is_some() {
                    break;
                }
            }
            let region = selected.ok_or(Error::NoMemory)?;
            plan.push_reservation(Reservation {
                region,
                no_map: request.no_map,
                reusable: request.reusable,
            })?;
            plan.requests[index].as_mut().unwrap().allocation = Some(region);
        }
        Ok(plan)
    }

    /// Yield only whole 4-KiB pages after all declared reservations. Reusable
    /// pools remain excluded until their actual owner implements reclamation.
    pub fn usable_pages(&self) -> Result<impl Iterator<Item = Region> + '_, Error> {
        if self.requests().any(|request| request.allocation.is_none()) {
            return Err(Error::UnresolvedReservations);
        }
        Ok(self.free_pages())
    }

    fn free_pages(&self) -> FreePages<'_> {
        FreePages {
            map: self,
            ram: 0,
            cursor: 0,
        }
    }
    fn push_reservation(&mut self, entry: Reservation) -> Result<(), Error> {
        let slot = self
            .reserved
            .get_mut(self.reserved_count)
            .ok_or(Error::TooManyRegions)?;
        *slot = entry;
        self.reserved_count += 1;
        Ok(())
    }
    fn add_reservation(&mut self, node: &Properties<'a>, cells: Cells) -> Result<(), Error> {
        let no_map = boolean(node.no_map)?;
        let reusable = boolean(node.reusable)?;
        if no_map && reusable {
            return Err(Error::InvalidProperty);
        }
        if let Some(reg) = node.reg {
            for region in cells.ranges(reg)? {
                self.push_reservation(Reservation {
                    region,
                    no_map,
                    reusable,
                })?;
            }
        } else {
            let size = integer(node.size.ok_or(Error::InvalidProperty)?, cells.size)?;
            let alignment = node
                .alignment
                .map(|v| integer(v, cells.size))
                .transpose()?
                .unwrap_or(PAGE);
            if size == 0 || !alignment.is_power_of_two() {
                return Err(Error::InvalidRange);
            }
            let ranges = node.alloc_ranges.map(|v| cells.ranges(v)).transpose()?;
            let slot = self
                .requests
                .get_mut(self.request_count)
                .ok_or(Error::TooManyRegions)?;
            *slot = Some(AllocationRequest {
                name: node.name,
                size,
                alignment,
                no_map,
                reusable,
                ranges,
                allocation: None,
            });
            self.request_count += 1;
        }
        Ok(())
    }
}

struct FreePages<'a> {
    map: &'a MemoryMap<'a>,
    ram: usize,
    cursor: u64,
}
impl Iterator for FreePages<'_> {
    type Item = Region;
    fn next(&mut self) -> Option<Region> {
        while let Some(ram) = self.map.ram().get(self.ram) {
            self.cursor = self.cursor.max(ram.start);
            while self.cursor < ram.end {
                let mut covered = self.cursor;
                let mut end = ram.end;
                for entry in self.map.reservations() {
                    if entry.region.start <= self.cursor && self.cursor < entry.region.end {
                        covered = covered.max(entry.region.end.min(ram.end));
                    } else if entry.region.start > self.cursor {
                        end = end.min(entry.region.start);
                    }
                }
                if covered > self.cursor {
                    self.cursor = covered;
                    continue;
                }
                let start = align_up(self.cursor, PAGE);
                self.cursor = end;
                if let Some(start) = start.filter(|&start| start < (end & !(PAGE - 1))) {
                    return Some(Region {
                        start,
                        end: end & !(PAGE - 1),
                    });
                }
            }
            self.ram += 1;
        }
        None
    }
}

#[derive(Default)]
struct Properties<'a> {
    name: &'a str,
    address_cells: Option<&'a [u8]>,
    size_cells: Option<&'a [u8]>,
    device_type: Option<&'a [u8]>,
    status: Option<&'a [u8]>,
    reg: Option<&'a [u8]>,
    ranges: Option<&'a [u8]>,
    size: Option<&'a [u8]>,
    alignment: Option<&'a [u8]>,
    alloc_ranges: Option<&'a [u8]>,
    no_map: Option<&'a [u8]>,
    reusable: Option<&'a [u8]>,
}
impl<'a> Properties<'a> {
    fn set(&mut self, name: &str, value: &'a [u8]) -> Result<(), Error> {
        let slot = match name {
            "#address-cells" => &mut self.address_cells,
            "#size-cells" => &mut self.size_cells,
            "device_type" => &mut self.device_type,
            "status" => &mut self.status,
            "reg" => &mut self.reg,
            "ranges" => &mut self.ranges,
            "size" => &mut self.size,
            "alignment" => &mut self.alignment,
            "alloc-ranges" => &mut self.alloc_ranges,
            "no-map" => &mut self.no_map,
            "reusable" => &mut self.reusable,
            "linux,usable-memory" | "linux,usable-memory-range" => {
                return Err(Error::UnsupportedMemoryConstraint);
            }
            _ => return Ok(()),
        };
        if slot.replace(value).is_some() {
            Err(Error::DuplicateProperty)
        } else {
            Ok(())
        }
    }
    fn cells(&self) -> Result<Cells, Error> {
        Cells::new(
            self.address_cells.ok_or(Error::UnsupportedCells)?,
            self.size_cells.ok_or(Error::UnsupportedCells)?,
        )
    }
    fn enabled(&self) -> Result<bool, Error> {
        match self.status {
            None | Some(b"ok\0" | b"okay\0") => Ok(true),
            Some(v) if v.last() == Some(&0) && !v[..v.len() - 1].contains(&0) => Ok(false),
            _ => Err(Error::InvalidProperty),
        }
    }
}
fn boolean(value: Option<&[u8]>) -> Result<bool, Error> {
    match value {
        None => Ok(false),
        Some([]) => Ok(true),
        _ => Err(Error::InvalidProperty),
    }
}
fn integer(bytes: &[u8], cells: usize) -> Result<u64, Error> {
    if bytes.len() != cells * 4 || !matches!(cells, 1 | 2) {
        return Err(Error::InvalidProperty);
    }
    Ok(bytes
        .iter()
        .fold(0, |value, &byte| value << 8 | u64::from(byte)))
}
fn align_up(value: u64, alignment: u64) -> Option<u64> {
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::super::tests::Tree;
    use super::*;
    use std::vec::Vec;

    fn cells(values: &[u64]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect()
    }
    fn start(ram: &[u64]) -> Tree {
        let mut tree = Tree::new();
        tree.begin("");
        tree.property("#address-cells", &2u32.to_be_bytes());
        tree.property("#size-cells", &2u32.to_be_bytes());
        tree.begin("memory");
        tree.property("device_type", b"memory\0");
        tree.property("reg", &cells(ram));
        tree.end();
        tree.begin("reserved-memory");
        tree.property("#address-cells", &2u32.to_be_bytes());
        tree.property("#size-cells", &2u32.to_be_bytes());
        tree.property("ranges", b"");
        tree
    }
    fn fixed(tree: &mut Tree, start: u64, size: u64) {
        tree.begin(&std::format!("pool@{start:x}"));
        tree.property("reg", &cells(&[start, size]));
        tree.property("no-map", b"");
        tree.end();
    }
    fn finish(mut tree: Tree) -> Vec<u8> {
        tree.end();
        tree.end();
        tree.finish()
    }
    fn usable(map: &MemoryMap<'_>) -> Vec<(u64, u64)> {
        map.usable_pages()
            .unwrap()
            .map(|r| (r.start(), r.size()))
            .collect()
    }

    #[test]
    fn overlapping_carveouts_and_partial_pages_never_become_free_ram() {
        let base = 1u64 << 32;
        let mut tree = start(&[base + 0x1001, 0x1efff]);
        fixed(&mut tree, base + 0x3001, 0x2fff);
        fixed(&mut tree, base + 0x4000, 0x3000);
        let bytes = finish(tree);
        let mut map = MemoryMap::from_fdt(&Fdt::new(&bytes).unwrap()).unwrap();
        map.reserve(Region::new(base + 0x8100, 0x100).unwrap())
            .unwrap();
        assert_eq!(
            usable(&map),
            [
                (base + 0x2000, 0x1000),
                (base + 0x7000, 0x1000),
                (base + 0x9000, 0x17000)
            ]
        );
        assert!(map
            .reservations()
            .iter()
            .any(|r| r.region.start() == 0x4400_0000));
        assert!(map.reservations().iter().any(|r| r.no_map));
    }

    #[test]
    fn dynamic_pools_obey_alignment_windows_and_boot_object_reservations() {
        let mut tree = start(&[0x1000, 0x1f000]);
        tree.begin("dma-pool");
        tree.property("size", &cells(&[0x1800]));
        tree.property("alignment", &cells(&[0x4000]));
        tree.property("alloc-ranges", &cells(&[0x9000, 0x7000]));
        tree.property("reusable", b"");
        tree.end();
        fixed(&mut tree, 0x1000, 0x3000);
        let bytes = finish(tree);
        let mut map = MemoryMap::from_fdt(&Fdt::new(&bytes).unwrap()).unwrap();
        assert!(matches!(
            map.usable_pages(),
            Err(Error::UnresolvedReservations)
        ));
        let plan = map.plan_reservations().unwrap();
        let request = *plan.requests().next().unwrap();
        assert_eq!(request.name, "dma-pool");
        assert!(request.reusable);
        let pool = Region::new(0xc000, 0x2000).unwrap();
        assert_eq!(request.allocation(), Some(pool));
        assert!(plan
            .usable_pages()
            .unwrap()
            .all(|free| !free.overlaps(pool)));
        assert!(map.requests().all(|request| request.allocation().is_none()));
        map.reserve(Region::new(0xc000, 4096).unwrap()).unwrap();
        // The only permitted window ends at 0x10000; reserving 0xc000 must
        // actually make this plan impossible, never spill out of that window.
        assert!(matches!(map.plan_reservations(), Err(Error::NoMemory)));
    }

    #[test]
    fn failed_plan_is_atomic_and_firmware_placeholders_are_rejected() {
        let bytes = finish(start(&[0, 0]));
        assert!(matches!(
            MemoryMap::from_fdt(&Fdt::new(&bytes).unwrap()),
            Err(Error::InvalidRange)
        ));
        let bytes = finish(start(&[0x1000, 0x5000, 0x4000, 0x3000]));
        assert!(matches!(
            MemoryMap::from_fdt(&Fdt::new(&bytes).unwrap()),
            Err(Error::OverlappingRam)
        ));
        let mut tree = start(&[0x1000, 0x5000]);
        for name in ["first", "second"] {
            tree.begin(name);
            tree.property("size", &cells(&[0x4000]));
            tree.end();
        }
        let bytes = finish(tree);
        let map = MemoryMap::from_fdt(&Fdt::new(&bytes).unwrap()).unwrap();
        assert!(matches!(map.plan_reservations(), Err(Error::NoMemory)));
        assert!(map.requests().all(|request| request.allocation().is_none()));
        assert!(matches!(
            map.usable_pages(),
            Err(Error::UnresolvedReservations)
        ));
        assert!(Region::new(u64::MAX, 1).is_err());
        let mut tree = start(&[0x1000, 0x5000]);
        tree.end();
        tree.begin("chosen");
        tree.property("linux,usable-memory-range", &cells(&[0x2000, 0x1000]));
        tree.end();
        tree.end();
        let bytes = tree.finish();
        assert!(matches!(
            MemoryMap::from_fdt(&Fdt::new(&bytes).unwrap()),
            Err(Error::UnsupportedMemoryConstraint)
        ));
    }

    #[test]
    fn actual_qemu_ram_and_stock_template_are_distinguished() {
        if let Some(path) = std::env::var_os("MBOOT_FDT_FIXTURE") {
            let bytes = std::fs::read(path).unwrap();
            let map = MemoryMap::from_fdt(&Fdt::new(&bytes).unwrap()).unwrap();
            assert_eq!(
                map.ram(),
                [Region::new(0x4000_0000, 128 * 1024 * 1024).unwrap()]
            );
            assert!(map.usable_pages().is_ok());
            std::println!("Real QEMU firmware RAM ranges PASS");
        }
        if let Some(path) = std::env::var_os("MBOOT_STOCK_FDT_FIXTURE") {
            let bytes = std::fs::read(path).unwrap();
            assert!(matches!(
                MemoryMap::from_fdt(&Fdt::new(&bytes).unwrap()),
                Err(Error::InvalidRange)
            ));
            std::println!("Stock DTB with unpopulated RAM is rejected PASS");
        }
    }
}
