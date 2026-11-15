//! Non-VHE, 4-KiB Stage-2 translation with explicitly owned 8-bit VMIDs.
//! CPU address translation does not establish DMA isolation or device authority.
use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};

pub const PAGE_SIZE: u64 = 4096;
const ADDRESS_MASK: u64 = 0x0000_ffff_ffff_f000;
const TABLE_OR_PAGE: u64 = 3;
const ACCESS_FLAG: u64 = 1 << 10;
const INNER_SHAREABLE: u64 = 3 << 8;
const READ: u64 = 1 << 6;
const WRITE: u64 = 1 << 7;
const NORMAL_WB: u64 = 15 << 2;
const EXECUTE_NEVER: u64 = 1 << 54;
const HCR_VM: u64 = 1;
const HCR_RW: u64 = 1 << 31;
const UNSUPPORTED_HOST_MODE: u64 = (1 << 34) | (1 << 27); // E2H / TGE

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    UnsupportedCpu,
    UnsupportedHostMode,
    InactiveAddressSpace,
    InvalidAddress,
    InvalidPermissions,
    InvalidIdentifier,
    InvalidDescriptor,
    AlreadyMapped,
    NotMapped,
    OutOfMemory,
}

#[repr(C, align(4096))]
pub struct PageTable {
    entries: [u64; 512],
}

