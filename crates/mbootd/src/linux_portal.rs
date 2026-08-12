use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const PORTAL_ROOT: &str = "/run/mboot/linux-portal";
const MAX_GRANTS: usize = 32;
const MAX_ENTRIES: usize = 65_536;
const MAX_FILE_SIZE: u64 = 256 * 1024 * 1024;
const MAX_TOTAL_SIZE: u64 = 512 * 1024 * 1024;
const MAX_CHUNK_SIZE: usize = 1536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PortalError {
    Busy,
    InvalidArgument,
    InvalidState,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PortalMount {
    pub(crate) source: PathBuf,
    pub(crate) target: String,
    pub(crate) writable: bool,
}

struct Grant {
    instance: u64,
    id: u64,
    path: String,
    writable: bool,
}

struct Transaction {
    instance: u64,
    expected_size: u64,
    written: u64,
    temporary: PathBuf,
    destination: PathBuf,
    file: File,
}

#[derive(Default)]
pub(crate) struct LinuxPortalState {
    grants: Vec<Grant>,
    transaction: Option<Transaction>,
    entries: usize,
    total_size: u64,
}

impl LinuxPortalState {
    pub(crate) fn reset(&mut self, instance: u64) -> Result<(), PortalError> {
        if instance == 0 || self.transaction.is_some() {
            return Err(if instance == 0 {
                PortalError::InvalidArgument
            } else {
                PortalError::Busy
            });
        }
        let root = instance_root(instance);
        if root.exists() {
            fs::remove_dir_all(&root).map_err(|_| PortalError::Internal)?;
        }
        fs::create_dir_all(root.join("tree")).map_err(|_| PortalError::Internal)?;
        self.grants.retain(|grant| grant.instance != instance);
        self.entries = 0;
        self.total_size = 0;
        Ok(())
    }

    pub(crate) fn grant(
        &mut self,
        instance: u64,
        id: u64,
        writable: bool,
        path: &str,
    ) -> Result<(), PortalError> {
        if instance == 0
            || id == 0
            || !valid_absolute_path(path)
            || self.grants.len() >= MAX_GRANTS
            || self.grants.iter().any(|grant| grant.id == id)
            || self
                .grants
                .iter()
                .filter(|grant| grant.instance == instance)
                .any(|grant| paths_overlap(&grant.path, path))
        {
            return Err(PortalError::InvalidArgument);
        }
        let destination = staged_path(instance, path)?;
        fs::create_dir_all(&destination).map_err(|_| PortalError::Internal)?;
        self.grants.push(Grant {
            instance,
            id,
            path: path.to_string(),
            writable,
        });
        Ok(())
    }

    pub(crate) fn mkdir(
        &mut self,
        instance: u64,
        grant: u64,
        path: &str,
    ) -> Result<(), PortalError> {
        self.validate_entry(instance, grant, path)?;
        self.reserve_entry()?;
        fs::create_dir_all(staged_path(instance, path)?).map_err(|_| PortalError::Internal)
    }

    pub(crate) fn begin_file(
        &mut self,
        instance: u64,
        grant: u64,
        path: &str,
        size: u64,
    ) -> Result<(), PortalError> {
        if self.transaction.is_some() {
            return Err(PortalError::Busy);
        }
        self.validate_entry(instance, grant, path)?;
        if size > MAX_FILE_SIZE || self.total_size.saturating_add(size) > MAX_TOTAL_SIZE {
            return Err(PortalError::InvalidArgument);
        }
        self.reserve_entry()?;
        let destination = staged_path(instance, path)?;
        let parent = destination.parent().ok_or(PortalError::InvalidArgument)?;
        fs::create_dir_all(parent).map_err(|_| PortalError::Internal)?;
        let temporary = destination.with_extension("mboot-partial");
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    PortalError::Busy
                } else {
                    PortalError::Internal
                }
            })?;
        self.transaction = Some(Transaction {
            instance,
            expected_size: size,
            written: 0,
            temporary,
            destination,
            file,
        });
        Ok(())
    }

    pub(crate) fn append(
        &mut self,
        instance: u64,
        offset: u64,
        encoded: &str,
    ) -> Result<(), PortalError> {
        let transaction = self
            .transaction
            .as_mut()
            .filter(|transaction| transaction.instance == instance)
            .ok_or(PortalError::InvalidState)?;
        if offset != transaction.written {
            return Err(PortalError::InvalidArgument);
        }
        let bytes = decode_hex(encoded)?;
        if bytes.is_empty()
            || bytes.len() > MAX_CHUNK_SIZE
            || transaction.written.saturating_add(bytes.len() as u64) > transaction.expected_size
        {
            return Err(PortalError::InvalidArgument);
        }
        transaction
            .file
            .write_all(&bytes)
            .map_err(|_| PortalError::Internal)?;
        transaction.written += bytes.len() as u64;
        Ok(())
    }

    pub(crate) fn commit_file(&mut self, instance: u64) -> Result<(), PortalError> {
        let transaction = self.transaction.take().ok_or(PortalError::InvalidState)?;
        if transaction.instance != instance || transaction.written != transaction.expected_size {
            let _ = fs::remove_file(transaction.temporary);
            return Err(PortalError::InvalidState);
        }
        transaction
            .file
            .sync_all()
            .map_err(|_| PortalError::Internal)?;
        fs::rename(&transaction.temporary, &transaction.destination)
            .map_err(|_| PortalError::Internal)?;
        self.total_size += transaction.expected_size;
        Ok(())
    }

    pub(crate) fn cancel_file(&mut self, instance: u64) -> Result<(), PortalError> {
        let transaction = self.transaction.take().ok_or(PortalError::InvalidState)?;
        if transaction.instance != instance {
            self.transaction = Some(transaction);
            return Err(PortalError::InvalidState);
        }
        fs::remove_file(transaction.temporary).map_err(|_| PortalError::Internal)
    }

    pub(crate) fn mounts(&self, instance: u64) -> Result<Vec<PortalMount>, PortalError> {
        self.grants
            .iter()
            .filter(|grant| grant.instance == instance)
            .map(|grant| {
                Ok(PortalMount {
                    source: staged_path(instance, &grant.path)?,
                    target: grant.path.clone(),
                    writable: grant.writable,
                })
            })
            .collect()
    }

    fn validate_entry(&self, instance: u64, grant: u64, path: &str) -> Result<(), PortalError> {
        if !valid_absolute_path(path) {
            return Err(PortalError::InvalidArgument);
        }
        let root = self
            .grants
            .iter()
            .find(|candidate| candidate.instance == instance && candidate.id == grant)
            .ok_or(PortalError::InvalidState)?;
        if path != root.path
            && !path
                .strip_prefix(&root.path)
                .is_some_and(|suffix| suffix.starts_with('/'))
        {
            return Err(PortalError::InvalidArgument);
        }
        Ok(())
    }

    fn reserve_entry(&mut self) -> Result<(), PortalError> {
        if self.entries >= MAX_ENTRIES {
            return Err(PortalError::InvalidArgument);
        }
        self.entries += 1;
        Ok(())
    }
}

