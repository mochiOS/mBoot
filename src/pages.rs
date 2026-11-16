//! Boot RAM ownership after trusted firmware admission. Unlike the copyable
//! FDT layout, a pool and its allocation handles cannot be cloned or shared
//! between CPUs. Dropping a handle does not recycle hardware-visible memory.
use core::cell::RefCell;
use core::ptr::write_bytes;

use crate::fdt::memory::{MemoryMap, Region};

pub const PAGE_SIZE: u64 = 4096;
const MAX_RUNS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    UnresolvedMap,
    InvalidRange,
    InvalidLayout,
    OutOfMemory,
    MetadataFull,
    Busy,
}

struct FreeList {
    runs: [Option<Region>; MAX_RUNS],
    count: usize,
}

impl FreeList {
    fn insert(&mut self, mut region: Region) -> Result<(), Error> {
        let index =
            self.runs[..self.count].partition_point(|run| run.unwrap().start() < region.start());
        if index > 0 && self.runs[index - 1].unwrap().end() > region.start()
            || index < self.count && self.runs[index].unwrap().start() < region.end()
        {
            return Err(Error::InvalidRange);
        }
        if index > 0 && self.runs[index - 1].unwrap().end() == region.start() {
            let previous = self.runs[index - 1].unwrap();
            region = Region::new(previous.start(), region.end() - previous.start()).unwrap();
            self.runs[index - 1] = Some(region);
            if index < self.count && self.runs[index].unwrap().start() == region.end() {
                let end = self.runs[index].unwrap().end();
                self.runs[index - 1] =
                    Some(Region::new(region.start(), end - region.start()).unwrap());
                self.remove(index);
            }
        } else if index < self.count && self.runs[index].unwrap().start() == region.end() {
            self.runs[index] = Some(
                Region::new(
                    region.start(),
                    self.runs[index].unwrap().end() - region.start(),
                )
                .unwrap(),
            );
        } else {
            if self.count == MAX_RUNS {
                return Err(Error::MetadataFull);
            }
            self.runs.copy_within(index..self.count, index + 1);
            self.runs[index] = Some(region);
            self.count += 1;
        }
        Ok(())
    }

    fn remove(&mut self, index: usize) {
        self.runs.copy_within(index + 1..self.count, index);
        self.count -= 1;
        self.runs[self.count] = None;
    }

    fn take(&mut self, size: u64, alignment: u64) -> Result<Region, Error> {
        for index in 0..self.count {
            let run = self.runs[index].unwrap();
            let Some(start) = run
                .start()
                .checked_add(alignment - 1)
                .map(|v| v & !(alignment - 1))
            else {
                continue;
            };
            let Some(end) = start.checked_add(size).filter(|&end| end <= run.end()) else {
                continue;
            };
            // Preflight space for both fragments before changing the list.
            if start > run.start() && end < run.end() && self.count == MAX_RUNS {
                return Err(Error::MetadataFull);
            }
            self.remove(index);
            if start > run.start() {
                self.insert(Region::new(run.start(), start - run.start()).unwrap())
                    .unwrap();
            }
            if end < run.end() {
                self.insert(Region::new(end, run.end() - end).unwrap())
                    .unwrap();
            }
            return Ok(Region::new(start, size).unwrap());
        }
        Err(Error::OutOfMemory)
    }
}

pub struct PagePool {
    free: RefCell<FreeList>,
    linear_offset: u64,
    physical_limit: u64,
}

impl PagePool {
    /// Claim only the fully resolved, whole-page free ranges of a firmware plan.
    ///
    /// # Safety
    /// A trusted boot frontend must own these ranges exclusively and keep them
    /// as pinned Normal RW RAM for the pool and every outstanding allocation.
    /// Its live code, stacks, source images, DTB and all firmware/device/DMA
    /// reservations must already be excluded. No other allocator may claim any
    /// part of this RAM. Adding `linear_offset` must provide its actual CPU
    /// mapping, not a guessed physical-to-virtual conversion. `physical_limit`
    /// is the exclusive limit admitted by the CPU's real address-size features.
    pub unsafe fn claim(
        plan: &MemoryMap<'_>,
        physical_limit: u64,
        linear_offset: u64,
    ) -> Result<Self, Error> {
        let runs = plan.usable_pages().map_err(|_| Error::UnresolvedMap)?;
        // SAFETY: The frontend supplies the retained RAM ownership and mapping.
        unsafe { Self::from_regions(runs, physical_limit, linear_offset) }
    }

