use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const MAX_ROOTFS_SIZE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_CHUNK_SIZE: usize = 256 * 1024;
const STAGING_ROOT: &str = "/var/lib/mboot/staging";
const PACKAGE_ROOT: &str = "/bin/mboot";
const DIGEST_FILE: &str = "rootfs.sha256";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StageError {
    Busy,
    InvalidArgument,
    InvalidState,
    Integrity,
    Internal,
}

struct Transaction {
    instance: u64,
    bundle: String,
    expected_size: u64,
    expected_digest: [u8; 32],
    written: u64,
    digest: Sha256,
    temporary: PathBuf,
    file: File,
}

#[derive(Default)]
pub(crate) struct LinuxStageState {
    transaction: Option<Transaction>,
}

impl LinuxStageState {
    pub(crate) fn begin(
        &mut self,
        instance: u64,
        bundle: &str,
        size: u64,
        digest: &str,
    ) -> Result<bool, StageError> {
        if self.transaction.is_some() {
            return Err(StageError::Busy);
        }
        if instance == 0 || size == 0 || size > MAX_ROOTFS_SIZE || !valid_bundle_id(bundle) {
            return Err(StageError::InvalidArgument);
        }
        let expected_digest = parse_digest(digest)?;
        let package = Path::new(PACKAGE_ROOT).join(bundle);
        let installed = package.join("rootfs.squashfs");
        if installed.exists() {
            let metadata = fs::metadata(&installed).map_err(|_| StageError::Internal)?;
            if metadata.len() == size && cached_digest(&package)? == Some(expected_digest) {
                return Ok(true);
            }
            let mut file = File::open(&installed).map_err(|_| StageError::Internal)?;
            if metadata.len() == size && hash_file(&mut file)? == expected_digest {
                write_cached_digest(&package, expected_digest)?;
                return Ok(true);
            }
        }
        fs::create_dir_all(STAGING_ROOT).map_err(|_| StageError::Internal)?;
        let temporary = Path::new(STAGING_ROOT).join(format!("{instance}.squashfs.partial"));
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    StageError::Busy
                } else {
                    StageError::Internal
                }
            })?;
        self.transaction = Some(Transaction {
            instance,
            bundle: bundle.to_string(),
            expected_size: size,
            expected_digest,
            written: 0,
            digest: Sha256::new(),
            temporary,
            file,
        });
        Ok(false)
    }

    pub(crate) fn append(
        &mut self,
        instance: u64,
        offset: u64,
        encoded: &str,
    ) -> Result<(), StageError> {
        let transaction = self.transaction_mut(instance)?;
        if offset != transaction.written {
            return Err(StageError::InvalidArgument);
        }
        let bytes = match encoded.strip_prefix("base64:") {
            Some(encoded) => decode_base64(encoded)?,
            None => decode_hex(encoded)?,
        };
        if bytes.is_empty()
            || bytes.len() > MAX_CHUNK_SIZE
            || transaction.written.saturating_add(bytes.len() as u64) > transaction.expected_size
        {
            return Err(StageError::InvalidArgument);
        }
        transaction
            .file
            .write_all(&bytes)
            .map_err(|_| StageError::Internal)?;
        transaction.digest.update(&bytes);
        transaction.written += bytes.len() as u64;
        Ok(())
    }

    pub(crate) fn append_bytes(
        &mut self,
        instance: u64,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), StageError> {
        let transaction = self.transaction_mut(instance)?;
        if offset != transaction.written
            || bytes.is_empty()
            || bytes.len() > mboot_protocol::MAX_BULK_STAGE_BYTES
            || transaction.written.saturating_add(bytes.len() as u64) > transaction.expected_size
        {
            return Err(StageError::InvalidArgument);
        }
        transaction
            .file
            .write_all(bytes)
            .map_err(|_| StageError::Internal)?;
        transaction.digest.update(bytes);
        transaction.written += bytes.len() as u64;
        Ok(())
    }

    pub(crate) fn commit(&mut self, instance: u64) -> Result<PathBuf, StageError> {
        let mut transaction = self.transaction.take().ok_or(StageError::InvalidState)?;
        if transaction.instance != instance || transaction.written != transaction.expected_size {
            let _ = fs::remove_file(&transaction.temporary);
            return Err(StageError::InvalidState);
        }
        if transaction.digest.finalize_reset().as_slice() != transaction.expected_digest {
            let _ = fs::remove_file(&transaction.temporary);
            return Err(StageError::Integrity);
        }
        let package = Path::new(PACKAGE_ROOT).join(&transaction.bundle);
        fs::create_dir_all(&package).map_err(|_| StageError::Internal)?;
        let destination = package.join("rootfs.squashfs");
        if destination.exists() {
            let mut existing = File::open(&destination).map_err(|_| StageError::Internal)?;
            if hash_file(&mut existing)? == transaction.expected_digest {
                write_cached_digest(&package, transaction.expected_digest)?;
                let _ = fs::remove_file(&transaction.temporary);
                return Ok(destination);
            }
        }
        fs::rename(&transaction.temporary, &destination).map_err(|_| StageError::Internal)?;
        write_cached_digest(&package, transaction.expected_digest)?;
        // The verified bytes are immediately usable from the page cache. Persist
        // them asynchronously so application launch is not serialized on slow
        // removable flash media.
        let file = transaction.file;
        std::thread::spawn(move || {
            let _ = file.sync_all();
        });
        Ok(destination)
    }

    pub(crate) fn cancel(&mut self, instance: u64) -> Result<(), StageError> {
        let transaction = self.transaction.take().ok_or(StageError::InvalidState)?;
        if transaction.instance != instance {
            self.transaction = Some(transaction);
            return Err(StageError::InvalidState);
        }
        fs::remove_file(transaction.temporary).map_err(|_| StageError::Internal)
    }

    fn transaction_mut(&mut self, instance: u64) -> Result<&mut Transaction, StageError> {
        self.transaction
            .as_mut()
            .filter(|transaction| transaction.instance == instance)
            .ok_or(StageError::InvalidState)
    }
}

