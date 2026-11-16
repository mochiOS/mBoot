//! Real boot-RAM allocations through common Domain ownership and native
//! Stage-2 tables. The two controlled EL1 programs test memory isolation;
//! they are not production applications or a native Domain dispatcher.
use super::*;
use mboot::domain::{Domain, DomainId, DomainRole};

pub(super) fn run(config: Config, pool: &pages::PagePool) {
    check_table_rollback(config, pool);
    let domains = [
        Domain::new(
            DomainId::new(8),
            DomainRole::Application,
            0,
            mboot::BackendKind::ArmEl2,
            pool.allocate(64, PAGE_SIZE).expect("Domain A RAM"),
        ),
        Domain::new(
            DomainId::new(9),
            DomainRole::Application,
            0,
            mboot::BackendKind::ArmEl2,
            pool.allocate(64, PAGE_SIZE).expect("Domain B RAM"),
        ),
    ];
    let mut tables_a = native_memory::Tables::<8>::new(pool, config).expect("Domain A tables");
    let mut tables_b = native_memory::Tables::<8>::new(pool, config).expect("Domain B tables");
    let private_roots = [tables_a.root(), tables_b.root()];
    let tables = [&mut tables_a, &mut tables_b];
    let entry = CODE + guest_read as *const () as u64 - addr_of!(__guest_start) as u64;
    for i in 0..2 {
        let ram = domains[i].nested_pages();
        let other = domains[1 - i].nested_pages();
        check(
            ram.physical_start() + ram.size() <= other.physical_start()
                || other.physical_start() + other.size() <= ram.physical_start(),
            "Domain RAM aliases",
        );
        check(
            ram.physical_address(DATA, PAGE_SIZE).is_some(),
            "owned GPA bounds",
        );
        // SAFETY: Private stopped RAM, no references/guest/DMA accesses.
        unsafe { (ram.host_address(DATA, 8).unwrap() as *mut u64).write(0xabc0 + i as u64) };
        let root = private_roots[i];
        // SAFETY: Retained private root/children, unique VMIDs, no active guest.
        let mut mapper =
            unsafe { Mapper::new(&mut *tables[i], root, config, 8 + i as u8).unwrap() };
        select(&mapper);
        map_fixture_code(&mut mapper);
        // SAFETY: Only the exact Domain's retained pages back these GPAs.
        unsafe {
            mapper
                .map_page(
                    STACK,
                    ram.physical_address(STACK, PAGE_SIZE).unwrap(),
                    READ_WRITE,
                )
                .unwrap();
            mapper
                .map_page(
                    DATA,
                    ram.physical_address(DATA, PAGE_SIZE).unwrap(),
                    READ_WRITE,
                )
                .unwrap();
        }
        let mut guest = match vcpu::Vcpu::new(&mapper, entry, DATA, i as u64) {
            Ok(guest) => guest,
            Err(vcpu::Error::UnsupportedCpu) => {
                emit("Owned boot RAM allocated; vCPU register bank unsupported on this CPU\n");
                return;
            }
            Err(_) => panic!("owned vCPU admission"),
        };
        // SAFETY: Controlled reference code, exclusive retained backing,
        // no DMA/controller assignment or optional virtual execution engine.
        let exit = unsafe { guest.run(&mapper).unwrap() };
        check(
            exit.kind == vcpu::ExitKind::Synchronous
                && exit.esr >> 26 == 0x16
                && guest.register(0) == Some(0xabc0 + i as u64),
            "owned Domain read",
        );
        // A foreign Domain's physical backing and both private roots are not
        // capabilities in this guest's address space, even if their PA is known.
        for forbidden in [other.physical_start(), private_roots[0], private_roots[1]] {
            let mut probe = vcpu::Vcpu::new(&mapper, entry, forbidden, i as u64).unwrap();
            // SAFETY: Same stopped, retained owned context; no MMIO is exposed.
            let exit = unsafe { probe.run(&mapper).unwrap() };
            abort(
                Exit {
                    esr: exit.esr,
                    far: exit.far,
                    hpfar: exit.hpfar,
                    value: 0,
                },
                forbidden,
                false,
                false,
                false,
            );
        }
    }
    emit("Owned boot RAM / Domain isolation / private Stage-2 table denial PASS\n");
}

fn check_table_rollback(config: Config, pool: &pages::PagePool) {
    let backing = pool.allocate(1, PAGE_SIZE).unwrap();
    let mut tables = native_memory::Tables::<1>::new(pool, config).unwrap();
    let root = tables.root();
    let before = pool.available_bytes().unwrap();
    {
        // SAFETY: Retained private root and backing, distinct VMID, no guest.
        let mut mapper = unsafe { Mapper::new(&mut tables, root, config, 10).unwrap() };
        select(&mapper);
        // SAFETY: Only owned RAM is offered. This two-child walk must fail in
        // the one-slot arena before publishing a parent, and roll back its child.
        check(
            unsafe { mapper.map_page(DATA, backing.physical_start(), READ_WRITE) }
                == Err(Stage2Error::OutOfMemory),
            "owned table allocation did not fail atomically",
        );
        // SAFETY: No guest has run; select suppressed both EL1 walk ranges.
        unsafe { mapper.deactivate().expect("detach owned table root") };
    }
    let frame = tables.allocate_table().expect("reuse unpublished table");
    check(
        pool.available_bytes() == Ok(before - PAGE_SIZE),
        "rollback leaked another table allocation",
    );
    // SAFETY: This child was never published. No walker/guest/DMA sees it.
    unsafe {
        (tables.table_ptr(frame) as *mut u64).write(0xdead_beef);
        tables.free_table(frame);
    }
    let again = tables.allocate_table().unwrap();
    check(again == frame, "unpublished child was not reused");
    // SAFETY: Retained exclusively owned child, still unpublished.
    check(
        unsafe { (tables.table_ptr(again) as *const u64).read() } == 0,
        "cached Stage-2 table retained stale descriptors",
    );
    // SAFETY: The only editing CPU detached VTTBR/VMID above; no guest, DMA,
    // other CPU, hardware walker or retained continuation references this arena.
    let mut retired = unsafe { tables.retire() };
    retired.reclaim().expect("return retired private tables");
    check(retired.is_empty(), "retired table handles were lost");
    check(
        pool.available_bytes() == Ok(before + config.root_pages() as u64 * PAGE_SIZE),
        "retired table pages were not reclaimed",
    );
    emit("Owned Stage-2 table rollback / zeroed private reuse PASS\n");
    emit("Detached Stage-2 root / explicit table reclamation PASS\n");
}
