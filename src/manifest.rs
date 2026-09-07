use mnu_abi::hypervisor::{DOMAIN_CAPABILITY_DEVICE_CLAIM, DOMAIN_CAPABILITY_DEVICE_QUERY};
use sha2::{Digest, Sha256};

use crate::Error;

pub const MANIFEST_MAGIC: &[u8; 8] = b"MBLHV1\0\0";
pub const MANIFEST_VERSION: u16 = 6;
pub const MANIFEST_HEADER_SIZE: usize = 32;
pub const DOMAIN_ENTRY_SIZE: usize = 384;
pub const EVENT_CHANNEL_ENTRY_SIZE: usize = 32;
pub const DEVICE_ENTRY_SIZE: usize = 64;
pub const MAX_DOMAIN_COUNT: usize = 8;
pub const MAX_EVENT_CHANNEL_COUNT: usize = 64;
pub const MAX_DEVICE_COUNT: usize = 64;

pub const DOMAIN_FLAG_AUTO_START: u16 = 1 << 0;
pub const DOMAIN_FLAG_REQUIRED: u16 = 1 << 1;
const DOMAIN_FLAGS_KNOWN: u16 = DOMAIN_FLAG_AUTO_START | DOMAIN_FLAG_REQUIRED;
pub const DEVICE_FLAG_REQUIRED: u16 = 1 << 0;
pub const DEVICE_FLAG_EPHEMERAL: u16 = 1 << 1;
pub const DEVICE_FLAG_READ_ONLY: u16 = 1 << 2;
pub const DEVICE_FLAG_PARTITIONED: u16 = 1 << 3;
pub const DEVICE_FLAG_WRITABLE: u16 = 1 << 4;
const DEVICE_FLAGS_KNOWN: u16 =
    DEVICE_FLAG_REQUIRED
        | DEVICE_FLAG_EPHEMERAL
        | DEVICE_FLAG_READ_ONLY
        | DEVICE_FLAG_PARTITIONED
        | DEVICE_FLAG_WRITABLE;