impl Drop for LinuxPortalState {
    fn drop(&mut self) {
        if let Some(transaction) = self.transaction.take() {
            let _ = fs::remove_file(transaction.temporary);
        }
    }
}

fn instance_root(instance: u64) -> PathBuf {
    Path::new(PORTAL_ROOT).join(instance.to_string())
}

fn staged_path(instance: u64, path: &str) -> Result<PathBuf, PortalError> {
    if !valid_absolute_path(path) {
        return Err(PortalError::InvalidArgument);
    }
    Ok(instance_root(instance).join("tree").join(&path[1..]))
}

fn valid_absolute_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() > 1
        && !path.ends_with('/')
        && !path.contains("//")
        && path[1..]
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn paths_overlap(left: &str, right: &str) -> bool {
    left == right
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn decode_hex(value: &str) -> Result<Vec<u8>, PortalError> {
    if value.len() % 2 != 0 {
        return Err(PortalError::InvalidArgument);
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_digit(pair[0])?;
            let low = hex_digit(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

pub(crate) fn decode_path(value: &str) -> Result<String, PortalError> {
    let bytes = decode_hex(value)?;
    let path = String::from_utf8(bytes).map_err(|_| PortalError::InvalidArgument)?;
    valid_absolute_path(&path)
        .then_some(path)
        .ok_or(PortalError::InvalidArgument)
}

fn hex_digit(value: u8) -> Result<u8, PortalError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(PortalError::InvalidArgument),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_must_stay_below_its_grant() {
        let grant = Grant {
            instance: 7,
            id: 9,
            path: String::from("/home/alice/Develop"),
            writable: false,
        };
        let mut state = LinuxPortalState::default();
        state.grants.push(grant);
        assert!(
            state
                .validate_entry(7, 9, "/home/alice/Develop/src")
                .is_ok()
        );
        assert_eq!(
            state.validate_entry(7, 9, "/home/alice/Develop2"),
            Err(PortalError::InvalidArgument)
        );
        assert_eq!(
            state.validate_entry(7, 9, "/home/alice/../root"),
            Err(PortalError::InvalidArgument)
        );
    }
}
