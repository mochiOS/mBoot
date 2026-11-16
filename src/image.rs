use core::mem::size_of;
use core::ptr::{copy_nonoverlapping, write_bytes};

#[cfg(target_arch = "x86_64")]
use crate::memory::NestedPageTable;
use crate::Error;

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LITTLE_ENDIAN: u8 = 1;
const ELF_TYPE_EXECUTABLE: u16 = 2;
const ELF_MACHINE_X86_64: u16 = 0x3e;
const PF_EXECUTE: u32 = 1;

#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ElfArchitecture {
    X86_64 = ELF_MACHINE_X86_64,
    Aarch64 = 183,
}

#[cfg(target_arch = "x86_64")]
const NATIVE_ELF: ElfArchitecture = ElfArchitecture::X86_64;
#[cfg(target_arch = "aarch64")]
const NATIVE_ELF: ElfArchitecture = ElfArchitecture::Aarch64;

/// Stable, exclusively retained guest RAM, independent of EPT/NPT/Stage-2.
///
/// # Safety
/// Every returned host range must remain backed by the same writable RAM for
/// the complete load operation. It must not alias hypervisor-private memory,
/// MMIO, another Domain, or the immutable source image. VCPUs/DMA cannot mutate
/// this RAM while a loader holds the caller's exclusive stopped-Domain access.
pub unsafe trait ImageMemory {
    fn guest_memory_size(&self) -> u64;
    fn guest_host_address(&self, address: u64, length: u64) -> Option<u64>;
}

// SAFETY: NestedPageTable exposes bounds-checked host ranges in its own retained
// Domain RAM. The unsafe loader caller excludes VCPU/DMA/source-image aliases.
#[cfg(target_arch = "x86_64")]
unsafe impl ImageMemory for NestedPageTable {
    fn guest_memory_size(&self) -> u64 {
        NestedPageTable::guest_memory_size(self)
    }
    fn guest_host_address(&self, address: u64, length: u64) -> Option<u64> {
        NestedPageTable::guest_host_address(self, address, length)
    }
}
const PT_LOAD: u32 = 1;
const PT_NOTE: u32 = 4;
const MINIMUM_LOAD_GPA: u64 = 0x1_0000;
const XEN_ELFNOTE_PHYS32_ENTRY: u32 = 18;
const PVH_START_INFO_GPA: u64 = 0x4000;
const PVH_MODULE_LIST_GPA: u64 = 0x4040;
const PVH_MEMORY_MAP_GPA: u64 = 0x4080;
const PVH_COMMAND_LINE_GPA: u64 = 0x40c0;
const PVH_COMMAND_LINE_LIMIT: usize = 1024;
const XEN_HVM_START_MAGIC_VALUE: u32 = 0x336e_c578;
const XEN_HVM_MEMMAP_TYPE_RAM: u32 = 1;
const XEN_HVM_MEMMAP_TYPE_RESERVED: u32 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestImage {
    entry: u64,
    boot_info: u64,
    loaded_end: u64,
}

impl GuestImage {
    pub const fn entry(self) -> u64 {
        self.entry
    }

    pub const fn boot_info(self) -> u64 {
        self.boot_info
    }

    pub const fn loaded_end(self) -> u64 {
        self.loaded_end
    }
}

