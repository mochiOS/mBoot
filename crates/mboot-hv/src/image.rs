use core::mem::size_of;
use core::ptr::{copy_nonoverlapping, write_bytes};

use crate::memory::NestedPageTable;
use crate::Error;

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LITTLE_ENDIAN: u8 = 1;
const ELF_TYPE_EXECUTABLE: u16 = 2;
const ELF_MACHINE_X86_64: u16 = 0x3e;
const PT_LOAD: u32 = 1;
const MINIMUM_LOAD_GPA: u64 = 0x1_0000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestImage {
    entry: u64,
}

impl GuestImage {
    pub const fn entry(self) -> u64 {
        self.entry
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ElfHeader {
    ident: [u8; 16],
    file_type: u16,
    machine: u16,
    version: u32,
    entry: u64,
    program_header_offset: u64,
    section_header_offset: u64,
    flags: u32,
    header_size: u16,
    program_header_size: u16,
    program_header_count: u16,
    section_header_size: u16,
    section_header_count: u16,
    section_name_index: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ProgramHeader {
    segment_type: u32,
    flags: u32,
    offset: u64,
    virtual_address: u64,
    physical_address: u64,
    file_size: u64,
    memory_size: u64,
    alignment: u64,
}

/// Copies the loadable parts of a fixed-address x86_64 ELF into guest RAM.
///
/// # Safety
/// `memory` must retain exclusive writable ownership of its guest RAM while the
/// image is copied. No vCPU may be executing from it yet.
pub unsafe fn load_elf(image: &[u8], memory: &NestedPageTable) -> Result<GuestImage, Error> {
    let header: ElfHeader = read_struct(image, 0)?;
    if header.ident[..4] != ELF_MAGIC
        || header.ident[4] != ELF_CLASS_64
        || header.ident[5] != ELF_DATA_LITTLE_ENDIAN
        || header.file_type != ELF_TYPE_EXECUTABLE
        || header.machine != ELF_MACHINE_X86_64
        || header.program_header_size as usize != size_of::<ProgramHeader>()
    {
        return Err(Error::InvalidImage);
    }
    if header.entry < MINIMUM_LOAD_GPA || header.entry >= memory.guest_memory_size() {
        return Err(Error::ImageTooLarge);
    }

    let mut loaded = false;
    for index in 0..usize::from(header.program_header_count) {
        let offset = (header.program_header_offset as usize)
            .checked_add(
                index
                    .checked_mul(size_of::<ProgramHeader>())
                    .ok_or(Error::InvalidImage)?,
            )
            .ok_or(Error::InvalidImage)?;
        let segment: ProgramHeader = read_struct(image, offset)?;
        if segment.segment_type != PT_LOAD {
            continue;
        }
        if segment.physical_address != segment.virtual_address
            || segment.virtual_address < MINIMUM_LOAD_GPA
            || segment.file_size > segment.memory_size
        {
            return Err(Error::InvalidImage);
        }
        let source_start = segment.offset as usize;
        let source_end = source_start
            .checked_add(segment.file_size as usize)
            .ok_or(Error::InvalidImage)?;
        let source = image
            .get(source_start..source_end)
            .ok_or(Error::InvalidImage)?;
        let destination = memory
            .guest_host_address(segment.physical_address, segment.memory_size)
            .ok_or(Error::ImageTooLarge)?;

        // SAFETY: The source range was checked and the destination belongs to
        // unused guest RAM for the complete segment size.
        unsafe {
            write_bytes(destination as *mut u8, 0, segment.memory_size as usize);
            copy_nonoverlapping(source.as_ptr(), destination as *mut u8, source.len());
        }
        loaded = true;
    }
    if !loaded {
        return Err(Error::InvalidImage);
    }
    Ok(GuestImage {
        entry: header.entry,
    })
}

fn read_struct<T: Copy>(bytes: &[u8], offset: usize) -> Result<T, Error> {
    let end = offset
        .checked_add(size_of::<T>())
        .ok_or(Error::InvalidImage)?;
    let source = bytes.get(offset..end).ok_or(Error::InvalidImage)?;
    // SAFETY: The slice contains at least one complete T and unaligned reads are
    // used. ELF headers consist only of integer and byte-array fields.
    Ok(unsafe { source.as_ptr().cast::<T>().read_unaligned() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_elf_is_rejected() {
        let memory = NestedPageTable::test_new(crate::BackendKind::IntelVmx, 0x1000, 0x5000, 1);
        // SAFETY: Rejection happens before the synthetic host address is used.
        assert_eq!(
            unsafe { load_elf(&ELF_MAGIC, &memory) },
            Err(Error::InvalidImage)
        );
    }
}