    /// # Safety
    /// As for claim; ranges must come from the admitted, fully reserved plan.
    unsafe fn from_regions(
        runs: impl Iterator<Item = Region>,
        physical_limit: u64,
        linear_offset: u64,
    ) -> Result<Self, Error> {
        if !physical_limit.is_power_of_two()
            || physical_limit < PAGE_SIZE
            || linear_offset & (PAGE_SIZE - 1) != 0
        {
            return Err(Error::InvalidRange);
        }
        let mut free = FreeList {
            runs: [None; MAX_RUNS],
            count: 0,
        };
        for run in runs {
            let host = run
                .start()
                .checked_add(linear_offset)
                .ok_or(Error::InvalidRange)?;
            let end = run
                .end()
                .checked_add(linear_offset)
                .ok_or(Error::InvalidRange)?;
            if run.start() & (PAGE_SIZE - 1) != 0
                || run.size() & (PAGE_SIZE - 1) != 0
                || run.end() > physical_limit
                || host == 0
                || usize::try_from(host).is_err()
                || usize::try_from(end - 1).is_err()
            {
                return Err(Error::InvalidRange);
            }
            free.insert(run)?;
        }
        Ok(Self {
            free: RefCell::new(free),
            linear_offset,
            physical_limit,
        })
    }

    pub fn physical_limit(&self) -> u64 {
        self.physical_limit
    }

    pub fn available_bytes(&self) -> Result<u64, Error> {
        let free = self.free.try_borrow().map_err(|_| Error::Busy)?;
        Ok(free.runs[..free.count]
            .iter()
            .map(|r| r.unwrap().size())
            .sum())
    }

    /// Allocate a positive page count with power-of-two byte alignment. All
    /// bytes are zero before the unique handle becomes visible to a caller.
    pub fn allocate(&self, pages: u64, alignment: u64) -> Result<Pages<'_>, Error> {
        let size = pages
            .checked_mul(PAGE_SIZE)
            .filter(|&v| v != 0)
            .ok_or(Error::InvalidLayout)?;
        if !alignment.is_power_of_two() || alignment < PAGE_SIZE {
            return Err(Error::InvalidLayout);
        }
        let region = self
            .free
            .try_borrow_mut()
            .map_err(|_| Error::Busy)?
            .take(size, alignment)?;
        let host = region.start() + self.linear_offset;
        // SAFETY: Claim validated this full CPU mapping and exclusive pinned
        // Normal RAM. take removed it atomically; no other handle can alias it.
        unsafe { write_bytes(host as *mut u8, 0, size as usize) };
        Ok(Pages { pool: self, region })
    }
}

/// Unique retained backing pages. Raw address access does not create Rust
/// references across vCPU/DMA execution. Drop deliberately leaves them claimed;
/// hardware references must be retired explicitly before reclamation.
#[must_use]
pub struct Pages<'pool> {
    pool: &'pool PagePool,
    region: Region,
}

impl Pages<'_> {
    pub fn physical_start(&self) -> u64 {
        self.region.start()
    }
    pub fn size(&self) -> u64 {
        self.region.size()
    }
    pub fn physical_address(&self, offset: u64, length: u64) -> Option<u64> {
        (offset.checked_add(length)? <= self.size()).then(|| self.physical_start() + offset)
    }
    pub fn host_address(&self, offset: u64, length: u64) -> Option<u64> {
        self.physical_address(offset, length)?
            .checked_add(self.pool.linear_offset)
    }

    /// # Safety
    /// Stop all vCPUs/DMA/walkers that could reference these pages, revoke all
    /// mappings and finish the required TLB/IOMMU/cache invalidations first.
    /// No live Rust reference or retained hardware address may access them
    /// afterwards. A failed return retains the handle for retry/quarantine.
    pub unsafe fn release(self) -> Result<(), (Error, Self)> {
        let result = match self.pool.free.try_borrow_mut() {
            Ok(mut free) => free.insert(self.region),
            Err(_) => Err(Error::Busy),
        };
        result.map_err(|error| (error, self))
    }
}