/// Load a modern little-endian, 4 KiB AArch64 Linux Image at an explicitly
/// admitted 2 MiB base. This is the raw Image entry protocol, not EFI or PVH.
/// The caller supplies a separately validated guest DTB in x0 when entering.
///
/// # Safety
/// As for `load_elf`: the entire destination is retained, stopped and exclusive,
/// and the immutable source is disjoint. `base` describes guest RAM, not a host
/// pointer. The caller must reserve the resulting range against other payloads.
pub unsafe fn load_arm64_linux(
    image: &[u8],
    base: u64,
    memory: &impl ImageMemory,
) -> Result<GuestImage, Error> {
    let header = image.get(..64).ok_or(Error::InvalidImage)?;
    let field = |offset| u64::from_le_bytes(header[offset..offset + 8].try_into().unwrap());
    let offset = field(8);
    let size = field(16);
    let flags = field(24);
    // Unknown legacy sizes, big-endian kernels and other granules need their
    // own admitted profile. Never guess a load extent or execute compressed data.
    if header[56..60] != *b"ARM\x64"
        || size < image.len() as u64
        || flags & !8 != 2
        || field(32) != 0
        || field(40) != 0
        || field(48) != 0
        || base & (0x20_0000 - 1) != 0
        || offset & 3 != 0
    {
        return Err(Error::InvalidImage);
    }
    let start = base.checked_add(offset).ok_or(Error::ImageTooLarge)?;
    let end = start.checked_add(size).ok_or(Error::ImageTooLarge)?;
    if start < MINIMUM_LOAD_GPA || end > memory.guest_memory_size() {
        return Err(Error::ImageTooLarge);
    }
    let destination = memory
        .guest_host_address(start, size)
        .ok_or(Error::ImageTooLarge)?;
    let size = usize::try_from(size).map_err(|_| Error::ImageTooLarge)?;
    // SAFETY: Complete validation precedes writes into the caller-owned range.
    unsafe {
        write_bytes(destination as *mut u8, 0, size);
        copy_nonoverlapping(image.as_ptr(), destination as *mut u8, image.len());
    }
    Ok(GuestImage {
        entry: start,
        boot_info: 0,
        loaded_end: end,
    })
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HvmStartInfo {
    magic: u32,
    version: u32,
    flags: u32,
    nr_modules: u32,
    modlist_paddr: u64,
    cmdline_paddr: u64,
    rsdp_paddr: u64,
    memmap_paddr: u64,
    memmap_entries: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HvmModule {
    paddr: u64,
    size: u64,
    cmdline_paddr: u64,
    reserved: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HvmMemoryMapEntry {
    addr: u64,
    size: u64,
    kind: u32,
    reserved: u32,
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

/// Copies a native ELF into stopped guest RAM. Foreign CPUs are rejected.
///
/// # Safety
/// `memory` must retain exclusive writable ownership of its guest RAM while the
/// image is copied. No vCPU may be executing from it yet.
pub unsafe fn load_elf(image: &[u8], memory: &impl ImageMemory) -> Result<GuestImage, Error> {
    // SAFETY: Forward the caller's stopped, exclusively retained RAM contract.
    unsafe { load_elf_for(NATIVE_ELF, image, memory) }
}

/// Load an explicitly selected CPU image. ARM may use a low identity-mapped
/// bootstrap and upper-half VMAs whose bytes are stored at separate GPAs. The
/// signed bootstrap establishes Stage-1 before entering its upper-half code;
/// this byte loader never substitutes a virtual address for a physical range.
///
/// # Safety
/// As for `load_elf`: retain stopped, exclusive RAM and a disjoint immutable
/// source image. The eventual VCPU backend must match `architecture`.
pub unsafe fn load_elf_for(
    architecture: ElfArchitecture,
    image: &[u8],
    memory: &impl ImageMemory,
) -> Result<GuestImage, Error> {
    let header = elf_header_for(image, architecture)?;
    if header.entry < MINIMUM_LOAD_GPA || header.entry >= memory.guest_memory_size() {
        return Err(Error::ImageTooLarge);
    }
    if architecture == ElfArchitecture::Aarch64 && header.entry & 3 != 0 {
        return Err(Error::InvalidImage);
    }

    let mut loaded = false;
    let mut loaded_end = 0;
    let mut bootstrap_entry = false;
    // Validate the entire image before modifying any byte of guest RAM.
    for index in 0..usize::from(header.program_header_count) {
        let segment = program_header(image, header, index)?;
        if segment.segment_type != PT_LOAD {
            continue;
        }
        let identity = segment.physical_address == segment.virtual_address;
        if (!identity
            && (architecture != ElfArchitecture::Aarch64
                || segment.virtual_address < 0xffff_0000_0000_0000))
            || segment.physical_address < MINIMUM_LOAD_GPA
            || segment.file_size > segment.memory_size
        {
            return Err(Error::InvalidImage);
        }
        let source_start = usize::try_from(segment.offset).map_err(|_| Error::InvalidImage)?;
        let source_end = source_start
            .checked_add(usize::try_from(segment.file_size).map_err(|_| Error::InvalidImage)?)
            .ok_or(Error::InvalidImage)?;
        image
            .get(source_start..source_end)
            .ok_or(Error::InvalidImage)?;
        memory
            .guest_host_address(segment.physical_address, segment.memory_size)
            .ok_or(Error::ImageTooLarge)?;
        segment
            .virtual_address
            .checked_add(segment.memory_size)
            .ok_or(Error::InvalidImage)?;
        bootstrap_entry |= identity
            && segment.flags & PF_EXECUTE != 0
            && header.entry >= segment.virtual_address
            && header.entry
                < segment
                    .virtual_address
                    .checked_add(segment.file_size)
                    .ok_or(Error::InvalidImage)?;
        loaded_end = loaded_end.max(
            segment
                .physical_address
                .checked_add(segment.memory_size)
                .ok_or(Error::InvalidImage)?,
        );
        loaded = true;
    }
    if !loaded || !bootstrap_entry {
        return Err(Error::InvalidImage);
    }
    for index in 0..usize::from(header.program_header_count) {
        let segment = program_header(image, header, index)?;
        if segment.segment_type != PT_LOAD {
            continue;
        }
        let source_start = segment.offset as usize;
        let source = &image[source_start..source_start + segment.file_size as usize];
        let destination = memory
            .guest_host_address(segment.physical_address, segment.memory_size)
            .ok_or(Error::ImageTooLarge)?;
        // SAFETY: Preflight checked the complete source/destination ranges.
        // The caller and ImageMemory retain disjoint, stable, stopped RAM.
        unsafe {
            write_bytes(destination as *mut u8, 0, segment.memory_size as usize);
            copy_nonoverlapping(source.as_ptr(), destination as *mut u8, source.len());
        }
    }
    Ok(GuestImage {
        entry: header.entry,
        boot_info: 0,
        loaded_end,
    })
}

/// Places an authenticated boot module below `limit` in stopped Domain RAM.
///
/// The returned address is page aligned. `image_end` must be the first byte
/// after the native ELF image so the module cannot overwrite its loadable
/// segments.
///
/// # Safety
/// The Domain must be stopped and `memory` must be exclusively writable.
pub unsafe fn load_boot_module(
    module: &[u8],
    image_end: u64,
    limit: u64,
    memory: &impl ImageMemory,
) -> Result<(u64, u64), Error> {
    if module.is_empty() || limit > memory.guest_memory_size() {
        return Err(Error::InvalidImage);
    }
    let size = u64::try_from(module.len()).map_err(|_| Error::ImageTooLarge)?;
    let start = limit
        .checked_sub(size)
        .map(|address| address & !0xfff)
        .ok_or(Error::ImageTooLarge)?;
    if start < image_end || start < MINIMUM_LOAD_GPA {
        return Err(Error::ImageTooLarge);
    }
    let destination = memory
        .guest_host_address(start, size)
        .ok_or(Error::ImageTooLarge)?;
    // SAFETY: The bounds and stopped-Domain ownership were checked above.
    unsafe { copy_nonoverlapping(module.as_ptr(), destination as *mut u8, module.len()) };
    Ok((start, size))
}

/// Loads an x86 Linux PVH ELF and constructs the version 1 start-info block.
/// The first module is exposed as the initramfs, as required by the PVH ABI.
///
/// # Safety
/// `memory` must retain exclusive writable ownership of stopped guest RAM.
pub unsafe fn load_linux_pvh(
    kernel: &[u8],
    initramfs: Option<&[u8]>,
    command_line: &str,
    rsdp: u64,
    usable_memory_size: u64,
    memory: &impl ImageMemory,
) -> Result<GuestImage, Error> {
    if command_line.len() + 1 > PVH_COMMAND_LINE_LIMIT || !command_line.is_ascii() {
        return Err(Error::InvalidImage);
    }
    if usable_memory_size < MINIMUM_LOAD_GPA
        || usable_memory_size & 0xfff != 0
        || usable_memory_size >= memory.guest_memory_size()
    {
        return Err(Error::InvalidImage);
    }
    let header = elf_header(kernel)?;
    let entry = pvh_entry(kernel, header)?;
    if entry < MINIMUM_LOAD_GPA || entry >= usable_memory_size {
        return Err(Error::ImageTooLarge);
    }

    let mut loaded = false;
    let mut load_end = 0u64;
    for index in 0..usize::from(header.program_header_count) {
        let segment = program_header(kernel, header, index)?;
        if segment.segment_type != PT_LOAD {
            continue;
        }
        if segment.physical_address < MINIMUM_LOAD_GPA || segment.file_size > segment.memory_size {
            return Err(Error::InvalidImage);
        }
        let source_start = usize::try_from(segment.offset).map_err(|_| Error::InvalidImage)?;
        let source_end = source_start
            .checked_add(usize::try_from(segment.file_size).map_err(|_| Error::InvalidImage)?)
            .ok_or(Error::InvalidImage)?;
        let source = kernel
            .get(source_start..source_end)
            .ok_or(Error::InvalidImage)?;
        let segment_end = segment
            .physical_address
            .checked_add(segment.memory_size)
            .ok_or(Error::InvalidImage)?;
        if segment_end > usable_memory_size {
            return Err(Error::ImageTooLarge);
        }
        let destination = memory
            .guest_host_address(segment.physical_address, segment.memory_size)
            .ok_or(Error::ImageTooLarge)?;
        unsafe {
            write_bytes(destination as *mut u8, 0, segment.memory_size as usize);
            copy_nonoverlapping(source.as_ptr(), destination as *mut u8, source.len());
        }
        load_end = load_end.max(segment_end);
        loaded = true;
    }
    if !loaded {
        return Err(Error::InvalidImage);
    }

    let mut module = HvmModule::default();
    if let Some(initramfs) = initramfs {
        let address = align_up(load_end, 4096).ok_or(Error::ImageTooLarge)?;
        if address
            .checked_add(initramfs.len() as u64)
            .is_none_or(|end| end > usable_memory_size)
        {
            return Err(Error::ImageTooLarge);
        }
        let destination = memory
            .guest_host_address(address, initramfs.len() as u64)
            .ok_or(Error::ImageTooLarge)?;
        unsafe { copy_nonoverlapping(initramfs.as_ptr(), destination as *mut u8, initramfs.len()) };
        module.paddr = address;
        module.size = initramfs.len() as u64;
        write_guest_struct(memory, PVH_MODULE_LIST_GPA, &module)?;
    }

    let command_line_host = memory
        .guest_host_address(PVH_COMMAND_LINE_GPA, (command_line.len() + 1) as u64)
        .ok_or(Error::ImageTooLarge)?;
    unsafe {
        copy_nonoverlapping(
            command_line.as_ptr(),
            command_line_host as *mut u8,
            command_line.len(),
        );
        (command_line_host as *mut u8)
            .add(command_line.len())
            .write(0);
    }
    let memory_map = [
        HvmMemoryMapEntry {
            addr: 0,
            size: usable_memory_size,
            kind: XEN_HVM_MEMMAP_TYPE_RAM,
            reserved: 0,
        },
        HvmMemoryMapEntry {
            addr: usable_memory_size,
            size: memory.guest_memory_size() - usable_memory_size,
            kind: XEN_HVM_MEMMAP_TYPE_RESERVED,
            reserved: 0,
        },
    ];
    write_guest_struct(memory, PVH_MEMORY_MAP_GPA, &memory_map)?;
    let start_info = HvmStartInfo {
        magic: XEN_HVM_START_MAGIC_VALUE,
        version: 1,
        flags: 0,
        nr_modules: u32::from(initramfs.is_some()),
        modlist_paddr: if initramfs.is_some() {
            PVH_MODULE_LIST_GPA
        } else {
            0
        },
        cmdline_paddr: PVH_COMMAND_LINE_GPA,
        rsdp_paddr: rsdp,
        memmap_paddr: PVH_MEMORY_MAP_GPA,
        memmap_entries: memory_map.len() as u32,
        reserved: 0,
    };
    write_guest_struct(memory, PVH_START_INFO_GPA, &start_info)?;
    Ok(GuestImage {
        entry,
        boot_info: PVH_START_INFO_GPA,
        loaded_end: usable_memory_size,
    })
}

fn elf_header(image: &[u8]) -> Result<ElfHeader, Error> {
    elf_header_for(image, ElfArchitecture::X86_64)
}

fn elf_header_for(image: &[u8], architecture: ElfArchitecture) -> Result<ElfHeader, Error> {
    let header: ElfHeader = read_struct(image, 0)?;
    if header.ident[..4] != ELF_MAGIC
        || header.ident[4] != ELF_CLASS_64
        || header.ident[5] != ELF_DATA_LITTLE_ENDIAN
        || header.file_type != ELF_TYPE_EXECUTABLE
        || header.machine != architecture as u16
        || header.program_header_size as usize != size_of::<ProgramHeader>()
    {
        return Err(Error::InvalidImage);
    }
    Ok(header)
}

fn program_header(image: &[u8], header: ElfHeader, index: usize) -> Result<ProgramHeader, Error> {
    if index >= usize::from(header.program_header_count) {
        return Err(Error::InvalidImage);
    }
    let offset = usize::try_from(header.program_header_offset)
        .map_err(|_| Error::InvalidImage)?
        .checked_add(
            index
                .checked_mul(size_of::<ProgramHeader>())
                .ok_or(Error::InvalidImage)?,
        )
        .ok_or(Error::InvalidImage)?;
    read_struct(image, offset)
}

fn pvh_entry(image: &[u8], header: ElfHeader) -> Result<u64, Error> {
    for index in 0..usize::from(header.program_header_count) {
        let segment = program_header(image, header, index)?;
        if segment.segment_type != PT_NOTE {
            continue;
        }
        let start = usize::try_from(segment.offset).map_err(|_| Error::InvalidImage)?;
        let end = start
            .checked_add(usize::try_from(segment.file_size).map_err(|_| Error::InvalidImage)?)
            .ok_or(Error::InvalidImage)?;
        let notes = image.get(start..end).ok_or(Error::InvalidImage)?;
        let mut offset = 0usize;
        while offset < notes.len() {
            let name_size = read_u32(notes, offset)? as usize;
            let description_size = read_u32(notes, offset + 4)? as usize;
            let kind = read_u32(notes, offset + 8)?;
            let name_start = offset.checked_add(12).ok_or(Error::InvalidImage)?;
            let name_end = name_start
                .checked_add(name_size)
                .ok_or(Error::InvalidImage)?;
            let description_start =
                align_up(name_end as u64, 4).ok_or(Error::InvalidImage)? as usize;
            let description_end = description_start
                .checked_add(description_size)
                .ok_or(Error::InvalidImage)?;
            let next = align_up(description_end as u64, 4).ok_or(Error::InvalidImage)? as usize;
            let name = notes.get(name_start..name_end).ok_or(Error::InvalidImage)?;
            let description = notes
                .get(description_start..description_end)
                .ok_or(Error::InvalidImage)?;
            if kind == XEN_ELFNOTE_PHYS32_ENTRY
                && name.strip_suffix(&[0]) == Some(b"Xen".as_slice())
            {
                return match description.len() {
                    4 => Ok(u64::from(u32::from_le_bytes(
                        description.try_into().map_err(|_| Error::InvalidImage)?,
                    ))),
                    8 => Ok(u64::from_le_bytes(
                        description.try_into().map_err(|_| Error::InvalidImage)?,
                    )),
                    _ => Err(Error::InvalidImage),
                };
            }
            if next <= offset {
                return Err(Error::InvalidImage);
            }
            offset = next;
        }
    }
    Err(Error::InvalidImage)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or(Error::InvalidImage)?
            .try_into()
            .map_err(|_| Error::InvalidImage)?,
    ))
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
}

fn write_guest_struct<T: Copy>(
    memory: &impl ImageMemory,
    guest_address: u64,
    value: &T,
) -> Result<(), Error> {
    let destination = memory
        .guest_host_address(guest_address, size_of::<T>() as u64)
        .ok_or(Error::ImageTooLarge)?;
    unsafe { copy_nonoverlapping(value as *const T, destination as *mut T, 1) };
    Ok(())
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
    extern crate std;

    use super::*;
    use std::vec;

    #[repr(align(4096))]
    struct GuestMemory([u8; 128 * 1024]);

    struct ImageRam(core::cell::UnsafeCell<[u8; 128 * 1024]>);

    #[test]
    fn arm_linux_image_checks_header_and_extent_before_writing() {
        let mut image = [0u8; 128];
        image[8..16].copy_from_slice(&0x10000u64.to_le_bytes());
        image[16..24].copy_from_slice(&4096u64.to_le_bytes());
        image[24..32].copy_from_slice(&2u64.to_le_bytes());
        image[56..60].copy_from_slice(b"ARM\x64");
        for (field, value) in [
            (8, u64::MAX),
            (16, 0),
            (16, 0x20000),
            (24, 3),
            (24, 4),
            (24, 18),
            (32, 1),
        ] {
            let mut invalid = image;
            invalid[field..field + 8].copy_from_slice(&value.to_le_bytes());
            let memory = ImageRam(core::cell::UnsafeCell::new([0xaa; 128 * 1024]));
            // SAFETY: Private synchronous test RAM; failed admission must not
            // modify even one byte of it.
            assert!(unsafe { load_arm64_linux(&invalid, 0, &memory) }.is_err());
            assert!(unsafe { &*memory.0.get() }.iter().all(|b| *b == 0xaa));
        }
        let memory = ImageRam(core::cell::UnsafeCell::new([0xaa; 128 * 1024]));
        let loaded = unsafe { load_arm64_linux(&image, 0, &memory) }.unwrap();
        assert_eq!(loaded.entry(), 0x10000);
        assert_eq!(loaded.loaded_end(), 0x11000);
        let bytes = unsafe { &*memory.0.get() };
        assert_eq!(&bytes[0x10000..0x10080], &image);
        assert!(bytes[0x10080..0x11000].iter().all(|b| *b == 0));
        assert_eq!(bytes[0xffff], 0xaa);
        assert_eq!(bytes[0x11000], 0xaa);
    }

    // SAFETY: Test-owned RAM is interior-mutable, stable and disjoint from the
    // input image. Each test loads it synchronously with no VCPU or DMA actor.
    unsafe impl ImageMemory for ImageRam {
        fn guest_memory_size(&self) -> u64 {
            128 * 1024
        }
        fn guest_host_address(&self, address: u64, length: u64) -> Option<u64> {
            (address.checked_add(length)? <= self.guest_memory_size())
                .then(|| self.0.get() as u64 + address)
        }
    }

    fn split_native_elf(architecture: ElfArchitecture) -> std::vec::Vec<u8> {
        let mut image = vec![0; 0x300];
        let mut header = ElfHeader {
            ident: [0; 16],
            file_type: ELF_TYPE_EXECUTABLE,
            machine: architecture as u16,
            version: 1,
            entry: 0x10000,
            program_header_offset: size_of::<ElfHeader>() as u64,
            section_header_offset: 0,
            flags: 0,
            header_size: size_of::<ElfHeader>() as u16,
            program_header_size: size_of::<ProgramHeader>() as u16,
            program_header_count: 2,
            section_header_size: 0,
            section_header_count: 0,
            section_name_index: 0,
        };
        header.ident[..4].copy_from_slice(&ELF_MAGIC);
        header.ident[4] = ELF_CLASS_64;
        header.ident[5] = ELF_DATA_LITTLE_ENDIAN;
        write_test_struct(&mut image, 0, &header);
        for index in 0..2 {
            let physical = 0x10000 + index as u64 * 4096;
            let virtual_address = if architecture == ElfArchitecture::Aarch64 && index == 1 {
                0xffff_8000_0000_0000
            } else {
                physical
            };
            write_test_struct(
                &mut image,
                size_of::<ElfHeader>() + index * size_of::<ProgramHeader>(),
                &ProgramHeader {
                    segment_type: PT_LOAD,
                    flags: 5,
                    offset: 0x200 + index as u64 * 16,
                    virtual_address,
                    physical_address: physical,
                    file_size: 4,
                    memory_size: 8,
                    alignment: 1,
                },
            );
        }
        image[0x200..0x204].copy_from_slice(&[0x1f, 0x20, 0x03, 0xd5]);
        image[0x210..0x214].copy_from_slice(&[0xc0, 0x03, 0x5f, 0xd6]);
        image
    }

    #[test]
    fn native_elf_loads_physical_bytes_without_confusing_upper_virtual_addresses() {
        for architecture in [ElfArchitecture::X86_64, ElfArchitecture::Aarch64] {
            let image = split_native_elf(architecture);
            let memory = ImageRam(core::cell::UnsafeCell::new([0xaa; 128 * 1024]));
            // SAFETY: This test exclusively retains the stopped RAM and input.
            let loaded = unsafe { load_elf_for(architecture, &image, &memory) }.unwrap();
            assert_eq!(loaded.entry(), 0x10000);
            assert_eq!(loaded.loaded_end(), 0x11008);
            let bytes = unsafe { &*memory.0.get() };
            assert_eq!(&bytes[0x10000..0x10004], &image[0x200..0x204]);
            assert_eq!(&bytes[0x11000..0x11004], &image[0x210..0x214]);
            assert_eq!(&bytes[0x10004..0x10008], &[0; 4]);
            assert_eq!(&bytes[0x11004..0x11008], &[0; 4]);
            assert_eq!(bytes[0x10008], 0xaa);
            assert_eq!(bytes[0x11008], 0xaa);
        }
        if let Some(path) = std::env::var_os("MBOOT_ARM_BOOT_ELF") {
            // Optional integration with a real compiler/LLD image: its low
            // bootstrap, high text, initialized data and BSS have separate LMAs.
            let image = std::fs::read(path).unwrap();
            let memory = ImageRam(core::cell::UnsafeCell::new([0xaa; 128 * 1024]));
            let loaded =
                unsafe { load_elf_for(ElfArchitecture::Aarch64, &image, &memory) }.unwrap();
            assert_eq!(loaded.entry(), 0x10000);
            assert_eq!(loaded.loaded_end(), 0x12018);
            let bytes = unsafe { &*memory.0.get() };
            assert_eq!(&bytes[0x10000..0x10004], &[0x1f, 0x20, 0x03, 0xd5]);
            assert_eq!(&bytes[0x11000..0x11004], &[0xc0, 0x03, 0x5f, 0xd6]);
            assert_eq!(
                &bytes[0x12000..0x12008],
                &0x1122334455667788u64.to_le_bytes()
            );
            assert_eq!(&bytes[0x12008..0x12018], &[0; 16]);
            std::println!("Real Clang/LLD ARM bootstrap and upper-half load layout PASS");
        }
    }

    #[test]
    fn native_elf_rejects_foreign_or_invalid_layout_before_writing_ram() {
        let valid = split_native_elf(ElfArchitecture::Aarch64);
        for mode in 0..8 {
            let mut image = valid.clone();
            let mut header: ElfHeader = read_struct(&image, 0).unwrap();
            let mut segment = program_header(&image, header, 1).unwrap();
            let mut architecture = ElfArchitecture::Aarch64;
            let mut expected = Error::InvalidImage;
            match mode {
                0 => architecture = ElfArchitecture::X86_64,
                1 => segment.virtual_address = 0x0001_0000_0000_0000,
                2 => segment.physical_address = 0x8000,
                3 => {
                    segment.physical_address = 0x1fffc;
                    expected = Error::ImageTooLarge;
                }
                4 => header.entry += 1,
                5 => header.entry = 0x12000,
                6 => segment.virtual_address = u64::MAX - 3,
                _ => segment.file_size = 9,
            }
            write_test_struct(&mut image, 0, &header);
            write_test_struct(
                &mut image,
                size_of::<ElfHeader>() + size_of::<ProgramHeader>(),
                &segment,
            );
            let memory = ImageRam(core::cell::UnsafeCell::new([0xaa; 128 * 1024]));
            // SAFETY: Rejection must occur before writing this test-owned RAM.
            assert_eq!(
                unsafe { load_elf_for(architecture, &image, &memory) },
                Err(expected)
            );
            assert!(unsafe { &*memory.0.get() }.iter().all(|byte| *byte == 0xaa));
        }
    }

    #[test]
    fn truncated_elf_is_rejected() {
        let memory = NestedPageTable::test_new(crate::BackendKind::IntelVmx, 0x1000, 0x5000, 1);
        // SAFETY: Rejection happens before the synthetic host address is used.
        assert_eq!(
            unsafe { load_elf(&ELF_MAGIC, &memory) },
            Err(Error::InvalidImage)
        );
    }

    #[test]
    fn boot_module_is_page_aligned_below_the_reserved_limit() {
        let mut ram = GuestMemory([0; 128 * 1024]);
        let memory = NestedPageTable::test_new(
            crate::BackendKind::IntelVmx,
            0x1000,
            ram.0.as_mut_ptr() as u64,
            32,
        );
        let module = [0x5a; 5000];
        // SAFETY: The synthetic RAM is exclusively owned by this stopped test Domain.
        let (start, size) =
            unsafe { load_boot_module(&module, 0x10_000, 0x20_000, &memory).unwrap() };
        assert_eq!(start & 0xfff, 0);
        assert_eq!(size, module.len() as u64);
        assert!(start >= 0x10_000);
        assert!(start + size <= 0x20_000);
        assert_eq!(
            &ram.0[start as usize..start as usize + module.len()],
            &module
        );
    }

    #[test]
    fn boot_module_cannot_overlap_the_native_image() {
        let mut ram = GuestMemory([0; 128 * 1024]);
        let memory = NestedPageTable::test_new(
            crate::BackendKind::IntelVmx,
            0x1000,
            ram.0.as_mut_ptr() as u64,
            32,
        );
        // SAFETY: Rejection happens without exposing this RAM to a running vCPU.
        assert_eq!(
            unsafe { load_boot_module(&[0; 0x3000], 0x1e_000, 0x20_000, &memory) },
            Err(Error::ImageTooLarge)
        );
    }

    #[test]
    fn loads_a_linux_pvh_kernel_and_boot_information() {
        let mut kernel = vec![0u8; 0x400];
        let mut header = ElfHeader {
            ident: [0; 16],
            file_type: ELF_TYPE_EXECUTABLE,
            machine: ELF_MACHINE_X86_64,
            version: 1,
            entry: 0,
            program_header_offset: size_of::<ElfHeader>() as u64,
            section_header_offset: 0,
            flags: 0,
            header_size: size_of::<ElfHeader>() as u16,
            program_header_size: size_of::<ProgramHeader>() as u16,
            program_header_count: 2,
            section_header_size: 0,
            section_header_count: 0,
            section_name_index: 0,
        };
        header.ident[..4].copy_from_slice(&ELF_MAGIC);
        header.ident[4] = ELF_CLASS_64;
        header.ident[5] = ELF_DATA_LITTLE_ENDIAN;
        write_test_struct(&mut kernel, 0, &header);
        write_test_struct(
            &mut kernel,
            size_of::<ElfHeader>(),
            &ProgramHeader {
                segment_type: PT_LOAD,
                flags: 5,
                offset: 0x200,
                virtual_address: 0xffff_ffff_8100_0000,
                physical_address: 0x1_0000,
                file_size: 4,
                memory_size: 8,
                alignment: 0x1000,
            },
        );
        write_test_struct(
            &mut kernel,
            size_of::<ElfHeader>() + size_of::<ProgramHeader>(),
            &ProgramHeader {
                segment_type: PT_NOTE,
                flags: 0,
                offset: 0x300,
                virtual_address: 0,
                physical_address: 0,
                file_size: 24,
                memory_size: 24,
                alignment: 4,
            },
        );
        kernel[0x200..0x204].copy_from_slice(b"PVH!");
        kernel[0x300..0x304].copy_from_slice(&4u32.to_le_bytes());
        kernel[0x304..0x308].copy_from_slice(&8u32.to_le_bytes());
        kernel[0x308..0x30c].copy_from_slice(&XEN_ELFNOTE_PHYS32_ENTRY.to_le_bytes());
        kernel[0x30c..0x310].copy_from_slice(b"Xen\0");
        kernel[0x310..0x318].copy_from_slice(&0x1_0000u64.to_le_bytes());

        let mut guest = GuestMemory([0xaa; 128 * 1024]);
        let memory = NestedPageTable::test_new(
            crate::BackendKind::IntelVmx,
            0x1000,
            guest.0.as_mut_ptr() as u64,
            guest.0.len() / 4096,
        );
        let loaded = unsafe {
            load_linux_pvh(
                &kernel,
                Some(b"initramfs"),
                "console=ttyS0",
                0x1234,
                guest.0.len() as u64 - 2 * 4096,
                &memory,
            )
        }
        .unwrap();
        assert_eq!(loaded.entry(), 0x1_0000);
        assert_eq!(loaded.boot_info(), PVH_START_INFO_GPA);
        assert_eq!(&guest.0[0x1_0000..0x1_0004], b"PVH!");
        assert_eq!(&guest.0[0x1_0004..0x1_0008], &[0; 4]);
        let start_info: HvmStartInfo = unsafe {
            guest
                .0
                .as_ptr()
                .add(PVH_START_INFO_GPA as usize)
                .cast::<HvmStartInfo>()
                .read_unaligned()
        };
        assert_eq!(start_info.magic, XEN_HVM_START_MAGIC_VALUE);
        assert_eq!(start_info.nr_modules, 1);
        assert_eq!(start_info.rsdp_paddr, 0x1234);
        assert_eq!(start_info.memmap_entries, 2);
        let memory_map: [HvmMemoryMapEntry; 2] = unsafe {
            guest
                .0
                .as_ptr()
                .add(PVH_MEMORY_MAP_GPA as usize)
                .cast::<[HvmMemoryMapEntry; 2]>()
                .read_unaligned()
        };
        assert_eq!(memory_map[0].size, guest.0.len() as u64 - 2 * 4096);
        assert_eq!(memory_map[0].kind, XEN_HVM_MEMMAP_TYPE_RAM);
        assert_eq!(memory_map[1].size, 2 * 4096);
        assert_eq!(memory_map[1].kind, XEN_HVM_MEMMAP_TYPE_RESERVED);
    }

    fn write_test_struct<T: Copy>(bytes: &mut [u8], offset: usize, value: &T) {
        unsafe {
            copy_nonoverlapping(
                value as *const T as *const u8,
                bytes.as_mut_ptr().add(offset),
                size_of::<T>(),
            )
        }
    }
}