impl PageTable {
    pub const fn zeroed() -> Self {
        Self { entries: [0; 512] }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryType {
    Normal,
    Device,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    pub writable: bool,
    pub executable: bool,
    pub memory: MemoryType,
}

impl Access {
    fn descriptor(self, frame: u64) -> Result<u64, Error> {
        if self.memory == MemoryType::Device && self.executable {
            return Err(Error::InvalidPermissions);
        }
        Ok(frame
            | TABLE_OR_PAGE
            | ACCESS_FLAG
            | INNER_SHAREABLE
            | READ
            | if self.writable { WRITE } else { 0 }
            | if self.executable { 0 } else { EXECUTE_NEVER }
            | if self.memory == MemoryType::Normal {
                NORMAL_WB
            } else {
                0
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    physical_bits: u8,
    ipa_bits: u8,
    first_level: u8,
    root_pages: usize,
    ps: u64,
}

impl Config {
    /// Derive baseline geometry without assuming FEAT_TTST's smaller tables.
    /// A concatenated root covers the extra IPA bits at its starting level.
    pub fn from_features(features: u64, ipa_bits: Option<u8>) -> Result<Self, Error> {
        let granule = (features >> 40) & 15;
        if !match granule {
            0 => (features >> 28) & 15 <= 1,
            2 | 3 => true,
            _ => false,
        } {
            return Err(Error::UnsupportedCpu);
        }
        let ps = features & 15;
        let physical_bits = match ps {
            0 => 32,
            1 => 36,
            2 => 40,
            3 => 42,
            4 => 44,
            5 | 6 => 48,
            _ => return Err(Error::UnsupportedCpu),
        };
        let ipa_bits = ipa_bits.unwrap_or(physical_bits);
        if !(32..=physical_bits).contains(&ipa_bits) {
            return Err(Error::InvalidAddress);
        }
        let (first_level, root_pages) = if ipa_bits >= 44 {
            (0, 1)
        } else if ipa_bits >= 35 {
            (1, 1usize << ipa_bits.saturating_sub(39))
        } else {
            (2, 1usize << (ipa_bits - 30))
        };
        Ok(Self {
            physical_bits,
            ipa_bits,
            first_level,
            root_pages,
            ps: ps.min(5),
        })
    }

    pub fn detect() -> Result<Self, Error> {
        let features;
        // SAFETY: Read-only CPU feature access from privileged boot code.
        unsafe {
            asm!("mrs {}, id_aa64mmfr0_el1", out(reg) features, options(nomem, nostack, preserves_flags));
        }
        Self::from_features(features, None)
    }

    pub fn root_pages(self) -> usize {
        self.root_pages
    }
    pub fn ipa_bits(self) -> u8 {
        self.ipa_bits
    }
    pub fn physical_bits(self) -> u8 {
        self.physical_bits
    }
    pub fn root_alignment(self) -> u64 {
        self.root_pages as u64 * PAGE_SIZE
    }

    pub fn vtcr(self) -> u64 {
        (1 << 31)
            | (self.ps << 16)
            | (3 << 12)
            | (1 << 10)
            | (1 << 8)
            | (u64::from(2 - self.first_level) << 6)
            | u64::from(64 - self.ipa_bits)
    }

    fn frame(self, physical: u64) -> Result<(), Error> {
        if physical & (PAGE_SIZE - 1) != 0 || physical >= 1u64 << self.physical_bits {
            return Err(Error::InvalidAddress);
        }
        Ok(())
    }

    fn root(self, root: u64) -> Result<(), Error> {
        self.frame(root)?;
        let end = root
            .checked_add(self.root_alignment())
            .ok_or(Error::InvalidAddress)?;
        if root == 0 || root & (self.root_alignment() - 1) != 0 || end > 1u64 << self.physical_bits
        {
            return Err(Error::InvalidAddress);
        }
        Ok(())
    }
}

/// Access to mBoot-owned table frames, including the contiguous root block.
///
/// # Safety
/// Roots and allocated children are distinct, nonzero, aligned, zeroed, pinned writable
/// frames. Every returned pointer denotes exactly one valid PageTable. The
/// allocator cannot return an already-owned/live frame. Freeing is permitted
/// only for unpublished children, never a table used by a hardware walker.
pub unsafe trait TableMemory {
    fn allocate_table(&mut self) -> Result<u64, Error>;
    unsafe fn free_table(&mut self, frame: u64);
    unsafe fn table_ptr(&self, frame: u64) -> *mut PageTable;
}

/// A borrowed translation editor, not the owner of the Domain or table pool.
/// The resource owner retains roots/children until all guests and CPUs stop
/// referencing them, and invalidates the VMID before reusing those resources.
pub struct Mapper<'a, M: TableMemory> {
    memory: &'a mut M,
    root: u64,
    config: Config,
    vmid: u8,
}

impl<'a, M: TableMemory> Mapper<'a, M> {
    /// Attach an existing, exclusively owned root; does not clear live mappings.
    ///
    /// # Safety
    /// Root/children must satisfy TableMemory, and `vmid` must be exclusively
    /// reserved for this address space by the hypervisor's VMID allocator.
    pub unsafe fn new(
        memory: &'a mut M,
        root: u64,
        config: Config,
        vmid: u8,
    ) -> Result<Self, Error> {
        config.root(root)?;
        if vmid == 0 {
            return Err(Error::InvalidIdentifier);
        }
        Ok(Self {
            memory,
            root,
            config,
            vmid,
        })
    }

    fn ipa(&self, ipa: u64) -> Result<(), Error> {
        if ipa & (PAGE_SIZE - 1) != 0 || ipa >= 1u64 << self.config.ipa_bits {
            return Err(Error::InvalidAddress);
        }
        Ok(())
    }

    fn read(&self, table: u64, index: usize) -> u64 {
        // SAFETY: Validated root/child frames and a masked 9-bit table index.
        unsafe {
            read_volatile(core::ptr::addr_of!(
                (*self.memory.table_ptr(table)).entries[index]
            ))
        }
    }

    fn write(&mut self, table: u64, index: usize, value: u64) {
        // SAFETY: The editing contract grants exclusive table access.
        unsafe {
            write_volatile(
                core::ptr::addr_of_mut!((*self.memory.table_ptr(table)).entries[index]),
                value,
            );
        }
    }

    fn top(&self, ipa: u64) -> u64 {
        let shift = 39 - self.config.first_level * 9;
        self.root + ((ipa >> (shift + 9)) * PAGE_SIZE)
    }

    fn child(&self, entry: u64) -> Result<u64, Error> {
        if entry & TABLE_OR_PAGE != TABLE_OR_PAGE || entry & !(ADDRESS_MASK | TABLE_OR_PAGE) != 0 {
            return Err(Error::InvalidDescriptor);
        }
        let frame = entry & ADDRESS_MASK;
        self.config.frame(frame)?;
        if frame == 0 {
            return Err(Error::InvalidDescriptor);
        }
        Ok(frame)
    }

    fn leaf(&self, ipa: u64) -> Result<(u64, usize, u64), Error> {
        self.ipa(ipa)?;
        let mut table = self.top(ipa);
        for level in self.config.first_level..3 {
            let entry = self.read(table, ((ipa >> (39 - 9 * level)) & 511) as usize);
            if entry == 0 {
                return Err(Error::NotMapped);
            }
            table = self.child(entry)?;
        }
        let index = ((ipa >> 12) & 511) as usize;
        let entry = self.read(table, index);
        if entry == 0 {
            return Err(Error::NotMapped);
        }
        let known = ADDRESS_MASK
            | TABLE_OR_PAGE
            | ACCESS_FLAG
            | INNER_SHAREABLE
            | READ
            | WRITE
            | NORMAL_WB
            | EXECUTE_NEVER;
        let memory = entry & NORMAL_WB;
        if entry & (TABLE_OR_PAGE | ACCESS_FLAG | INNER_SHAREABLE | READ)
            != TABLE_OR_PAGE | ACCESS_FLAG | INNER_SHAREABLE | READ
            || entry & !known != 0
            || !matches!(memory, 0 | NORMAL_WB)
            || memory == 0 && entry & EXECUTE_NEVER == 0
        {
            return Err(Error::InvalidDescriptor);
        }
        self.config.frame(entry & ADDRESS_MASK)?;
        Ok((table, index, entry))
    }

    fn vttbr(&self) -> u64 {
        self.root | (u64::from(self.vmid) << 48)
    }

    /// Identity retained by a vCPU; changing the selected VMID/root cannot
    /// silently move a continuation into another Domain's address space.
    pub(super) fn identity(&self) -> (u64, u64) {
        (self.vttbr(), self.config.vtcr())
    }

    pub(super) fn require_active(&self) -> Result<(), Error> {
        self.editing_context()
    }

    fn context(&self) -> Result<(u64, u64, u64), Error> {
        let level: u64;
        let daif: u64;
        // SAFETY: These read-only registers may be inspected before EL2 setup.
        unsafe {
            asm!("mrs {}, CurrentEL", "mrs {}, daif", out(reg) level, out(reg) daif, options(nomem, nostack, preserves_flags));
        }
        if level != 8 || daif & 0xc0 != 0xc0 {
            return Err(Error::UnsupportedHostMode);
        }
        let hcr: u64;
        let vtcr: u64;
        let vttbr: u64;
        // SAFETY: CurrentEL confirmed EL2; no EL2 register bank aliasing is used.
        unsafe {
            asm!("mrs {}, hcr_el2", "mrs {}, vtcr_el2", "mrs {}, vttbr_el2", out(reg) hcr, out(reg) vtcr, out(reg) vttbr, options(nomem, nostack, preserves_flags));
        }
        if hcr & UNSUPPORTED_HOST_MODE != 0 || hcr & HCR_RW == 0 {
            return Err(Error::UnsupportedHostMode);
        }
        let native = Config::detect()?;
        if self.config.physical_bits > native.physical_bits
            || self.config.ipa_bits > native.physical_bits
        {
            return Err(Error::UnsupportedCpu);
        }
        Ok((hcr, vtcr, vttbr))
    }

    fn editing_context(&self) -> Result<(), Error> {
        let (hcr, vtcr, vttbr) = self.context()?;
        if hcr & HCR_VM == 0 || vtcr != self.config.vtcr() || vttbr != self.vttbr() {
            return Err(Error::InactiveAddressSpace);
        }
        Ok(())
    }

    /// Flush the selected VMID; never switch translation contexts implicitly.
    unsafe fn invalidate(&self) {
        // SAFETY: editing_context() verified the active address space. All
        // sharing guests remain quiescent and reentrancy is excluded.
        unsafe {
            asm!(
                "dsb ishst",
                "tlbi vmalls12e1is",
                "dsb ish",
                "isb",
                options(nostack, preserves_flags)
            );
        }
    }

    /// Select the address space for the next guest entry on this physical CPU.
    ///
    /// # Safety
    /// Run non-reentrantly at EL2 with IRQ/FIQ masked and no guest executing.
    /// All tables and mapped Domain pages must remain pinned and owned.
    /// EL1 Stage-1 controls must be valid with SCTLR_EL1.M and both TCR_EL1
    /// EPD bits set to suppress speculative walks during the VMID switch.
    /// The vCPU entry owner restores the destination's controls afterwards.
    pub unsafe fn activate(&self) -> Result<(), Error> {
        let (hcr, _, _) = self.context()?;
        let sctlr: u64;
        let tcr: u64;
        // SAFETY: context() checked privileged non-VHE EL2 access.
        unsafe {
            asm!("mrs {}, sctlr_el1", "mrs {}, tcr_el1", out(reg) sctlr, out(reg) tcr,
                options(nomem, nostack, preserves_flags));
        }
        const EPD: u64 = (1 << 7) | (1 << 23);
        if sctlr & 1 == 0 || tcr & EPD != EPD {
            return Err(Error::UnsupportedHostMode);
        }
        // SAFETY: The caller owns guest entry and context() checked the CPU.
        unsafe {
            asm!("dsb ish", "msr hcr_el2, {off}", "isb",
                "msr vtcr_el2, {control}", "msr vttbr_el2, {root}", "isb",
                "tlbi vmalls12e1is", "dsb ish", "isb", "msr hcr_el2, {on}", "isb",
                off = in(reg) hcr & !HCR_VM, control = in(reg) self.config.vtcr(),
                root = in(reg) self.vttbr(), on = in(reg) hcr | HCR_VM,
                options(nostack, preserves_flags));
        }
        Ok(())
    }

    /// Map a Domain-owned page. Allocation failure publishes no partial chain.
    ///
    /// # Safety
    /// Run under the address-space editing lock with all sharing guests stopped,
    /// at non-reentrant EL2 with IRQ/FIQ masked. `frame` must belong to this
    /// Domain or an explicitly authorized grant/device mapping, never mBoot's
    /// tables/code/private data. CPU translation alone is not DMA permission.
    /// This address space must be active on the editing CPU. Before executing
    /// newly written code, the owner must complete instruction-cache coherence.
    pub unsafe fn map_page(&mut self, ipa: u64, frame: u64, access: Access) -> Result<(), Error> {
        self.ipa(ipa)?;
        self.config.frame(frame)?;
        let value = access.descriptor(frame)?;
        self.editing_context()?;
        let mut table = self.top(ipa);
        let mut links = [(0u64, 0usize, 0u64); 3];
        let mut count = 0;
        let result = (|| {
            for level in self.config.first_level..3 {
                let index = ((ipa >> (39 - 9 * level)) & 511) as usize;
                let entry = self.read(table, index);
                let child = if entry == 0 {
                    let child = self.memory.allocate_table()?;
                    // Invalid/aliased frames violate the unsafe allocator
                    // contract. Do not free an alias and corrupt a live root.
                    if child == 0
                        || child & (PAGE_SIZE - 1) != 0
                        || (self.root..self.root + self.config.root_alignment()).contains(&child)
                        || links[..count].iter().any(|&(_, _, frame)| frame == child)
                    {
                        return Err(Error::InvalidDescriptor);
                    }
                    if self.config.frame(child).is_err() {
                        // SAFETY: An aligned, unique allocation can be returned
                        // when this CPU's configured PA range cannot encode it.
                        unsafe {
                            self.memory.free_table(child);
                        }
                        return Err(Error::InvalidAddress);
                    }
                    links[count] = (table, index, child);
                    count += 1;
                    child
                } else {
                    self.child(entry)?
                };
                table = child;
            }
            let index = ((ipa >> 12) & 511) as usize;
            if self.read(table, index) != 0 {
                return Err(Error::AlreadyMapped);
            }
            self.write(table, index, value);
            for &(parent, at, child) in links[..count].iter().rev() {
                // SAFETY: Make a completely initialized child visible first.
                unsafe {
                    asm!("dsb ishst", options(nostack, preserves_flags));
                }
                self.write(parent, at, child | TABLE_OR_PAGE);
            }
            // SAFETY: All VMID edit preconditions were checked before mutation.
            unsafe {
                self.invalidate();
            }
            Ok(())
        })();
        if result.is_err() {
            for &(_, _, child) in links[..count].iter().rev() {
                // SAFETY: No parent was published before the fallible work ended.
                unsafe {
                    self.memory.free_table(child);
                }
            }
        }
        result
    }

    /// # Safety
    /// Same quiescence, editing-lock and EL2 requirements as map_page().
    pub unsafe fn protect_page(&mut self, ipa: u64, access: Access) -> Result<(), Error> {
        self.editing_context()?;
        let (table, index, old) = self.leaf(ipa)?;
        let new = access.descriptor(old & ADDRESS_MASK)?;
        if old != new {
            self.write(table, index, 0);
            // SAFETY: Complete break-before-make for memory and access attributes.
            unsafe {
                self.invalidate();
            }
            self.write(table, index, new);
            // SAFETY: Publish the replacement before any guest can resume.
            unsafe {
                self.invalidate();
            }
        }
        Ok(())
    }

    /// Return the frame only after its Stage-2/combined translations are gone.
    /// The caller retains frame ownership and decides when it can be freed.
    ///
    /// # Safety
    /// Same quiescence, editing-lock and EL2 requirements as map_page().
    pub unsafe fn unmap_page(&mut self, ipa: u64) -> Result<u64, Error> {
        self.editing_context()?;
        let (table, index, entry) = self.leaf(ipa)?;
        self.write(table, index, 0);
        // SAFETY: Synchronous invalidation completes before returning the frame.
        unsafe {
            self.invalidate();
        }
        Ok(entry & ADDRESS_MASK)
    }
}
