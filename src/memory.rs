use core::ptr::{read_volatile, write_bytes, write_volatile};

use crate::{BackendKind, Error};

const PAGE_SIZE: u64 = 4096;
const ENTRY_COUNT: usize = 512;
const EPT_READ_WRITE_EXECUTE: u64 = 0b111;
const EPT_LEAF_WRITE_BACK: u64 = EPT_READ_WRITE_EXECUTE | (6 << 3);
const EPT_DEVICE_READ_WRITE: u64 = 0b011;
const EPT_WRITE_BACK: u64 = 6;
const EPT_WALK_LENGTH_4: u64 = 3 << 3;
const NPT_PRESENT_WRITE_USER: u64 = 0b111;
const NPT_PRESENT_USER_NO_EXECUTE: u64 = 0b101 | (1 << 63);
const NPT_DEVICE_READ_WRITE: u64 = NPT_PRESENT_WRITE_USER | (1 << 3) | (1 << 4) | (1 << 63);
// Native Domains start in ring 0. Their initial identity map must remain
// supervisor-only; the guest kernel creates user-accessible mappings explicitly
// when it constructs a process address space.
const GUEST_PAGE_TABLE_FLAGS: u64 = 0b011;
const GUEST_LARGE_PAGE_FLAGS: u64 = GUEST_PAGE_TABLE_FLAGS | (1 << 7);
const LARGE_PAGE_SIZE: u64 = 2 * 1024 * 1024;
const NESTED_LARGE_PAGE: u64 = 1 << 7;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NestedPageResources {
    pub root: u64,
    pub level3: u64,
    pub level3_pages: usize,
    pub level2: u64,
    pub level2_pages: usize,
    pub level1: u64,
    pub level1_pages: usize,
    pub device_level1: u64,
    pub device_level1_pages: usize,
    pub device_state: u64,
    pub guest_base: u64,
    pub guest_pages: usize,
}

