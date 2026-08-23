pub mod cpu;
pub mod descriptor;
mod instructions;
pub mod svm;
pub mod vmx;

pub(crate) use instructions::{read_cr0, read_cr4, read_msr, write_cr0, write_cr4, write_msr};