// SAFETY: A Pages handle uniquely retains bounded, pinned RAM from the claimed
// pool. The unsafe loader still excludes running vCPUs/DMA and source aliases.
unsafe impl crate::image::ImageMemory for Pages<'_> {
    fn guest_memory_size(&self) -> u64 {
        self.size()
    }
    fn guest_host_address(&self, address: u64, length: u64) -> Option<u64> {
        self.host_address(address, length)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::alloc::{alloc, dealloc, Layout};

    struct Backing {
        pointer: *mut u8,
        layout: Layout,
    }
    impl Backing {
        fn new() -> Self {
            let layout = Layout::from_size_align(64 * 4096, 65536).unwrap();
            // SAFETY: Retained allocation, explicitly checked before use.
            let pointer = unsafe { alloc(layout) };
            assert!(!pointer.is_null());
            // SAFETY: Exclusive allocated bytes, to verify zero on delegation.
            unsafe { write_bytes(pointer, 0xa5, layout.size()) };
            Self { pointer, layout }
        }
        fn pool(&self) -> PagePool {
            let region = Region::new(0x1000_1000, self.layout.size() as u64).unwrap();
            let offset = self.pointer as u64 - region.start();
            // SAFETY: Fake physical addresses map only this test's retained
            // private backing. No hardware, other pool, or immutable source
            // aliases it. All handles are retired before Backing is dropped.
            unsafe { PagePool::from_regions(core::iter::once(region), 1 << 40, offset).unwrap() }
        }
    }
    impl Drop for Backing {
        fn drop(&mut self) {
            // SAFETY: Test scopes end after all pool/handle uses.
            unsafe { dealloc(self.pointer, self.layout) };
        }
    }

    #[test]
    fn allocations_have_distinct_physical_and_host_addresses_and_start_zeroed() {
        let backing = Backing::new();
        let pool = backing.pool();
        let before = pool.available_bytes().unwrap();
        let a = pool.allocate(2, 16384).unwrap();
        let b = pool.allocate(3, PAGE_SIZE).unwrap();
        assert_eq!(a.physical_start() & 16383, 0);
        assert_ne!(a.physical_start(), a.host_address(0, 1).unwrap());
        assert!(
            a.physical_start() + a.size() <= b.physical_start()
                || b.physical_start() + b.size() <= a.physical_start()
        );
        assert_eq!(pool.available_bytes().unwrap(), before - 5 * PAGE_SIZE);
        // SAFETY: Stopped test buffers, disjoint from all Rust allocations.
        assert!(unsafe {
            core::slice::from_raw_parts(
                a.host_address(0, a.size()).unwrap() as *const u8,
                a.size() as usize,
            )
        }
        .iter()
        .all(|&v| v == 0));
        assert_eq!(a.host_address(a.size(), 1), None);
        assert_eq!(a.physical_address(u64::MAX, 2), None);
    }

    #[test]
    fn invalid_or_unfulfillable_allocations_leave_the_pool_unchanged() {
        let backing = Backing::new();
        let pool = backing.pool();
        let before = pool.available_bytes().unwrap();
        for (pages, alignment) in [
            (0, PAGE_SIZE),
            (u64::MAX, PAGE_SIZE),
            (1, 3),
            (1, 2048),
            (65, PAGE_SIZE),
        ] {
            assert!(pool.allocate(pages, alignment).is_err());
            assert_eq!(pool.available_bytes().unwrap(), before);
        }
        let invalid = Region::new(0x1000_0000, 4096).unwrap();
        // SAFETY: Invalid PA bounds fail before constructing/dereferencing RAM.
        assert!(matches!(
            unsafe { PagePool::from_regions(core::iter::once(invalid), 1 << 28, 0) },
            Err(Error::InvalidRange)
        ));
    }

    #[test]
    fn only_explicit_retirement_reclaims_pages_and_redelegation_clears_them() {
        let backing = Backing::new();
        let pool = backing.pool();
        let before = pool.available_bytes().unwrap();
        let leaked = pool.allocate(1, PAGE_SIZE).unwrap();
        drop(leaked);
        assert_eq!(pool.available_bytes().unwrap(), before - PAGE_SIZE);
        let page = pool.allocate(1, PAGE_SIZE).unwrap();
        let physical = page.physical_start();
        // SAFETY: No guest, mapping, DMA or Rust reference accesses this buffer.
        unsafe {
            write_bytes(
                page.host_address(0, PAGE_SIZE).unwrap() as *mut u8,
                0x5a,
                4096,
            )
        };
        // SAFETY: Never published; there are no hardware references to retire.
        assert!(unsafe { page.release() }.is_ok());
        let again = pool.allocate(1, PAGE_SIZE).unwrap();
        assert_eq!(again.physical_start(), physical);
        // SAFETY: Retained stopped buffer, no other reference exists.
        assert_eq!(
            unsafe { (again.host_address(0, 1).unwrap() as *const u8).read() },
            0
        );
    }
}
