pub mod cpu;
pub mod descriptor;
mod instructions;
pub mod svm;
pub mod timer;
pub mod vmx;

// Guest CPUID exposes x87/SSE, not XSAVE/AVX. Each vCPU owns its legacy state;
// the assembly entry bridges save host state before loading guest state.
#[derive(Clone, Copy, Debug)]
#[repr(C, align(16))]
pub(crate) struct FxState([u8; 512]);

impl Default for FxState {
    fn default() -> Self {
        let mut state = Self([0; 512]);
        state.0[..2].copy_from_slice(&0x037fu16.to_le_bytes());
        state.0[24..28].copy_from_slice(&0x1f80u32.to_le_bytes());
        state
    }
}

pub(crate) use instructions::{
    read_cr0, read_cr3, read_cr4, read_msr, write_cr0, write_cr4, write_msr,
};
