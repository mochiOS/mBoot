use core::ptr::write_bytes;

use crate::{BackendKind, Error};

const PAGE_SIZE: u64 = 4096;
const ENTRY_COUNT: usize = 512;
const EPT_READ_WRITE_EXECUTE: u64 = 0b111;
const EPT_LEAF_WRITE_BACK: u64 = EPT_READ_WRITE_EXECUTE | (6 << 3);
const EPT_WRITE_BACK: u64 = 6;
const EPT_WALK_LENGTH_4: u64 = 3 << 3;
const NPT_PRESENT_WRITE_USER: u64 = 0b111;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NestedPageResources {
    pub root: u64,
    pub level3: u64,
    pub level2: u64,
    pub level1: u64,
    pub guest_page: u64,
}

impl NestedPageResources {
    pub fn validate(self) -> Result<(), Error> {
        for page in [
            self.root,
            self.level3,
            self.level2,
            self.level1,
            self.guest_page,
        ] {
            if page == 0 || page & (PAGE_SIZE - 1) != 0 {
                return Err(Error::InvalidPage);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NestedPageTable {
    backend: BackendKind,
    root: u64,
    guest_page: u64,
}

impl NestedPageTable {
    /// Builds a four-level translation that maps guest physical page zero to
    /// one mBoot-owned host page. No other guest physical address is mapped.
    ///
    /// # Safety
    /// Every address in `resources` must be identity-mapped, writable, and
    /// exclusively owned by mBoot for the lifetime of the returned table.
    pub unsafe fn initialize(
        backend: BackendKind,
        resources: NestedPageResources,
    ) -> Result<Self, Error> {
        resources.validate()?;
        for page in [
            resources.root,
            resources.level3,
            resources.level2,
            resources.level1,
        ] {
            // SAFETY: The function contract gives mBoot exclusive writable ownership.
            unsafe { write_bytes(page as *mut u8, 0, PAGE_SIZE as usize) };
        }

        let link_flags = match backend {
            BackendKind::IntelVmx => EPT_READ_WRITE_EXECUTE,
            BackendKind::AmdSvm => NPT_PRESENT_WRITE_USER,
        };
        // SAFETY: All tables were validated, zeroed, and are exclusively owned.
        unsafe {
            write_entry(resources.root, 0, resources.level3 | link_flags);
            write_entry(resources.level3, 0, resources.level2 | link_flags);
            write_entry(resources.level2, 0, resources.level1 | link_flags);
            let leaf_flags = match backend {
                BackendKind::IntelVmx => EPT_LEAF_WRITE_BACK,
                BackendKind::AmdSvm => NPT_PRESENT_WRITE_USER,
            };
            write_entry(resources.level1, 0, resources.guest_page | leaf_flags);
            write_bytes(resources.guest_page as *mut u8, 0, PAGE_SIZE as usize);
        }

        Ok(Self {
            backend,
            root: resources.root,
            guest_page: resources.guest_page,
        })
    }

    pub const fn root(&self) -> u64 {
        self.root
    }

    pub const fn guest_page(&self) -> u64 {
        self.guest_page
    }

    pub const fn hardware_root(&self) -> u64 {
        match self.backend {
            BackendKind::IntelVmx => self.root | EPT_WRITE_BACK | EPT_WALK_LENGTH_4,
            BackendKind::AmdSvm => self.root,
        }
    }

    #[cfg(test)]
    pub(crate) const fn test_new(backend: BackendKind, root: u64, guest_page: u64) -> Self {
        Self {
            backend,
            root,
            guest_page,
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

    #[test]
    fn ept_maps_only_guest_page_zero() {
        let mut root = Page([0; ENTRY_COUNT]);
        let mut level3 = Page([0; ENTRY_COUNT]);
        let mut level2 = Page([0; ENTRY_COUNT]);
        let mut level1 = Page([0; ENTRY_COUNT]);
        let mut guest = Page([u64::MAX; ENTRY_COUNT]);
        let resources = resources(&mut root, &mut level3, &mut level2, &mut level1, &mut guest);

        // SAFETY: Test pages are aligned, live, writable, and uniquely borrowed.
        let table =
            unsafe { NestedPageTable::initialize(BackendKind::IntelVmx, resources) }.unwrap();
        assert_eq!(root.0[0], resources.level3 | EPT_READ_WRITE_EXECUTE);
        assert_eq!(level1.0[0], resources.guest_page | EPT_LEAF_WRITE_BACK);
        assert_eq!(level1.0[1], 0);
        assert_eq!(guest.0[0], 0);
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
        let mut guest = Page([0; ENTRY_COUNT]);
        let resources = resources(&mut root, &mut level3, &mut level2, &mut level1, &mut guest);

        // SAFETY: Test pages are aligned, live, writable, and uniquely borrowed.
        let table = unsafe { NestedPageTable::initialize(BackendKind::AmdSvm, resources) }.unwrap();
        assert_eq!(root.0[0], resources.level3 | NPT_PRESENT_WRITE_USER);
        assert_eq!(table.hardware_root(), resources.root);
    }

    fn resources(
        root: &mut Page,
        level3: &mut Page,
        level2: &mut Page,
        level1: &mut Page,
        guest: &mut Page,
    ) -> NestedPageResources {
        NestedPageResources {
            root: root.0.as_mut_ptr() as u64,
            level3: level3.0.as_mut_ptr() as u64,
            level2: level2.0.as_mut_ptr() as u64,
            level1: level1.0.as_mut_ptr() as u64,
            guest_page: guest.0.as_mut_ptr() as u64,
        }
    }
}
