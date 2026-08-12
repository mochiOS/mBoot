use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExportKind {
    Directory,
    File,
}

#[derive(Clone, Debug)]
pub(crate) struct ExportEntry {
    pub(crate) kind: ExportKind,
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) mode: u32,
    source: PathBuf,
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
    mode: u32,
    written: u64,
    temporary: PathBuf,
    destination: PathBuf,
    file: File,
}

struct Export {
    instance: u64,
    entries: Vec<ExportEntry>,
}

#[derive(Default)]
pub(crate) struct LinuxPortalState {
    grants: Vec<Grant>,
    transaction: Option<Transaction>,
    export: Option<Export>,
    entries: usize,
    total_size: u64,
}

impl LinuxPortalState {
    pub(crate) fn reset(&mut self, instance: u64) -> Result<(), PortalError> {
        if instance == 0 || self.transaction.is_some() || self.export.is_some() {
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
        mode: u32,
    ) -> Result<(), PortalError> {
        if instance == 0
            || id == 0
            || !valid_absolute_path(path)
            || self.grants.len() >= MAX_GRANTS
            || self.grants.iter().any(|grant| grant.id == id)
            || mode > 0o777
            || self
                .grants
                .iter()
                .filter(|grant| grant.instance == instance)
                .any(|grant| paths_overlap(&grant.path, path))
            || (writable
                && self
                    .grants
                    .iter()
                    .any(|grant| grant.writable && paths_overlap(&grant.path, path)))
        {
            return Err(PortalError::InvalidArgument);
        }
        let destination = staged_path(instance, path)?;
        fs::create_dir_all(&destination).map_err(|_| PortalError::Internal)?;
        fs::set_permissions(&destination, fs::Permissions::from_mode(mode))
            .map_err(|_| PortalError::Internal)?;
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
        mode: u32,
    ) -> Result<(), PortalError> {
        self.validate_entry(instance, grant, path)?;
        if mode > 0o777 {
            return Err(PortalError::InvalidArgument);
        }
        self.reserve_entry()?;
        let destination = staged_path(instance, path)?;
        fs::create_dir_all(&destination).map_err(|_| PortalError::Internal)?;
        fs::set_permissions(destination, fs::Permissions::from_mode(mode))
            .map_err(|_| PortalError::Internal)
    }

    pub(crate) fn begin_file(
        &mut self,
        instance: u64,
        grant: u64,
        path: &str,
        size: u64,
        mode: u32,
    ) -> Result<(), PortalError> {
        if self.transaction.is_some() {
            return Err(PortalError::Busy);
        }
        self.validate_entry(instance, grant, path)?;
        if size > MAX_FILE_SIZE
            || mode > 0o777
            || self.total_size.saturating_add(size) > MAX_TOTAL_SIZE
        {
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
            mode,
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
        fs::set_permissions(
            &transaction.destination,
            fs::Permissions::from_mode(transaction.mode),
        )
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

    pub(crate) fn release(&mut self, instance: u64) -> Result<(), PortalError> {
        if instance == 0
            || self
                .transaction
                .as_ref()
                .is_some_and(|transaction| transaction.instance == instance)
            || self
                .export
                .as_ref()
                .is_some_and(|export| export.instance == instance)
        {
            return Err(PortalError::Busy);
        }
        let root = instance_root(instance);
        if root.exists() {
            fs::remove_dir_all(root).map_err(|_| PortalError::Internal)?;
        }
        self.grants.retain(|grant| grant.instance != instance);
        Ok(())
    }

    pub(crate) fn begin_export(
        &mut self,
        instance: u64,
        grant_id: u64,
    ) -> Result<(usize, u32), PortalError> {
        if self
            .export
            .as_ref()
            .is_some_and(|export| export.instance != instance)
        {
            return Err(PortalError::Busy);
        }
        self.export = None;
        let grant = self
            .grants
            .iter()
            .find(|grant| grant.instance == instance && grant.id == grant_id && grant.writable)
            .ok_or(PortalError::InvalidState)?;
        let root = staged_path(instance, &grant.path)?;
        let root_mode = fs::symlink_metadata(&root)
            .map_err(|_| PortalError::Internal)?
            .permissions()
            .mode()
            & 0o777;
        let mut entries = Vec::new();
        let mut total_size = 0u64;
        collect_export_entries(&root, &root, &mut entries, &mut total_size)?;
        self.export = Some(Export { instance, entries });
        Ok((
            self.export
                .as_ref()
                .map_or(0, |export| export.entries.len()),
            root_mode,
        ))
    }

    pub(crate) fn export_entry(
        &self,
        instance: u64,
        index: usize,
    ) -> Result<&ExportEntry, PortalError> {
        self.export
            .as_ref()
            .filter(|export| export.instance == instance)
            .and_then(|export| export.entries.get(index))
            .ok_or(PortalError::InvalidState)
    }

    pub(crate) fn export_chunk(
        &self,
        instance: u64,
        index: usize,
        offset: u64,
        maximum: usize,
    ) -> Result<(u64, Vec<u8>), PortalError> {
        let entry = self.export_entry(instance, index)?;
        if entry.kind != ExportKind::File
            || offset > entry.size
            || maximum == 0
            || maximum > MAX_CHUNK_SIZE
        {
            return Err(PortalError::InvalidArgument);
        }
        let remaining = entry.size.saturating_sub(offset);
        let count = remaining.min(maximum as u64) as usize;
        let mut bytes = vec![0u8; count];
        let mut file = File::open(&entry.source).map_err(|_| PortalError::Internal)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| PortalError::Internal)?;
        file.read_exact(&mut bytes)
            .map_err(|_| PortalError::Internal)?;
        Ok((entry.size, bytes))
    }

    pub(crate) fn end_export(&mut self, instance: u64) -> Result<(), PortalError> {
        let export = self.export.take().ok_or(PortalError::InvalidState)?;
        if export.instance != instance {
            self.export = Some(export);
            return Err(PortalError::InvalidState);
        }
        Ok(())
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

fn collect_export_entries(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<ExportEntry>,
    total_size: &mut u64,
) -> Result<(), PortalError> {
    let mut children = fs::read_dir(directory)
        .map_err(|_| PortalError::Internal)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| PortalError::Internal)?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        if entries.len() >= MAX_ENTRIES {
            return Err(PortalError::InvalidArgument);
        }
        let source = child.path();
        let metadata = fs::symlink_metadata(&source).map_err(|_| PortalError::Internal)?;
        let relative = source
            .strip_prefix(root)
            .map_err(|_| PortalError::InvalidState)?
            .to_str()
            .ok_or(PortalError::InvalidArgument)?
            .to_string();
        if metadata.file_type().is_symlink() {
            return Err(PortalError::InvalidArgument);
        }
        if metadata.is_dir() {
            entries.push(ExportEntry {
                kind: ExportKind::Directory,
                path: relative,
                size: 0,
                mode: metadata.permissions().mode() & 0o777,
                source: source.clone(),
            });
            collect_export_entries(root, &source, entries, total_size)?;
        } else if metadata.is_file() && metadata.len() <= MAX_FILE_SIZE {
            *total_size = total_size.saturating_add(metadata.len());
            if *total_size > MAX_TOTAL_SIZE {
                return Err(PortalError::InvalidArgument);
            }
            entries.push(ExportEntry {
                kind: ExportKind::File,
                path: relative,
                size: metadata.len(),
                mode: metadata.permissions().mode() & 0o777,
                source,
            });
        } else {
            return Err(PortalError::InvalidArgument);
        }
    }
    Ok(())
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