fn cached_digest(package: &Path) -> Result<Option<[u8; 32]>, StageError> {
    let value = match fs::read_to_string(package.join(DIGEST_FILE)) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(StageError::Internal),
    };
    Ok(parse_digest(value.trim()).ok())
}

fn write_cached_digest(package: &Path, digest: [u8; 32]) -> Result<(), StageError> {
    let mut encoded = String::with_capacity(65);
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded.push('\n');
    fs::write(package.join(DIGEST_FILE), encoded).map_err(|_| StageError::Internal)
}

impl Drop for LinuxStageState {
    fn drop(&mut self) {
        if let Some(transaction) = self.transaction.take() {
            let _ = fs::remove_file(transaction.temporary);
        }
    }
}

fn hash_file(file: &mut File) -> Result<[u8; 32], StageError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|_| StageError::Internal)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|_| StageError::Internal)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().into())
}

fn parse_digest(value: &str) -> Result<[u8; 32], StageError> {
    let bytes = decode_hex(value)?;
    bytes.try_into().map_err(|_| StageError::InvalidArgument)
}

fn decode_hex(encoded: &str) -> Result<Vec<u8>, StageError> {
    if encoded.len() % 2 != 0 {
        return Err(StageError::InvalidArgument);
    }
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_digit(pair[0]).ok_or(StageError::InvalidArgument)?;
            let low = hex_digit(pair[1]).ok_or(StageError::InvalidArgument)?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn decode_base64(encoded: &str) -> Result<Vec<u8>, StageError> {
    if encoded.is_empty() || encoded.len() % 4 == 1 {
        return Err(StageError::InvalidArgument);
    }
    let mut decoded = Vec::with_capacity(encoded.len() / 4 * 3 + 2);
    let chunks = encoded.as_bytes().chunks(4);
    for chunk in chunks {
        if chunk.len() < 2 {
            return Err(StageError::InvalidArgument);
        }
        let a = base64_digit(chunk[0])?;
        let b = base64_digit(chunk[1])?;
        let c = chunk
            .get(2)
            .copied()
            .map(base64_digit)
            .transpose()?
            .unwrap_or(0);
        let d = chunk
            .get(3)
            .copied()
            .map(base64_digit)
            .transpose()?
            .unwrap_or(0);
        let bits = (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(c) << 6) | u32::from(d);
        decoded.push((bits >> 16) as u8);
        if chunk.len() > 2 {
            decoded.push((bits >> 8) as u8);
        }
        if chunk.len() > 3 {
            decoded.push(bits as u8);
        }
    }
    Ok(decoded)
}

fn base64_digit(byte: u8) -> Result<u8, StageError> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(StageError::InvalidArgument),
    }
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

pub(crate) fn valid_bundle_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('.')
        && !value.ends_with('.')
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_ids_and_digests_are_strict() {
        assert!(valid_bundle_id("org.example.editor"));
        assert!(!valid_bundle_id("Org.example"));
        assert!(!valid_bundle_id("org/example"));
        assert!(parse_digest(&"00".repeat(32)).is_ok());
        assert_eq!(
            parse_digest(&"00".repeat(31)),
            Err(StageError::InvalidArgument)
        );
        assert_eq!(
            parse_digest(&"GG".repeat(32)),
            Err(StageError::InvalidArgument)
        );
    }

    #[test]
    fn base64_chunks_are_strict_and_bounded() {
        assert_eq!(decode_base64("AAE").unwrap(), vec![0, 1]);
        assert_eq!(decode_base64("YWJj").unwrap(), b"abc");
        assert_eq!(decode_base64("A"), Err(StageError::InvalidArgument));
        assert_eq!(decode_base64("AA="), Err(StageError::InvalidArgument));
    }
}
