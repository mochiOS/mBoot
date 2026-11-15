//! Restricted, CPU-local SMC access after trusted firmware conduit discovery.
//! This module does not forward arbitrary guest function IDs, addresses, power
//! requests or CPU-start requests to firmware. Domain authority stays with the
//! hypervisor dispatcher, and CPU mitigation policy is separate from transport.
use core::arch::asm;
use core::marker::PhantomData;

const PSCI_VERSION: u64 = 0x8400_0000;
const ARCH_FEATURES: u64 = 0x8000_0001;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Workaround {
    BranchPredictor,
    BranchHistory,
}

impl Workaround {
    fn function(self) -> u64 {
        match self {
            Self::BranchPredictor => 0x8000_8000,
            Self::BranchHistory => 0x8000_3fff,
        }
    }
    fn index(self) -> usize {
        match self {
            Self::BranchPredictor => 0,
            Self::BranchHistory => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidContext,
    WrongCpu,
    MissingPsci,
    UnsupportedWorkaround,
}

/// A retained conduit belongs to one physical CPU. It cannot be cloned or
/// moved to another CPU by a safe scheduler. Features are raw SMCCC32 results:
/// an unsupported call remains -1, never an invented "unaffected" response.
pub struct Firmware {
    owner: u64,
    psci_version: u32,
    features: [i32; 2],
    _local: PhantomData<*mut ()>,
}

impl Firmware {
    /// # Safety
    /// Trusted platform discovery must have established a live, standard SMC
    /// conduit implementing PSCI 0.2 or later. An arbitrary DTB supplied by a
    /// guest is not that evidence. Firmware preserves the nonvolatile AAPCS
    /// state and returns to this non-VHE EL2 caller. All DAIF masks remain set;
    /// execution must stay on this CPU for the lifetime of the handle.
    pub unsafe fn from_smc_conduit() -> Result<Self, Error> {
        let owner = current_cpu()?;
        // SAFETY: The unsafe constructor supplies the trusted SMC contract.
        let version = unsafe { call(PSCI_VERSION, 0) } as u32;
        if (version as i32) < 0 || version < 2 {
            return Err(Error::MissingPsci);
        }
        let mut features = [0; 2];
        for workaround in [Workaround::BranchPredictor, Workaround::BranchHistory] {
            // SAFETY: Only architected feature queries, no pointers or guest
            // values are passed to secure firmware. Query on the owned CPU.
            features[workaround.index()] =
                unsafe { call(ARCH_FEATURES, workaround.function()) } as u32 as i32;
        }
        Ok(Self {
            owner,
            psci_version: version,
            features,
            _local: PhantomData,
        })
    }

    pub fn psci_version(&self) -> u32 {
        self.psci_version
    }

    /// Report the exact signed-32-bit firmware result. This alone does not
    /// prove a CPU is safe: the entry owner must also apply CPU-specific and
    /// architectural requirements before admitting an untrusted Domain.
    pub fn architectural_features(&self, workaround: Workaround) -> Result<i32, Error> {
        self.validate()?;
        Ok(self.features[workaround.index()])
    }

    /// Invoke only an explicitly advertised workaround. WA1/WA3 have no defined
    /// return value; the feature result, not an accidental X0 value on return,
    /// controls availability. A caller receiving status 1 needs no invocation.
    pub fn apply(&self, workaround: Workaround) -> Result<(), Error> {
        self.validate()?;
        if self.features[workaround.index()] != 0 {
            return Err(Error::UnsupportedWorkaround);
        }
        // SAFETY: This retained, trusted conduit advertised the exact function
        // on the same physical CPU. It takes no guest pointers or arguments.
        unsafe {
            call(workaround.function(), 0);
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), Error> {
        if current_cpu()? != self.owner {
            return Err(Error::WrongCpu);
        }
        Ok(())
    }
}

unsafe fn call(function: u64, argument: u64) -> u64 {
    let result;
    // SAFETY: Callers admit the physical conduit and whitelist both IDs and
    // arguments. AAPCS volatile GPR/SIMD clobbers and memory effects are visible
    // to Rust; no live guest registers are passed through this host call.
    unsafe {
        asm!("smc #0", inout("x0") function => result, inout("x1") argument => _,
            inout("x2") 0u64 => _, inout("x3") 0u64 => _, clobber_abi("C"));
    }
    result
}

fn current_cpu() -> Result<u64, Error> {
    let level: u64;
    let daif: u64;
    // SAFETY: Read-only current context before selecting any EL2-only bank.
    unsafe {
        asm!("mrs {}, CurrentEL", "mrs {}, daif", out(reg) level, out(reg) daif,
        options(nomem, nostack, preserves_flags));
    }
    if level != 8 || daif & 0x3c0 != 0x3c0 {
        return Err(Error::InvalidContext);
    }
    let hcr: u64;
    let owner: u64;
    // SAFETY: Validated EL2. No virtual MPIDR alias or VHE EL1 bank is used.
    unsafe {
        asm!("mrs {}, hcr_el2", "mrs {}, mpidr_el1", out(reg) hcr, out(reg) owner,
        options(nomem, nostack, preserves_flags));
    }
    if hcr & ((1 << 34) | (1 << 27)) != 0 {
        return Err(Error::InvalidContext);
    }
    Ok(owner)
}
