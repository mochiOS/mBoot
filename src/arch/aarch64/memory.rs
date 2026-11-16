//! Retained Stage-2 tables backed by mBoot-owned physical pages. The root and
//! child tables remain private to EL2; Domain RAM has separate Pages handles.
use super::stage2::{Config, Error, PageTable, TableMemory, PAGE_SIZE};
use crate::pages::{self, PagePool, Pages};

struct Child<'pool> {
    pages: Pages<'pool>,
    live: bool,
}

/// Dropping an arena does not recycle its physical backing: hardware walkers
/// may retain its addresses. Keep it with its Domain until retirement. Only
/// unpublished child tables are reusable through TableMemory::free_table.
pub struct Tables<'pool, const N: usize = 64> {
    root: Pages<'pool>,
    pool: &'pool PagePool,
    children: [Option<Child<'pool>>; N],
}

impl<'pool, const N: usize> Tables<'pool, N> {
    pub fn new(pool: &'pool PagePool, config: Config) -> Result<Self, pages::Error> {
        if pool.physical_limit() > 1u64 << config.physical_bits() {
            return Err(pages::Error::InvalidRange);
        }
        let root = pool.allocate(config.root_pages() as u64, config.root_alignment())?;
        if root.physical_start() == 0 {
            // SAFETY: This rejected root has never been exposed to a mapper,
            // vCPU, walker or device. Failure quarantines its backing pages.
            let _ = unsafe { root.release() };
            return Err(pages::Error::InvalidRange);
        }
        Ok(Self {
            root,
            pool,
            children: [const { None }; N],
        })
    }

    pub fn root(&self) -> u64 {
        self.root.physical_start()
    }

    /// # Safety
    /// Detach this address space from every CPU (including the selected
    /// VTTBR), stop all vCPUs/DMA/walkers and finish TLB/IOMMU/cache maintenance.
    /// No retained continuation, pointer or Rust reference may access these
    /// pages afterwards. The returned type cannot serve as TableMemory.
    pub unsafe fn retire(self) -> RetiredTables<'pool, N> {
        RetiredTables {
            root: Some(self.root),
            children: self.children.map(|child| child.map(|child| child.pages)),
        }
    }

    fn pointer(&self, frame: u64) -> Option<*mut PageTable> {
        if frame & (PAGE_SIZE - 1) != 0 {
            return None;
        }
        if let Some(offset) = frame.checked_sub(self.root.physical_start()) {
            if let Some(host) = self.root.host_address(offset, PAGE_SIZE) {
                return Some(host as *mut PageTable);
            }
        }
        self.children
            .iter()
            .flatten()
            .find(|child| child.live && child.pages.physical_start() == frame)
            .and_then(|child| child.pages.host_address(0, PAGE_SIZE))
            .map(|host| host as *mut PageTable)
    }
}

/// A caller-proven quiescent arena. Failed reclamation retains the remaining
/// handles for retry; dropping it leaves those pages quarantined in the pool.
pub struct RetiredTables<'pool, const N: usize> {
    root: Option<Pages<'pool>>,
    children: [Option<Pages<'pool>>; N],
}

impl<const N: usize> RetiredTables<'_, N> {
    pub fn reclaim(&mut self) -> Result<(), pages::Error> {
        for child in &mut self.children {
            reclaim_slot(child)?;
        }
        reclaim_slot(&mut self.root)
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none() && self.children.iter().all(Option::is_none)
    }
}

fn reclaim_slot(slot: &mut Option<Pages<'_>>) -> Result<(), pages::Error> {
    if let Some(pages) = slot.take() {
        // SAFETY: Only retire's proven quiescent handles reach this function.
        if let Err((error, pages)) = unsafe { pages.release() } {
            *slot = Some(pages);
            return Err(error);
        }
    }
    Ok(())
}

// SAFETY: Each handle uniquely retains aligned, pinned, zeroed Normal RAM.
// Root blocks satisfy Config's concatenation alignment; children are distinct
// pool allocations. Only a caller-proven unpublished child enters the private
// reuse cache, and no guest or DMA may access these EL2-owned pages.
unsafe impl<const N: usize> TableMemory for Tables<'_, N> {
    fn allocate_table(&mut self) -> Result<u64, Error> {
        if let Some(child) = self.children.iter_mut().flatten().find(|child| !child.live) {
            let frame = child.pages.physical_start();
            let host = child.pages.host_address(0, PAGE_SIZE).unwrap();
            // SAFETY: free_table admitted only unpublished tables; no parent
            // or walker can reference this cached slot while it is reset.
            unsafe { (host as *mut PageTable).write(PageTable::zeroed()) };
            child.live = true;
            return Ok(frame);
        }
        let slot = self
            .children
            .iter_mut()
            .find(|child| child.is_none())
            .ok_or(Error::OutOfMemory)?;
        let pages = self
            .pool
            .allocate(1, PAGE_SIZE)
            .map_err(|_| Error::OutOfMemory)?;
        let frame = pages.physical_start();
        if frame == 0 {
            // SAFETY: This page was never published. On failed reclamation it
            // stays claimed rather than becoming an invalid Stage-2 table.
            let _ = unsafe { pages.release() };
            return Err(Error::OutOfMemory);
        }
        *slot = Some(Child { pages, live: true });
        Ok(frame)
    }

    unsafe fn free_table(&mut self, frame: u64) {
        let child = self
            .children
            .iter_mut()
            .flatten()
            .find(|child| child.live && child.pages.physical_start() == frame)
            .expect("free_table requires an owned unpublished child");
        child.live = false;
    }

    unsafe fn table_ptr(&self, frame: u64) -> *mut PageTable {
        self.pointer(frame)
            .expect("table_ptr requires an owned live table frame")
    }
}