impl NestedPageResources {
    pub fn validate(self) -> Result<(), Error> {
        for page in [
            self.root,
            self.level3,
            self.level2,
            self.level1,
            self.device_level1,
            self.device_state,
            self.guest_base,
        ] {
            if page == 0 || page & (PAGE_SIZE - 1) != 0 {
                return Err(Error::InvalidPage);
            }
        }
        let required_level1_pages = self.guest_pages.div_ceil(ENTRY_COUNT);
        let required_level3_pages = self.level2_pages.div_ceil(ENTRY_COUNT);
        if self.guest_pages == 0
            || required_level1_pages > ENTRY_COUNT
            || required_level3_pages == 0
            || required_level3_pages > ENTRY_COUNT
            || self.level3_pages != required_level3_pages
            || self.level2_pages == 0
            || self.level2_pages > ENTRY_COUNT * ENTRY_COUNT
            || self.level1_pages != required_level1_pages
            || self.device_level1_pages == 0
        {
            return Err(Error::InvalidPage);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NestedPageTable {
    backend: BackendKind,
    root: u64,
    level3: u64,
    level3_pages: usize,
    level1: u64,
    level1_pages: usize,
    level2: u64,
    level2_pages: usize,
    device_level1: u64,
    device_level1_pages: usize,
    device_state: u64,
    guest_base: u64,
    guest_pages: usize,
}

impl NestedPageTable {
    /// Builds a four-level translation for the contiguous RAM owned by a Domain.
    /// No guest physical address outside that RAM is mapped.
    ///
    /// # Safety
    /// Every address in `resources` must be identity-mapped, writable, and
    /// exclusively owned by mBoot for the lifetime of the returned table.
    pub unsafe fn initialize(
        backend: BackendKind,
        resources: NestedPageResources,
    ) -> Result<Self, Error> {
        resources.validate()?;
        // SAFETY: The function contract gives mBoot exclusive writable ownership.
        unsafe { write_bytes(resources.root as *mut u8, 0, PAGE_SIZE as usize) };
        unsafe {
            write_bytes(
                resources.level3 as *mut u8,
                0,
                resources.level3_pages * PAGE_SIZE as usize,
            );
            write_bytes(
                resources.level2 as *mut u8,
                0,
                resources.level2_pages * PAGE_SIZE as usize,
            );
            write_bytes(
                resources.device_level1 as *mut u8,
                0,
                resources.device_level1_pages * PAGE_SIZE as usize,
            );
            write_bytes(resources.device_state as *mut u8, 0, PAGE_SIZE as usize);
        }
        // SAFETY: The complete contiguous leaf-table allocation is exclusively owned.
        unsafe {
            write_bytes(
                resources.level1 as *mut u8,
                0,
                resources.level1_pages * PAGE_SIZE as usize,
            )
        };

        let link_flags = match backend {
            BackendKind::IntelVmx => EPT_READ_WRITE_EXECUTE,
            BackendKind::AmdSvm => NPT_PRESENT_WRITE_USER,
        };
        // SAFETY: All tables were validated, zeroed, and are exclusively owned.
        unsafe {
            for index in 0..resources.level3_pages {
                write_entry(
                    resources.root,
                    index,
                    (resources.level3 + index as u64 * PAGE_SIZE) | link_flags,
                );
            }
            write_entry(resources.level3, 0, resources.level2 | link_flags);
            for index in 0..resources.level1_pages {
                write_entry(
                    resources.level2,
                    index,
                    (resources.level1 + index as u64 * PAGE_SIZE) | link_flags,
                );
            }
            let leaf_flags = match backend {
                BackendKind::IntelVmx => EPT_LEAF_WRITE_BACK,
                BackendKind::AmdSvm => NPT_PRESENT_WRITE_USER,
            };
            for index in 0..resources.guest_pages {
                let table = resources.level1 + (index / ENTRY_COUNT) as u64 * PAGE_SIZE;
                write_entry(
                    table,
                    index % ENTRY_COUNT,
                    (resources.guest_base + index as u64 * PAGE_SIZE) | leaf_flags,
                );
            }
            write_bytes(
                resources.guest_base as *mut u8,
                0,
                resources.guest_pages * PAGE_SIZE as usize,
            );
        }

        Ok(Self {
            backend,
            root: resources.root,
            level3: resources.level3,
            level3_pages: resources.level3_pages,
            level1: resources.level1,
            level1_pages: resources.level1_pages,
            level2: resources.level2,
            level2_pages: resources.level2_pages,
            device_level1: resources.device_level1,
            device_level1_pages: resources.device_level1_pages,
            device_state: resources.device_state,
            guest_base: resources.guest_base,
            guest_pages: resources.guest_pages,
        })
    }

    pub const fn root(&self) -> u64 {
        self.root
    }

    pub const fn guest_base(&self) -> u64 {
        self.guest_base
    }

    pub const fn guest_memory_size(&self) -> u64 {
        self.guest_pages as u64 * PAGE_SIZE
    }

    pub fn guest_host_address(&self, guest_address: u64, len: u64) -> Option<u64> {
        let end = guest_address.checked_add(len)?;
        if end > self.guest_memory_size() {
            return None;
        }
        self.guest_base.checked_add(guest_address)
    }

    pub fn owned_page_host_address(&self, guest_page: u64) -> Option<u64> {
        if guest_page & (PAGE_SIZE - 1) != 0
            || guest_page.checked_add(PAGE_SIZE)? > self.guest_memory_size()
        {
            return None;
        }
        self.guest_base.checked_add(guest_page)
    }

    /// Replaces one stopped Domain mapping with a page owned by another Domain.
    ///
    /// # Safety
    /// Both addresses must denote live aligned pages. No vCPU may use this nested
    /// page table until the backend translation cache has been invalidated.
    pub unsafe fn map_shared_page(
        &self,
        guest_page: u64,
        host_page: u64,
        writable: bool,
    ) -> Result<(), Error> {
        let (table, index) = self.page_entry(guest_page)?;
        if host_page == 0 || host_page & (PAGE_SIZE - 1) != 0 {
            return Err(Error::InvalidPage);
        }
        let flags = match self.backend {
            BackendKind::IntelVmx => (6 << 3) | 1 | if writable { 1 << 1 } else { 0 },
            BackendKind::AmdSvm => NPT_PRESENT_USER_NO_EXECUTE | if writable { 1 << 1 } else { 0 },
        };
        unsafe { write_entry(table, index, host_page | flags) };
        Ok(())
    }

    /// Restores a shared guest page to the Domain's own backing page.
    ///
    /// # Safety
    /// The vCPU must be stopped until the backend translation cache is invalidated.
    pub unsafe fn restore_owned_page(&self, guest_page: u64) -> Result<(), Error> {
        let (table, index) = self.page_entry(guest_page)?;
        let host_page = self
            .guest_base
            .checked_add(guest_page)
            .ok_or(Error::InvalidPage)?;
        let flags = match self.backend {
            BackendKind::IntelVmx => EPT_LEAF_WRITE_BACK,
            BackendKind::AmdSvm => NPT_PRESENT_WRITE_USER,
        };
        unsafe { write_entry(table, index, host_page | flags) };
        Ok(())
    }

    /// Maps one physical device page into a stopped Domain with non-executable,
    /// uncacheable semantics.
    ///
    /// # Safety
    /// `host_page` must be a validated device MMIO page. The vCPU must remain
    /// stopped until the nested translation cache has been invalidated.
    pub unsafe fn map_device_page(&self, guest_page: u64, host_page: u64) -> Result<(), Error> {
        let (table, index) = self.page_entry(guest_page)?;
        if host_page == 0 || host_page & (PAGE_SIZE - 1) != 0 || host_page >> 52 != 0 {
            return Err(Error::InvalidPage);
        }
        let flags = match self.backend {
            BackendKind::IntelVmx => EPT_DEVICE_READ_WRITE,
            BackendKind::AmdSvm => NPT_DEVICE_READ_WRITE,
        };
        unsafe { write_entry(table, index, host_page | flags) };
        Ok(())
    }

    /// Maps a PCI BAR into sparse guest-physical MMIO space. Aligned portions
    /// use 2 MiB nested leaves so a large GPU aperture does not consume one
    /// page-table entry per 4 KiB page.
    ///
    /// # Safety
    /// The complete host range must be validated device MMIO. The vCPU must be
    /// stopped until the nested translation cache has been invalidated.
    pub unsafe fn map_device_range(
        &self,
        guest_start: u64,
        host_start: u64,
        len: u64,
    ) -> Result<(), Error> {
        let end = guest_start.checked_add(len).ok_or(Error::InvalidPage)?;
        if guest_start < self.guest_memory_size()
            || guest_start & (PAGE_SIZE - 1) != 0
            || host_start == 0
            || host_start & (PAGE_SIZE - 1) != 0
            || len == 0
            || len & (PAGE_SIZE - 1) != 0
            || end > self.level2_pages as u64 * 1024 * 1024 * 1024
        {
            return Err(Error::InvalidPage);
        }
        let mut offset = 0;
        while offset < len {
            let guest = guest_start + offset;
            let host = host_start + offset;
            let result = if guest & (LARGE_PAGE_SIZE - 1) == 0
                && host & (LARGE_PAGE_SIZE - 1) == 0
                && len - offset >= LARGE_PAGE_SIZE
            {
                unsafe { self.map_device_large_page(guest, host) }
            } else {
                unsafe { self.map_sparse_device_page(guest, host) }
            };
            if let Err(error) = result {
                if offset != 0 {
                    // Keep the operation transactional. Callers may safely
                    // retry the assignment or release the other BARs without
                    // leaving a partially exposed device aperture behind.
                    unsafe { self.unmap_device_range(guest_start, offset)? };
                }
                return Err(error);
            }
            offset += if guest & (LARGE_PAGE_SIZE - 1) == 0
                && host & (LARGE_PAGE_SIZE - 1) == 0
                && len - offset >= LARGE_PAGE_SIZE
            {
                LARGE_PAGE_SIZE
            } else {
                PAGE_SIZE
            };
        }
        Ok(())
    }

    /// Removes a sparse PCI BAR mapping. Page-table pages stay reserved for the
    /// Domain and may be reused only after a Domain restart.
    ///
    /// # Safety
    /// The vCPU must be stopped until the nested translation cache is flushed.
    pub unsafe fn unmap_device_range(&self, guest_start: u64, len: u64) -> Result<(), Error> {
        if guest_start < self.guest_memory_size()
            || guest_start & (PAGE_SIZE - 1) != 0
            || len == 0
            || len & (PAGE_SIZE - 1) != 0
        {
            return Err(Error::InvalidPage);
        }
        let mut offset = 0;
        while offset < len {
            let guest = guest_start + offset;
            let level2_entry = self.sparse_level2_entry(guest)?;
            let value = unsafe { read_volatile(level2_entry) };
            if value & NESTED_LARGE_PAGE != 0 {
                unsafe { write_volatile(level2_entry, 0) };
                offset += LARGE_PAGE_SIZE;
                continue;
            }
            let level1 = value & 0x000f_ffff_ffff_f000;
            if level1 == 0 {
                return Err(Error::InvalidPage);
            }
            let entry = unsafe { (level1 as *mut u64).add(((guest >> 12) & 0x1ff) as usize) };
            if unsafe { read_volatile(entry) } == 0 {
                return Err(Error::InvalidPage);
            }
            unsafe { write_volatile(entry, 0) };
            offset += PAGE_SIZE;
        }
        Ok(())
    }

    unsafe fn map_device_large_page(&self, guest: u64, host: u64) -> Result<(), Error> {
        let entry = self.sparse_level2_entry(guest)?;
        if unsafe { read_volatile(entry) } != 0 {
            return Err(Error::InvalidPage);
        }
        let flags = match self.backend {
            BackendKind::IntelVmx => EPT_DEVICE_READ_WRITE | NESTED_LARGE_PAGE,
            BackendKind::AmdSvm => NPT_DEVICE_READ_WRITE | NESTED_LARGE_PAGE,
        };
        unsafe { write_volatile(entry, host | flags) };
        Ok(())
    }

    unsafe fn map_sparse_device_page(&self, guest: u64, host: u64) -> Result<(), Error> {
        let level2_entry = self.sparse_level2_entry(guest)?;
        let mut level1 = unsafe { read_volatile(level2_entry) } & 0x000f_ffff_ffff_f000;
        if level1 == 0 {
            let state = self.device_state as *mut usize;
            let index = unsafe { read_volatile(state) };
            if index >= self.device_level1_pages {
                return Err(Error::InvalidPage);
            }
            level1 = self.device_level1 + index as u64 * PAGE_SIZE;
            unsafe { write_volatile(state, index + 1) };
            let link_flags = match self.backend {
                BackendKind::IntelVmx => EPT_READ_WRITE_EXECUTE,
                BackendKind::AmdSvm => NPT_PRESENT_WRITE_USER,
            };
            unsafe { write_volatile(level2_entry, level1 | link_flags) };
        } else if unsafe { read_volatile(level2_entry) } & NESTED_LARGE_PAGE != 0 {
            return Err(Error::InvalidPage);
        }
        let entry = unsafe { (level1 as *mut u64).add(((guest >> 12) & 0x1ff) as usize) };
        if unsafe { read_volatile(entry) } != 0 {
            return Err(Error::InvalidPage);
        }
        let flags = match self.backend {
            BackendKind::IntelVmx => EPT_DEVICE_READ_WRITE,
            BackendKind::AmdSvm => NPT_DEVICE_READ_WRITE,
        };
        unsafe { write_volatile(entry, host | flags) };
        Ok(())
    }

    fn sparse_level2_entry(&self, guest: u64) -> Result<*mut u64, Error> {
        let level2_index = usize::try_from(guest >> 30).map_err(|_| Error::InvalidPage)?;
        if level2_index >= self.level2_pages {
            return Err(Error::InvalidPage);
        }
        let level3_page = level2_index / ENTRY_COUNT;
        if level3_page >= self.level3_pages {
            return Err(Error::InvalidPage);
        }
        let level3_table = self.level3 + level3_page as u64 * PAGE_SIZE;
        let level3_entry = unsafe { (level3_table as *mut u64).add(level2_index % ENTRY_COUNT) };
        let expected = self.level2 + level2_index as u64 * PAGE_SIZE;
        let current = unsafe { read_volatile(level3_entry) } & 0x000f_ffff_ffff_f000;
        if current == 0 {
            let link_flags = match self.backend {
                BackendKind::IntelVmx => EPT_READ_WRITE_EXECUTE,
                BackendKind::AmdSvm => NPT_PRESENT_WRITE_USER,
            };
            unsafe { write_volatile(level3_entry, expected | link_flags) };
        } else if current != expected {
            return Err(Error::InvalidPage);
        }
        Ok(unsafe { (expected as *mut u64).add(((guest >> 21) & 0x1ff) as usize) })
    }

    fn page_entry(&self, guest_page: u64) -> Result<(u64, usize), Error> {
        self.owned_page_host_address(guest_page)
            .ok_or(Error::InvalidPage)?;
        let page = usize::try_from(guest_page / PAGE_SIZE).map_err(|_| Error::InvalidPage)?;
        let table_index = page / ENTRY_COUNT;
        if table_index >= self.level1_pages {
            return Err(Error::InvalidPage);
        }
        Ok((
            self.level1 + table_index as u64 * PAGE_SIZE,
            page % ENTRY_COUNT,
        ))
    }

    /// Creates a guest-owned four-level table that identity maps all Domain RAM.
    /// The PML4 starts at GPA 0 and is suitable for CR3.
    ///
    /// # Safety
    /// No vCPU may be using the first three guest pages while this runs.
    pub unsafe fn initialize_guest_page_tables(&self) -> Result<u64, Error> {
        if self.guest_memory_size() < 3 * PAGE_SIZE {
            return Err(Error::InvalidPage);
        }
        let pml4 = self.guest_base;
        let pdpt = self.guest_base + PAGE_SIZE;
        let directory = self.guest_base + 2 * PAGE_SIZE;
        // SAFETY: These three pages belong exclusively to stopped guest RAM.
        unsafe {
            write_bytes(pml4 as *mut u8, 0, (3 * PAGE_SIZE) as usize);
            write_entry(pml4, 0, PAGE_SIZE | GUEST_PAGE_TABLE_FLAGS);
            write_entry(pdpt, 0, (2 * PAGE_SIZE) | GUEST_PAGE_TABLE_FLAGS);
            for index in 0..self.guest_pages.div_ceil(ENTRY_COUNT) {
                write_entry(
                    directory,
                    index,
                    index as u64 * 2 * 1024 * 1024 | GUEST_LARGE_PAGE_FLAGS,
                );
            }
        }
        Ok(0)
    }

    /// Extends a Native Domain's initial identity map over a sparse device range.
    /// Nested translation must separately map every page in the range.
    ///
    /// # Safety
    /// `initialize_guest_page_tables` must have completed and the vCPU must be stopped.
    pub unsafe fn map_guest_identity_device_range(
        &self,
        guest_start: u64,
        len: u64,
    ) -> Result<(), Error> {
        let end = guest_start.checked_add(len).ok_or(Error::InvalidPage)?;
        if guest_start < self.guest_memory_size()
            || guest_start & (PAGE_SIZE - 1) != 0
            || len == 0
            || len & (PAGE_SIZE - 1) != 0
            || end > 1024 * 1024 * 1024
        {
            return Err(Error::InvalidPage);
        }
        let directory = self.guest_base + 2 * PAGE_SIZE;
        let first =
            usize::try_from(guest_start / LARGE_PAGE_SIZE).map_err(|_| Error::InvalidPage)?;
        let last = usize::try_from((end - 1) / LARGE_PAGE_SIZE).map_err(|_| Error::InvalidPage)?;
        if last >= ENTRY_COUNT {
            return Err(Error::InvalidPage);
        }
        for index in first..=last {
            unsafe {
                write_entry(
                    directory,
                    index,
                    index as u64 * LARGE_PAGE_SIZE | GUEST_LARGE_PAGE_FLAGS,
                )
            };
        }
        Ok(())
    }

    /// Clears all Domain RAM before a fresh image is loaded.
    ///
    /// # Safety
    /// The Domain vCPU must be stopped and no backend may access guest RAM.
    pub unsafe fn clear_guest_memory(&self) {
        unsafe {
            write_bytes(
                self.guest_base as *mut u8,
                0,
                self.guest_memory_size() as usize,
            )
        };
    }

    pub const fn hardware_root(&self) -> u64 {
        match self.backend {
            BackendKind::IntelVmx => self.root | EPT_WRITE_BACK | EPT_WALK_LENGTH_4,
            BackendKind::AmdSvm => self.root,
        }
    }

    #[cfg(test)]
    pub(crate) const fn test_new(
        backend: BackendKind,
        root: u64,
        guest_base: u64,
        guest_pages: usize,
    ) -> Self {
        Self {
            backend,
            root,
            level3: root,
            level3_pages: 1,
            level1: root,
            level1_pages: guest_pages.div_ceil(ENTRY_COUNT),
            level2: root,
            level2_pages: 1,
            device_level1: root,
            device_level1_pages: 1,
            device_state: root,
            guest_base,
            guest_pages,
        }
    }
}

unsafe fn write_entry(table: u64, index: usize, value: u64) {
    debug_assert!(index < ENTRY_COUNT);
    // SAFETY: The caller supplies a mapped page and an in-bounds index.
    unsafe { (table as *mut u64).add(index).write_volatile(value) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(align(4096))]
    struct Page([u64; ENTRY_COUNT]);

    #[repr(align(4096))]
    struct TwoPages([u64; ENTRY_COUNT * 2]);

    #[test]
    fn ept_maps_only_configured_guest_pages() {
        let mut root = Page([0; ENTRY_COUNT]);
        let mut level3 = Page([0; ENTRY_COUNT]);
        let mut level2 = Page([0; ENTRY_COUNT]);
        let mut level1 = Page([0; ENTRY_COUNT]);
        let mut device_level1 = Page([0; ENTRY_COUNT]);
        let mut device_state = Page([0; ENTRY_COUNT]);
        let mut guest = Page([u64::MAX; ENTRY_COUNT]);
        let resources = resources(
            &mut root,
            &mut level3,
            &mut level2,
            &mut level1,
            &mut device_level1,
            &mut device_state,
            &mut guest,
        );

        // SAFETY: Test pages are aligned, live, writable, and uniquely borrowed.
        let table =
            unsafe { NestedPageTable::initialize(BackendKind::IntelVmx, resources) }.unwrap();
        assert_eq!(root.0[0], resources.level3 | EPT_READ_WRITE_EXECUTE);
        assert_eq!(level1.0[0], resources.guest_base | EPT_LEAF_WRITE_BACK);
        assert_eq!(level1.0[1], 0);
        assert_eq!(guest.0[0], 0);
        let mut shared = Page([0; ENTRY_COUNT]);
        unsafe {
            table
                .map_shared_page(0, shared.0.as_mut_ptr() as u64, false)
                .unwrap()
        };
        assert_eq!(level1.0[0], shared.0.as_mut_ptr() as u64 | (6 << 3) | 1);
        unsafe { table.restore_owned_page(0).unwrap() };
        assert_eq!(level1.0[0], resources.guest_base | EPT_LEAF_WRITE_BACK);
        unsafe {
            table
                .map_device_page(0, shared.0.as_mut_ptr() as u64)
                .unwrap()
        };
        assert_eq!(
            level1.0[0],
            shared.0.as_mut_ptr() as u64 | EPT_DEVICE_READ_WRITE
        );
        assert_eq!(
            table.hardware_root() & 0xfff,
            EPT_WRITE_BACK | EPT_WALK_LENGTH_4
        );
    }

    #[test]
    fn npt_uses_normal_page_table_permissions() {
        let mut root = Page([0; ENTRY_COUNT]);
        let mut level3 = Page([0; ENTRY_COUNT]);
        let mut level2 = Page([0; ENTRY_COUNT]);
        let mut level1 = Page([0; ENTRY_COUNT]);
        let mut device_level1 = Page([0; ENTRY_COUNT]);
        let mut device_state = Page([0; ENTRY_COUNT]);
        let mut guest = Page([0; ENTRY_COUNT]);
        let resources = resources(
            &mut root,
            &mut level3,
            &mut level2,
            &mut level1,
            &mut device_level1,
            &mut device_state,
            &mut guest,
        );

        // SAFETY: Test pages are aligned, live, writable, and uniquely borrowed.
        let table = unsafe { NestedPageTable::initialize(BackendKind::AmdSvm, resources) }.unwrap();
        assert_eq!(root.0[0], resources.level3 | NPT_PRESENT_WRITE_USER);
        assert_eq!(table.hardware_root(), resources.root);
        let mut shared = Page([0; ENTRY_COUNT]);
        unsafe {
            table
                .map_shared_page(0, shared.0.as_mut_ptr() as u64, true)
                .unwrap()
        };
        assert_eq!(
            level1.0[0],
            shared.0.as_mut_ptr() as u64 | NPT_PRESENT_USER_NO_EXECUTE | (1 << 1)
        );
        unsafe {
            table
                .map_device_page(0, shared.0.as_mut_ptr() as u64)
                .unwrap()
        };
        assert_eq!(
            level1.0[0],
            shared.0.as_mut_ptr() as u64 | NPT_DEVICE_READ_WRITE
        );
    }

    #[test]
    fn mappings_cross_a_two_megabyte_leaf_table_boundary() {
        let mut leaves = TwoPages([0; ENTRY_COUNT * 2]);
        let table = NestedPageTable::test_new(
            BackendKind::IntelVmx,
            leaves.0.as_mut_ptr() as u64,
            0x20_0000,
            ENTRY_COUNT + 1,
        );
        unsafe { table.map_device_page(2 * 1024 * 1024, 0x40_0000).unwrap() };
        assert_eq!(leaves.0[ENTRY_COUNT], 0x40_0000 | EPT_DEVICE_READ_WRITE);
    }

    #[test]
    fn sparse_gpu_ranges_use_large_and_small_device_leaves() {
        let mut root = Page([0; ENTRY_COUNT]);
        let mut level3 = Page([0; ENTRY_COUNT]);
        let mut level2 = Page([0; ENTRY_COUNT]);
        let mut level1 = Page([0; ENTRY_COUNT]);
        let mut device_level1 = Page([0; ENTRY_COUNT]);
        let mut device_state = Page([0; ENTRY_COUNT]);
        let mut guest = Page([0; ENTRY_COUNT]);
        let resources = resources(
            &mut root,
            &mut level3,
            &mut level2,
            &mut level1,
            &mut device_level1,
            &mut device_state,
            &mut guest,
        );
        let table =
            unsafe { NestedPageTable::initialize(BackendKind::IntelVmx, resources) }.unwrap();

        unsafe { table.map_device_range(0x20_0000, 0x40_0000, 0x20_1000) }.unwrap();
        assert_eq!(
            level2.0[1],
            0x40_0000 | EPT_DEVICE_READ_WRITE | NESTED_LARGE_PAGE
        );
        assert_ne!(level2.0[2] & 0x000f_ffff_ffff_f000, 0);
        assert_eq!(device_level1.0[0], 0x60_0000 | EPT_DEVICE_READ_WRITE);

        unsafe { table.unmap_device_range(0x20_0000, 0x20_1000) }.unwrap();
        assert_eq!(level2.0[1], 0);
        assert_eq!(device_level1.0[0], 0);
    }

    fn resources(
        root: &mut Page,
        level3: &mut Page,
        level2: &mut Page,
        level1: &mut Page,
        device_level1: &mut Page,
        device_state: &mut Page,
        guest: &mut Page,
    ) -> NestedPageResources {
        NestedPageResources {
            root: root.0.as_mut_ptr() as u64,
            level3: level3.0.as_mut_ptr() as u64,
            level3_pages: 1,
            level2: level2.0.as_mut_ptr() as u64,
            level2_pages: 1,
            level1: level1.0.as_mut_ptr() as u64,
            level1_pages: 1,
            device_level1: device_level1.0.as_mut_ptr() as u64,
            device_level1_pages: 1,
            device_state: device_state.0.as_mut_ptr() as u64,
            guest_base: guest.0.as_mut_ptr() as u64,
            guest_pages: 1,
        }
    }
}