pub const AUTO_REQUESTER: u16 = u16::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ManifestDomainRole {
    System = 1,
    Hardware = 2,
    Application = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ManifestRestartPolicy {
    Never = 0,
    OnFailure = 1,
    Always = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ManifestImageFormat {
    NativeElf = 0,
    LinuxPvh = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ManifestDeviceKind {
    Other = 0,
    Display = 1,
    Block = 2,
    Network = 3,
    Usb = 4,
    Audio = 5,
    Nvme = 6,
    Vmd = 7,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManifestDomain<'a> {
    pub id: u32,
    pub role: ManifestDomainRole,
    pub flags: u16,
    pub memory_size: u64,
    pub vcpu_count: u16,
    pub restart_policy: ManifestRestartPolicy,
    pub max_restarts: u8,
    pub image_format: ManifestImageFormat,
    pub capabilities: u64,
    pub image_sha256: [u8; 32],
    pub image_path: &'a str,
    pub initramfs_sha256: [u8; 32],
    pub initramfs_path: Option<&'a str>,
    pub command_line: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManifestEventChannel {
    pub domain_a: u32,
    pub port_a: u32,
    pub domain_b: u32,
    pub port_b: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManifestDevice {
    pub segment: u16,
    pub requester: u16,
    pub kind: ManifestDeviceKind,
    pub flags: u16,
    pub domain_id: u32,
    pub storage_disk_guid: [u8; 16],
    pub storage_partition_type_guid: [u8; 16],
    pub storage_partition_guid: [u8; 16],
}

impl ManifestDevice {
    pub const fn is_required(self) -> bool {
        self.flags & DEVICE_FLAG_REQUIRED != 0
    }

    pub const fn is_ephemeral(self) -> bool {
        self.flags & DEVICE_FLAG_EPHEMERAL != 0
    }

    pub const fn is_read_only(self) -> bool {
        self.flags & DEVICE_FLAG_READ_ONLY != 0
    }

    pub const fn is_partitioned(self) -> bool {
        self.flags & DEVICE_FLAG_PARTITIONED != 0
    }

    pub const fn is_writable(self) -> bool {
        self.flags & DEVICE_FLAG_WRITABLE != 0
    }
}

impl ManifestDomain<'_> {
    pub const fn auto_starts(self) -> bool {
        self.flags & DOMAIN_FLAG_AUTO_START != 0
    }

    pub const fn is_required(self) -> bool {
        self.flags & DOMAIN_FLAG_REQUIRED != 0
    }

    pub fn verify_image(self, image: &[u8]) -> Result<(), Error> {
        let actual: [u8; 32] = Sha256::digest(image).into();
        if actual == self.image_sha256 {
            Ok(())
        } else {
            Err(Error::ImageDigestMismatch)
        }
    }

    pub fn verify_initramfs(self, image: &[u8]) -> Result<(), Error> {
        if self.initramfs_path.is_none() {
            return Err(Error::InvalidManifest);
        }
        let actual: [u8; 32] = Sha256::digest(image).into();
        if actual == self.initramfs_sha256 {
            Ok(())
        } else {
            Err(Error::ImageDigestMismatch)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaunchManifest<'a> {
    bytes: &'a [u8],
    domain_count: usize,
    event_channel_count: usize,
    device_count: usize,
}

impl<'a> LaunchManifest<'a> {
    pub fn parse(bytes: &'a [u8], expected_sha256: [u8; 32]) -> Result<Self, Error> {
        let actual: [u8; 32] = Sha256::digest(bytes).into();
        if actual != expected_sha256 {
            return Err(Error::ManifestDigestMismatch);
        }
        if bytes.get(..8) != Some(MANIFEST_MAGIC.as_slice())
            || read_u16(bytes, 8)? != MANIFEST_VERSION
            || usize::from(read_u16(bytes, 10)?) != MANIFEST_HEADER_SIZE
            || usize::from(read_u16(bytes, 12)?) != DOMAIN_ENTRY_SIZE
        {
            return Err(Error::InvalidManifest);
        }
        let domain_count = usize::from(read_u16(bytes, 14)?);
        let event_channel_count = usize::from(read_u16(bytes, 20)?);
        let device_count = usize::from(read_u16(bytes, 24)?);
        if domain_count == 0
            || domain_count > MAX_DOMAIN_COUNT
            || event_channel_count > MAX_EVENT_CHANNEL_COUNT
            || usize::from(read_u16(bytes, 22)?) != EVENT_CHANNEL_ENTRY_SIZE
            || device_count > MAX_DEVICE_COUNT
            || usize::from(read_u16(bytes, 26)?) != DEVICE_ENTRY_SIZE
        {
            return Err(Error::InvalidManifest);
        }
        let expected_size = MANIFEST_HEADER_SIZE
            .checked_add(
                domain_count
                    .checked_mul(DOMAIN_ENTRY_SIZE)
                    .ok_or(Error::InvalidManifest)?,
            )
            .and_then(|size| {
                event_channel_count
                    .checked_mul(EVENT_CHANNEL_ENTRY_SIZE)
                    .and_then(|channel_bytes| size.checked_add(channel_bytes))
            })
            .and_then(|size| {
                device_count
                    .checked_mul(DEVICE_ENTRY_SIZE)
                    .and_then(|device_bytes| size.checked_add(device_bytes))
            })
            .ok_or(Error::InvalidManifest)?;
        if read_u32(bytes, 16)? as usize != expected_size
            || bytes.len() != expected_size
            || bytes[28..MANIFEST_HEADER_SIZE]
                .iter()
                .any(|byte| *byte != 0)
        {
            return Err(Error::InvalidManifest);
        }
        let manifest = Self {
            bytes,
            domain_count,
            event_channel_count,
            device_count,
        };
        for index in 0..domain_count {
            let domain = manifest.domain(index)?;
            for previous in 0..index {
                if manifest.domain(previous)?.id == domain.id {
                    return Err(Error::InvalidManifest);
                }
            }
        }
        for index in 0..event_channel_count {
            let channel = manifest.event_channel(index)?;
            if !manifest.has_domain(channel.domain_a) || !manifest.has_domain(channel.domain_b) {
                return Err(Error::InvalidManifest);
            }
            for previous in 0..index {
                let other = manifest.event_channel(previous)?;
                if endpoint_matches(channel.domain_a, channel.port_a, other)
                    || endpoint_matches(channel.domain_b, channel.port_b, other)
                {
                    return Err(Error::InvalidManifest);
                }
            }
        }
        for index in 0..device_count {
            let device = manifest.device(index)?;
            let owner = manifest
                .domain_by_id(device.domain_id)
                .ok_or(Error::InvalidManifest)?;
            if owner.role != ManifestDomainRole::Hardware
                || owner.capabilities & DOMAIN_CAPABILITY_DEVICE_CLAIM == 0
            {
                return Err(Error::InvalidManifest);
            }
            for previous in 0..index {
                let other = manifest.device(previous)?;
                if other.segment == device.segment
                    && other.requester == device.requester
                    && (device.requester != AUTO_REQUESTER || other.kind == device.kind)
                {
                    return Err(Error::InvalidManifest);
                }
            }
        }
        Ok(manifest)
    }

    pub const fn domain_count(self) -> usize {
        self.domain_count
    }

    pub const fn event_channel_count(self) -> usize {
        self.event_channel_count
    }

    pub const fn device_count(self) -> usize {
        self.device_count
    }

    pub fn domain(self, index: usize) -> Result<ManifestDomain<'a>, Error> {
        if index >= self.domain_count {
            return Err(Error::InvalidManifest);
        }
        let offset = MANIFEST_HEADER_SIZE + index * DOMAIN_ENTRY_SIZE;
        let entry = &self.bytes[offset..offset + DOMAIN_ENTRY_SIZE];
        let id = read_u32(entry, 0)?;
        let role = match read_u16(entry, 4)? {
            1 => ManifestDomainRole::System,
            2 => ManifestDomainRole::Hardware,
            3 => ManifestDomainRole::Application,
            _ => return Err(Error::InvalidManifest),
        };
        let flags = read_u16(entry, 6)?;
        let memory_size = read_u64(entry, 8)?;
        let vcpu_count = read_u16(entry, 16)?;
        let restart_policy = match entry[18] {
            0 => ManifestRestartPolicy::Never,
            1 => ManifestRestartPolicy::OnFailure,
            2 => ManifestRestartPolicy::Always,
            _ => return Err(Error::InvalidManifest),
        };
        let max_restarts = entry[19];
        let image_format = match entry[20] {
            0 => ManifestImageFormat::NativeElf,
            1 => ManifestImageFormat::LinuxPvh,
            _ => return Err(Error::InvalidManifest),
        };
        if id == 0
            || flags & !DOMAIN_FLAGS_KNOWN != 0
            || memory_size == 0
            || memory_size & 0xfff != 0
            || vcpu_count == 0
            || restart_policy == ManifestRestartPolicy::Never && max_restarts != 0
            || restart_policy != ManifestRestartPolicy::Never && max_restarts == 0
            || entry[21..32].iter().any(|byte| *byte != 0)
            || entry[78..80].iter().any(|byte| *byte != 0)
        {
            return Err(Error::InvalidManifest);
        }
        let capabilities = read_u64(entry, 32)?;
        let known_capabilities = DOMAIN_CAPABILITY_DEVICE_QUERY | DOMAIN_CAPABILITY_DEVICE_CLAIM;
        if capabilities & !known_capabilities != 0
            || role != ManifestDomainRole::Hardware && capabilities != 0
        {
            return Err(Error::InvalidManifest);
        }
        let image_sha256 = entry[40..72]
            .try_into()
            .map_err(|_| Error::InvalidManifest)?;
        let path_len = usize::from(read_u16(entry, 72)?);
        let initramfs_path_len = usize::from(read_u16(entry, 74)?);
        let command_line_len = usize::from(read_u16(entry, 76)?);
        let initramfs_sha256 = entry[80..112]
            .try_into()
            .map_err(|_| Error::InvalidManifest)?;
        let path_bytes = entry
            .get(112..112 + path_len)
            .ok_or(Error::InvalidManifest)?;
        let initramfs_path_bytes = entry
            .get(192..192 + initramfs_path_len)
            .ok_or(Error::InvalidManifest)?;
        let command_line_bytes = entry
            .get(272..272 + command_line_len)
            .ok_or(Error::InvalidManifest)?;
        if path_len == 0
            || path_len > 80
            || initramfs_path_len > 80
            || command_line_len > 96
            || entry[112 + path_len..192].iter().any(|byte| *byte != 0)
            || entry[192 + initramfs_path_len..272]
                .iter()
                .any(|byte| *byte != 0)
            || entry[272 + command_line_len..]
                .iter()
                .any(|byte| *byte != 0)
        {
            return Err(Error::InvalidManifest);
        }
        let image_path = core::str::from_utf8(path_bytes).map_err(|_| Error::InvalidManifest)?;
        let initramfs_path = if initramfs_path_len == 0 {
            None
        } else {
            Some(core::str::from_utf8(initramfs_path_bytes).map_err(|_| Error::InvalidManifest)?)
        };
        let command_line =
            core::str::from_utf8(command_line_bytes).map_err(|_| Error::InvalidManifest)?;
        if !valid_uefi_path(image_path)
            || initramfs_path.is_some_and(|path| !valid_uefi_path(path))
            || !command_line.is_ascii()
            || image_format == ManifestImageFormat::NativeElf
                && (role != ManifestDomainRole::System && initramfs_path.is_some()
                    || !command_line.is_empty())
            || image_format == ManifestImageFormat::LinuxPvh && role != ManifestDomainRole::Hardware
            || initramfs_path.is_none() && initramfs_sha256 != [0; 32]
        {
            return Err(Error::InvalidManifest);
        }
        Ok(ManifestDomain {
            id,
            role,
            flags,
            memory_size,
            vcpu_count,
            restart_policy,
            max_restarts,
            image_format,
            capabilities,
            image_sha256,
            image_path,
            initramfs_sha256,
            initramfs_path,
            command_line,
        })
    }

    pub fn event_channel(self, index: usize) -> Result<ManifestEventChannel, Error> {
        if index >= self.event_channel_count {
            return Err(Error::InvalidManifest);
        }
        let offset = MANIFEST_HEADER_SIZE
            + self.domain_count * DOMAIN_ENTRY_SIZE
            + index * EVENT_CHANNEL_ENTRY_SIZE;
        let entry = &self.bytes[offset..offset + EVENT_CHANNEL_ENTRY_SIZE];
        let channel = ManifestEventChannel {
            domain_a: read_u32(entry, 0)?,
            port_a: read_u32(entry, 4)?,
            domain_b: read_u32(entry, 8)?,
            port_b: read_u32(entry, 12)?,
        };
        if channel.domain_a == 0
            || channel.domain_b == 0
            || channel.domain_a == channel.domain_b
            || channel.port_a == 0
            || channel.port_b == 0
            || entry[16..].iter().any(|byte| *byte != 0)
        {
            return Err(Error::InvalidManifest);
        }
        Ok(channel)
    }

    pub fn device(self, index: usize) -> Result<ManifestDevice, Error> {
        if index >= self.device_count {
            return Err(Error::InvalidManifest);
        }
        let offset = MANIFEST_HEADER_SIZE
            + self.domain_count * DOMAIN_ENTRY_SIZE
            + self.event_channel_count * EVENT_CHANNEL_ENTRY_SIZE
            + index * DEVICE_ENTRY_SIZE;
        let entry = &self.bytes[offset..offset + DEVICE_ENTRY_SIZE];
        let kind = match read_u16(entry, 4)? {
            0 => ManifestDeviceKind::Other,
            1 => ManifestDeviceKind::Display,
            2 => ManifestDeviceKind::Block,
            3 => ManifestDeviceKind::Network,
            4 => ManifestDeviceKind::Usb,
            5 => ManifestDeviceKind::Audio,
            6 => ManifestDeviceKind::Nvme,
            7 => ManifestDeviceKind::Vmd,
            _ => return Err(Error::InvalidManifest),
        };
        let device = ManifestDevice {
            segment: read_u16(entry, 0)?,
            requester: read_u16(entry, 2)?,
            kind,
            flags: read_u16(entry, 6)?,
            domain_id: read_u32(entry, 8)?,
            storage_disk_guid: read_guid(entry, 12)?,
            storage_partition_type_guid: read_guid(entry, 28)?,
            storage_partition_guid: read_guid(entry, 44)?,
        };
        let block_device = matches!(
            device.kind,
            ManifestDeviceKind::Block | ManifestDeviceKind::Nvme | ManifestDeviceKind::Vmd
        );
        if device.requester == 0
            || device.flags & !DEVICE_FLAGS_KNOWN != 0
            || device.domain_id == 0
            || (device.requester == AUTO_REQUESTER
                && (device.segment != 0
                    || !matches!(
                        device.kind,
                        ManifestDeviceKind::Display
                            | ManifestDeviceKind::Nvme
                            | ManifestDeviceKind::Vmd
                            | ManifestDeviceKind::Usb
                            | ManifestDeviceKind::Network
                    )))
            || (block_device
                && usize::from(device.is_ephemeral())
                    + usize::from(device.is_read_only())
                    + usize::from(device.is_partitioned())
                    + usize::from(device.is_writable())
                    != 1)
            || (!block_device
                && (device.is_ephemeral()
                    || device.is_read_only()
                    || device.is_partitioned()
                    || device.is_writable()))
            || (device.is_partitioned()
                && (device.storage_disk_guid == [0; 16]
                    || device.storage_partition_type_guid == [0; 16]
                    || device.storage_partition_guid == [0; 16]))
            || (!device.is_partitioned()
                && (device.storage_disk_guid != [0; 16]
                    || device.storage_partition_type_guid != [0; 16]
                    || device.storage_partition_guid != [0; 16]))
            || entry[60..].iter().any(|byte| *byte != 0)
        {
            return Err(Error::InvalidManifest);
        }
        Ok(device)
    }

    fn has_domain(self, id: u32) -> bool {
        (0..self.domain_count).any(|index| self.domain(index).is_ok_and(|domain| domain.id == id))
    }

    fn domain_by_id(self, id: u32) -> Option<ManifestDomain<'a>> {
        (0..self.domain_count)
            .find_map(|index| self.domain(index).ok().filter(|domain| domain.id == id))
    }
}

fn endpoint_matches(domain: u32, port: u32, channel: ManifestEventChannel) -> bool {
    (channel.domain_a == domain && channel.port_a == port)
        || (channel.domain_b == domain && channel.port_b == port)
}

fn valid_uefi_path(path: &str) -> bool {
    path.len() > 1
        && path.starts_with('\\')
        && !path.contains('\0')
        && !path.contains('/')
        && !path.chars().any(char::is_control)
        && !path.split('\\').any(|component| component == "..")
}

fn read_guid(bytes: &[u8], offset: usize) -> Result<[u8; 16], Error> {
    bytes
        .get(offset..offset + 16)
        .and_then(|value| value.try_into().ok())
        .ok_or(Error::InvalidManifest)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, Error> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or(Error::InvalidManifest)?
        .try_into()
        .map_err(|_| Error::InvalidManifest)?;
    Ok(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or(Error::InvalidManifest)?
        .try_into()
        .map_err(|_| Error::InvalidManifest)?;
    Ok(u32::from_le_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, Error> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or(Error::InvalidManifest)?
        .try_into()
        .map_err(|_| Error::InvalidManifest)?;
    Ok(u64::from_le_bytes(value))
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::{vec, vec::Vec};

    fn manifest() -> [u8; MANIFEST_HEADER_SIZE + DOMAIN_ENTRY_SIZE] {
        let mut bytes = [0; MANIFEST_HEADER_SIZE + DOMAIN_ENTRY_SIZE];
        let total_size = bytes.len() as u32;
        bytes[..8].copy_from_slice(MANIFEST_MAGIC);
        bytes[8..10].copy_from_slice(&MANIFEST_VERSION.to_le_bytes());
        bytes[10..12].copy_from_slice(&(MANIFEST_HEADER_SIZE as u16).to_le_bytes());
        bytes[12..14].copy_from_slice(&(DOMAIN_ENTRY_SIZE as u16).to_le_bytes());
        bytes[14..16].copy_from_slice(&1_u16.to_le_bytes());
        bytes[16..20].copy_from_slice(&total_size.to_le_bytes());
        bytes[22..24].copy_from_slice(&(EVENT_CHANNEL_ENTRY_SIZE as u16).to_le_bytes());
        bytes[26..28].copy_from_slice(&(DEVICE_ENTRY_SIZE as u16).to_le_bytes());
        let entry = &mut bytes[MANIFEST_HEADER_SIZE..];
        entry[..4].copy_from_slice(&1_u32.to_le_bytes());
        entry[4..6].copy_from_slice(&(ManifestDomainRole::System as u16).to_le_bytes());
        entry[6..8].copy_from_slice(&(DOMAIN_FLAG_AUTO_START | DOMAIN_FLAG_REQUIRED).to_le_bytes());
        entry[8..16].copy_from_slice(&(2_u64 * 1024 * 1024).to_le_bytes());
        entry[16..18].copy_from_slice(&1_u16.to_le_bytes());
        entry[18] = ManifestRestartPolicy::Never as u8;
        let path = b"\\EFI\\MBOOT\\MNU.ELF";
        entry[72..74].copy_from_slice(&(path.len() as u16).to_le_bytes());
        entry[112..112 + path.len()].copy_from_slice(path);
        bytes
    }

    fn manifest_with_device() -> Vec<u8> {
        let base = manifest();
        let mut bytes = vec![0; MANIFEST_HEADER_SIZE + 2 * DOMAIN_ENTRY_SIZE + DEVICE_ENTRY_SIZE];
        bytes[..MANIFEST_HEADER_SIZE].copy_from_slice(&base[..MANIFEST_HEADER_SIZE]);
        bytes[MANIFEST_HEADER_SIZE..MANIFEST_HEADER_SIZE + DOMAIN_ENTRY_SIZE]
            .copy_from_slice(&base[MANIFEST_HEADER_SIZE..]);
        let hardware = MANIFEST_HEADER_SIZE + DOMAIN_ENTRY_SIZE;
        bytes[hardware..hardware + DOMAIN_ENTRY_SIZE]
            .copy_from_slice(&base[MANIFEST_HEADER_SIZE..]);
        bytes[hardware..hardware + 4].copy_from_slice(&2_u32.to_le_bytes());
        bytes[hardware + 4..hardware + 6]
            .copy_from_slice(&(ManifestDomainRole::Hardware as u16).to_le_bytes());
        bytes[hardware + 32..hardware + 40].copy_from_slice(
            &(DOMAIN_CAPABILITY_DEVICE_QUERY | DOMAIN_CAPABILITY_DEVICE_CLAIM).to_le_bytes(),
        );
        let device = MANIFEST_HEADER_SIZE + 2 * DOMAIN_ENTRY_SIZE;
        bytes[device + 2..device + 4].copy_from_slice(&0x0010_u16.to_le_bytes());
        bytes[device + 4..device + 6]
            .copy_from_slice(&(ManifestDeviceKind::Block as u16).to_le_bytes());
        bytes[device + 6..device + 8]
            .copy_from_slice(&(DEVICE_FLAG_REQUIRED | DEVICE_FLAG_EPHEMERAL).to_le_bytes());
        bytes[device + 8..device + 12].copy_from_slice(&2_u32.to_le_bytes());
        bytes[14..16].copy_from_slice(&2_u16.to_le_bytes());
        let total_size = bytes.len() as u32;
        bytes[16..20].copy_from_slice(&total_size.to_le_bytes());
        bytes[24..26].copy_from_slice(&1_u16.to_le_bytes());
        bytes
    }

    #[test]
    fn parses_a_fixed_domain_entry() {
        let bytes = manifest();
        let digest = Sha256::digest(bytes).into();
        let manifest = LaunchManifest::parse(&bytes, digest).unwrap();
        let domain = manifest.domain(0).unwrap();
        assert_eq!(manifest.domain_count(), 1);
        assert_eq!(domain.id, 1);
        assert_eq!(domain.role, ManifestDomainRole::System);
        assert_eq!(domain.memory_size, 2 * 1024 * 1024);
        assert_eq!(domain.restart_policy, ManifestRestartPolicy::Never);
        assert_eq!(domain.image_format, ManifestImageFormat::NativeElf);
        assert_eq!(domain.image_path, "\\EFI\\MBOOT\\MNU.ELF");
        assert!(domain.auto_starts());
        assert!(domain.is_required());
    }

    #[test]
    fn rejects_a_changed_manifest() {
        let mut bytes = manifest();
        let digest = Sha256::digest(bytes).into();
        bytes[MANIFEST_HEADER_SIZE] = 9;
        assert_eq!(
            LaunchManifest::parse(&bytes, digest),
            Err(Error::ManifestDigestMismatch)
        );
    }

    #[test]
    fn rejects_parent_directory_paths() {
        let mut bytes = manifest();
        let path = b"\\EFI\\..\\MNU.ELF";
        bytes[MANIFEST_HEADER_SIZE + 72..MANIFEST_HEADER_SIZE + 74]
            .copy_from_slice(&(path.len() as u16).to_le_bytes());
        bytes[MANIFEST_HEADER_SIZE + 112..192].fill(0);
        bytes[MANIFEST_HEADER_SIZE + 112..MANIFEST_HEADER_SIZE + 112 + path.len()]
            .copy_from_slice(path);
        let digest = Sha256::digest(bytes).into();
        assert_eq!(
            LaunchManifest::parse(&bytes, digest),
            Err(Error::InvalidManifest)
        );
    }

    #[test]
    fn device_query_capability_belongs_only_to_hardware_domains() {
        let mut bytes = manifest();
        bytes[MANIFEST_HEADER_SIZE + 32..MANIFEST_HEADER_SIZE + 40]
            .copy_from_slice(&DOMAIN_CAPABILITY_DEVICE_QUERY.to_le_bytes());
        let digest = Sha256::digest(bytes).into();
        assert_eq!(
            LaunchManifest::parse(&bytes, digest),
            Err(Error::InvalidManifest)
        );

        bytes[MANIFEST_HEADER_SIZE + 4..MANIFEST_HEADER_SIZE + 6]
            .copy_from_slice(&(ManifestDomainRole::Hardware as u16).to_le_bytes());
        let digest = Sha256::digest(bytes).into();
        assert!(LaunchManifest::parse(&bytes, digest).is_ok());
    }

    #[test]
    fn parses_a_device_policy_for_a_capable_hardware_domain() {
        let bytes = manifest_with_device();
        let digest = Sha256::digest(&bytes).into();
        let manifest = LaunchManifest::parse(&bytes, digest).unwrap();
        assert_eq!(manifest.device_count(), 1);
        assert_eq!(
            manifest.device(0).unwrap(),
            ManifestDevice {
                segment: 0,
                requester: 0x0010,
                kind: ManifestDeviceKind::Block,
                flags: DEVICE_FLAG_REQUIRED | DEVICE_FLAG_EPHEMERAL,
                domain_id: 2,
                storage_disk_guid: [0; 16],
                storage_partition_type_guid: [0; 16],
                storage_partition_guid: [0; 16],
            }
        );
        assert!(manifest.device(0).unwrap().is_ephemeral());
    }

    #[test]
    fn parses_an_automatic_read_only_nvme_policy() {
        let mut bytes = manifest_with_device();
        let device = MANIFEST_HEADER_SIZE + 2 * DOMAIN_ENTRY_SIZE;
        bytes[device + 2..device + 4].copy_from_slice(&AUTO_REQUESTER.to_le_bytes());
        bytes[device + 4..device + 6]
            .copy_from_slice(&(ManifestDeviceKind::Nvme as u16).to_le_bytes());
        bytes[device + 6..device + 8]
            .copy_from_slice(&(DEVICE_FLAG_REQUIRED | DEVICE_FLAG_READ_ONLY).to_le_bytes());

        let digest = Sha256::digest(&bytes).into();
        let manifest = LaunchManifest::parse(&bytes, digest).unwrap();
        let device = manifest.device(0).unwrap();
        assert_eq!(device.requester, AUTO_REQUESTER);
        assert_eq!(device.kind, ManifestDeviceKind::Nvme);
        assert!(device.is_read_only());
        assert!(!device.is_ephemeral());
    }

    #[test]
    fn parses_an_automatic_display_policy() {
        let mut bytes = manifest_with_device();
        let device = MANIFEST_HEADER_SIZE + 2 * DOMAIN_ENTRY_SIZE;
        bytes[device + 2..device + 4].copy_from_slice(&AUTO_REQUESTER.to_le_bytes());
        bytes[device + 4..device + 6]
            .copy_from_slice(&(ManifestDeviceKind::Display as u16).to_le_bytes());
        bytes[device + 6..device + 8].copy_from_slice(&DEVICE_FLAG_REQUIRED.to_le_bytes());

        let digest = Sha256::digest(&bytes).into();
        let manifest = LaunchManifest::parse(&bytes, digest).unwrap();
        let device = manifest.device(0).unwrap();
        assert_eq!(device.requester, AUTO_REQUESTER);
        assert_eq!(device.kind, ManifestDeviceKind::Display);
        assert!(device.is_required());
    }

    #[test]
    fn parses_an_automatic_network_policy() {
        let mut bytes = manifest_with_device();
        let offset = MANIFEST_HEADER_SIZE + 2 * DOMAIN_ENTRY_SIZE;
        bytes[offset + 2..offset + 4].copy_from_slice(&AUTO_REQUESTER.to_le_bytes());
        bytes[offset + 4..offset + 6]
            .copy_from_slice(&(ManifestDeviceKind::Network as u16).to_le_bytes());
        bytes[offset + 6..offset + 8].copy_from_slice(&DEVICE_FLAG_REQUIRED.to_le_bytes());
        let digest = Sha256::digest(&bytes).into();
        let manifest = LaunchManifest::parse(&bytes, digest).unwrap();
        assert_eq!(manifest.device(0).unwrap().kind, ManifestDeviceKind::Network);
    }

    #[test]
    fn parses_an_enrolled_partition_policy() {
        let mut bytes = manifest_with_device();
        let offset = MANIFEST_HEADER_SIZE + 2 * DOMAIN_ENTRY_SIZE;
        bytes[offset + 6..offset + 8]
            .copy_from_slice(&(DEVICE_FLAG_REQUIRED | DEVICE_FLAG_PARTITIONED).to_le_bytes());
        bytes[offset + 12..offset + 28].copy_from_slice(&[1; 16]);
        bytes[offset + 28..offset + 44].copy_from_slice(&[2; 16]);
        bytes[offset + 44..offset + 60].copy_from_slice(&[3; 16]);

        let digest = Sha256::digest(&bytes).into();
        let device = LaunchManifest::parse(&bytes, digest)
            .unwrap()
            .device(0)
            .unwrap();
        assert!(device.is_partitioned());
        assert_eq!(device.storage_disk_guid, [1; 16]);
        assert_eq!(device.storage_partition_type_guid, [2; 16]);
        assert_eq!(device.storage_partition_guid, [3; 16]);
    }

    #[test]
    fn rejects_a_block_policy_with_read_only_and_ephemeral_modes() {
        let mut bytes = manifest_with_device();
        let device = MANIFEST_HEADER_SIZE + 2 * DOMAIN_ENTRY_SIZE;
        bytes[device + 6..device + 8].copy_from_slice(
            &(DEVICE_FLAG_REQUIRED | DEVICE_FLAG_EPHEMERAL | DEVICE_FLAG_READ_ONLY).to_le_bytes(),
        );

        let digest = Sha256::digest(&bytes).into();
        assert_eq!(
            LaunchManifest::parse(&bytes, digest),
            Err(Error::InvalidManifest)
        );
    }

    #[test]
    fn verifies_the_domain_image_digest() {
        let mut bytes = manifest();
        let image = b"mnu image";
        bytes[MANIFEST_HEADER_SIZE + 40..MANIFEST_HEADER_SIZE + 72]
            .copy_from_slice(&Sha256::digest(image));
        let digest = Sha256::digest(bytes).into();
        let domain = LaunchManifest::parse(&bytes, digest)
            .unwrap()
            .domain(0)
            .unwrap();
        assert_eq!(domain.verify_image(image), Ok(()));
        assert_eq!(
            domain.verify_image(b"changed image"),
            Err(Error::ImageDigestMismatch)
        );
    }

    #[test]
    fn parses_linux_pvh_boot_assets() {
        let mut bytes = manifest();
        let entry = &mut bytes[MANIFEST_HEADER_SIZE..];
        entry[4..6].copy_from_slice(&(ManifestDomainRole::Hardware as u16).to_le_bytes());
        entry[20] = ManifestImageFormat::LinuxPvh as u8;
        let initramfs = br"\EFI\MBOOT\DRIVER.CPIO";
        let command_line = b"console=ttyS0 init=/init";
        entry[74..76].copy_from_slice(&(initramfs.len() as u16).to_le_bytes());
        entry[76..78].copy_from_slice(&(command_line.len() as u16).to_le_bytes());
        entry[80..112].copy_from_slice(&Sha256::digest(b"initramfs"));
        entry[192..192 + initramfs.len()].copy_from_slice(initramfs);
        entry[272..272 + command_line.len()].copy_from_slice(command_line);
        let digest = Sha256::digest(bytes).into();
        let domain = LaunchManifest::parse(&bytes, digest)
            .unwrap()
            .domain(0)
            .unwrap();
        assert_eq!(domain.image_format, ManifestImageFormat::LinuxPvh);
        assert_eq!(domain.initramfs_path, Some("\\EFI\\MBOOT\\DRIVER.CPIO"));
        assert_eq!(domain.command_line, "console=ttyS0 init=/init");
        assert_eq!(domain.verify_initramfs(b"initramfs"), Ok(()));
    }
}
