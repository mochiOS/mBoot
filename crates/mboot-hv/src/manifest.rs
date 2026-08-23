use sha2::{Digest, Sha256};

use crate::Error;

pub const MANIFEST_MAGIC: &[u8; 8] = b"MBLHV1\0\0";
pub const MANIFEST_VERSION: u16 = 2;
pub const MANIFEST_HEADER_SIZE: usize = 32;
pub const DOMAIN_ENTRY_SIZE: usize = 160;
pub const EVENT_CHANNEL_ENTRY_SIZE: usize = 32;
pub const MAX_DOMAIN_COUNT: usize = 8;
pub const MAX_EVENT_CHANNEL_COUNT: usize = 64;

pub const DOMAIN_FLAG_AUTO_START: u16 = 1 << 0;
pub const DOMAIN_FLAG_REQUIRED: u16 = 1 << 1;
const DOMAIN_FLAGS_KNOWN: u16 = DOMAIN_FLAG_AUTO_START | DOMAIN_FLAG_REQUIRED;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ManifestDomainRole {
    System = 1,
    Hardware = 2,
    Application = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManifestDomain<'a> {
    pub id: u32,
    pub role: ManifestDomainRole,
    pub flags: u16,
    pub memory_size: u64,
    pub vcpu_count: u16,
    pub capabilities: u64,
    pub image_sha256: [u8; 32],
    pub image_path: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManifestEventChannel {
    pub domain_a: u32,
    pub port_a: u32,
    pub domain_b: u32,
    pub port_b: u32,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaunchManifest<'a> {
    bytes: &'a [u8],
    domain_count: usize,
    event_channel_count: usize,
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
        if domain_count == 0
            || domain_count > MAX_DOMAIN_COUNT
            || event_channel_count > MAX_EVENT_CHANNEL_COUNT
            || usize::from(read_u16(bytes, 22)?) != EVENT_CHANNEL_ENTRY_SIZE
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
            .ok_or(Error::InvalidManifest)?;
        if read_u32(bytes, 16)? as usize != expected_size
            || bytes.len() != expected_size
            || bytes[24..MANIFEST_HEADER_SIZE]
                .iter()
                .any(|byte| *byte != 0)
        {
            return Err(Error::InvalidManifest);
        }
        let manifest = Self {
            bytes,
            domain_count,
            event_channel_count,
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
        Ok(manifest)
    }

    pub const fn domain_count(self) -> usize {
        self.domain_count
    }

    pub const fn event_channel_count(self) -> usize {
        self.event_channel_count
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
        if id == 0
            || flags & !DOMAIN_FLAGS_KNOWN != 0
            || memory_size == 0
            || memory_size & 0xfff != 0
            || vcpu_count == 0
            || entry[18..32].iter().any(|byte| *byte != 0)
            || entry[74..80].iter().any(|byte| *byte != 0)
        {
            return Err(Error::InvalidManifest);
        }
        let capabilities = read_u64(entry, 32)?;
        let image_sha256 = entry[40..72]
            .try_into()
            .map_err(|_| Error::InvalidManifest)?;
        let path_len = usize::from(read_u16(entry, 72)?);
        let path_bytes = entry.get(80..80 + path_len).ok_or(Error::InvalidManifest)?;
        if path_len == 0 || entry[80 + path_len..].iter().any(|byte| *byte != 0) {
            return Err(Error::InvalidManifest);
        }
        let image_path = core::str::from_utf8(path_bytes).map_err(|_| Error::InvalidManifest)?;
        if !valid_uefi_path(image_path) {
            return Err(Error::InvalidManifest);
        }
        Ok(ManifestDomain {
            id,
            role,
            flags,
            memory_size,
            vcpu_count,
            capabilities,
            image_sha256,
            image_path,
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

    fn has_domain(self, id: u32) -> bool {
        (0..self.domain_count).any(|index| self.domain(index).is_ok_and(|domain| domain.id == id))
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
    use super::*;

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
        let entry = &mut bytes[MANIFEST_HEADER_SIZE..];
        entry[..4].copy_from_slice(&1_u32.to_le_bytes());
        entry[4..6].copy_from_slice(&(ManifestDomainRole::System as u16).to_le_bytes());
        entry[6..8].copy_from_slice(&(DOMAIN_FLAG_AUTO_START | DOMAIN_FLAG_REQUIRED).to_le_bytes());
        entry[8..16].copy_from_slice(&(2_u64 * 1024 * 1024).to_le_bytes());
        entry[16..18].copy_from_slice(&1_u16.to_le_bytes());
        let path = b"\\EFI\\MBOOT\\MNU.ELF";
        entry[72..74].copy_from_slice(&(path.len() as u16).to_le_bytes());
        entry[80..80 + path.len()].copy_from_slice(path);
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
        bytes[MANIFEST_HEADER_SIZE + 80..].fill(0);
        bytes[MANIFEST_HEADER_SIZE + 80..MANIFEST_HEADER_SIZE + 80 + path.len()]
            .copy_from_slice(path);
        let digest = Sha256::digest(bytes).into();
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
}
