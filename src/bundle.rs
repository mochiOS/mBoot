use crate::Error;

const MAGIC: [u8; 8] = *b"MBOOTNET";
const VERSION: u16 = 1;
const HEADER_SIZE: usize = 32;
const ENTRY_HEADER_SIZE: usize = 16;
const MAX_FILE_COUNT: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkBundle<'a> {
    bytes: &'a [u8],
    file_count: usize,
}

impl<'a> NetworkBundle<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.get(..8) != Some(MAGIC.as_slice())
            || read_u16(bytes, 8)? != VERSION
            || usize::from(read_u16(bytes, 10)?) != HEADER_SIZE
            || bytes
                .get(20..HEADER_SIZE)
                .is_none_or(|reserved| reserved.iter().any(|byte| *byte != 0))
        {
            return Err(Error::InvalidBundle);
        }
        let file_count = usize::from(read_u16(bytes, 12)?);
        if file_count == 0
            || file_count > MAX_FILE_COUNT
            || read_u16(bytes, 14)? != 0
            || usize::try_from(read_u32(bytes, 16)?).ok() != Some(bytes.len())
        {
            return Err(Error::InvalidBundle);
        }

        let bundle = Self { bytes, file_count };
        let mut offset = HEADER_SIZE;
        for index in 0..file_count {
            let (path, _, next) = bundle.entry_at(offset)?;
            if path.is_empty() || path.as_bytes().contains(&0) {
                return Err(Error::InvalidBundle);
            }
            for previous in 0..index {
                if paths_equal(bundle.entry(previous)?.0, path) {
                    return Err(Error::InvalidBundle);
                }
            }
            offset = next;
        }
        if offset != bytes.len() {
            return Err(Error::InvalidBundle);
        }
        Ok(bundle)
    }

    pub fn file(self, path: &str) -> Result<&'a [u8], Error> {
        for index in 0..self.file_count {
            let (candidate, bytes) = self.entry(index)?;
            if paths_equal(candidate, path) {
                return Ok(bytes);
            }
        }
        Err(Error::InvalidBundle)
    }

    fn entry(self, wanted: usize) -> Result<(&'a str, &'a [u8]), Error> {
        let mut offset = HEADER_SIZE;
        for index in 0..self.file_count {
            let (path, bytes, next) = self.entry_at(offset)?;
            if index == wanted {
                return Ok((path, bytes));
            }
            offset = next;
        }
        Err(Error::InvalidBundle)
    }

    fn entry_at(self, offset: usize) -> Result<(&'a str, &'a [u8], usize), Error> {
        let header_end = offset
            .checked_add(ENTRY_HEADER_SIZE)
            .ok_or(Error::InvalidBundle)?;
        let header = self
            .bytes
            .get(offset..header_end)
            .ok_or(Error::InvalidBundle)?;
        let path_len = usize::from(read_u16(header, 0)?);
        let data_len = usize::try_from(read_u64(header, 4)?).map_err(|_| Error::InvalidBundle)?;
        if path_len == 0 || read_u16(header, 2)? != 0 || read_u32(header, 12)? != 0 {
            return Err(Error::InvalidBundle);
        }
        let path_end = header_end
            .checked_add(path_len)
            .ok_or(Error::InvalidBundle)?;
        let data_end = path_end.checked_add(data_len).ok_or(Error::InvalidBundle)?;
        let path = core::str::from_utf8(
            self.bytes
                .get(header_end..path_end)
                .ok_or(Error::InvalidBundle)?,
        )
        .map_err(|_| Error::InvalidBundle)?;
        let data = self
            .bytes
            .get(path_end..data_end)
            .ok_or(Error::InvalidBundle)?;
        Ok((path, data, data_end))
    }
}

fn paths_equal(left: &str, right: &str) -> bool {
    left.bytes()
        .map(normalize_separator)
        .eq(right.bytes().map(normalize_separator))
}

const fn normalize_separator(byte: u8) -> u8 {
    if byte == b'/' {
        b'\\'
    } else {
        byte
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, Error> {
    let value = bytes.get(offset..offset + 2).ok_or(Error::InvalidBundle)?;
    Ok(u16::from_le_bytes(
        value.try_into().map_err(|_| Error::InvalidBundle)?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let value = bytes.get(offset..offset + 4).ok_or(Error::InvalidBundle)?;
    Ok(u32::from_le_bytes(
        value.try_into().map_err(|_| Error::InvalidBundle)?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, Error> {
    let value = bytes.get(offset..offset + 8).ok_or(Error::InvalidBundle)?;
    Ok(u64::from_le_bytes(
        value.try_into().map_err(|_| Error::InvalidBundle)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    extern crate alloc;

    fn bundle(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = Vec::from(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&(HEADER_SIZE as u16).to_le_bytes());
        bytes.extend_from_slice(&(files.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 12]);
        for (path, data) in files {
            bytes.extend_from_slice(&(path.len() as u16).to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes.extend_from_slice(&(data.len() as u64).to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(path.as_bytes());
            bytes.extend_from_slice(data);
        }
        let size = bytes.len() as u32;
        bytes[16..20].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    #[test]
    fn finds_files_with_uefi_or_unix_separators() {
        let bytes = bundle(&[(r"\EFI\MBOOT\LAUNCH.MF", b"manifest")]);
        let parsed = NetworkBundle::parse(&bytes).unwrap();
        assert_eq!(
            parsed.file("/EFI/MBOOT/LAUNCH.MF"),
            Ok(b"manifest".as_slice())
        );
    }

    #[test]
    fn rejects_truncated_and_duplicate_entries() {
        let mut truncated = bundle(&[(r"\EFI\MBOOT\MNU.ELF", b"image")]);
        truncated.pop();
        assert_eq!(NetworkBundle::parse(&truncated), Err(Error::InvalidBundle));

        let duplicate = bundle(&[
            (r"\EFI\MBOOT\MNU.ELF", b"one"),
            (r"/EFI/MBOOT/MNU.ELF", b"two"),
        ]);
        assert_eq!(NetworkBundle::parse(&duplicate), Err(Error::InvalidBundle));
    }
}
