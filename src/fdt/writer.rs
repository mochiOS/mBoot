//! Bounded DTB v17 construction for an admitted virtual machine. A generated
//! description records assigned resources; it never grants physical ownership.
use super::{Error, Fdt, HEADER_SIZE, MAX_DEPTH};

pub struct Writer<'a> {
    bytes: &'a mut [u8],
    used: usize,
    strings: [u8; 2048],
    names: usize,
    depth: usize,
    root: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fdt::Token;

    #[test]
    fn generated_tree_round_trips_names_alignment_and_bounds() {
        let mut buffer = [0xaa; 256];
        let mut tree = Writer::new(&mut buffer).unwrap();
        assert_eq!(tree.property("bad", &[]), Err(Error::InvalidTree));
        tree.begin("").unwrap();
        tree.property("compatible", b"mochios,virtual\0").unwrap();
        tree.begin("node").unwrap();
        tree.property("compatible", b"device\0").unwrap();
        tree.end().unwrap();
        tree.end().unwrap();
        assert_eq!(tree.end(), Err(Error::InvalidTree));
        let blob = tree.finish().unwrap();
        let parsed = Fdt::new(blob).unwrap();
        assert_eq!(
            parsed
                .tokens()
                .filter(|t| matches!(
                    t,
                    Ok(Token::Property {
                        name: "compatible",
                        ..
                    })
                ))
                .count(),
            2
        );
        let mut short = [0; 64];
        let mut tree = Writer::new(&mut short).unwrap();
        tree.begin("").unwrap();
        assert_eq!(tree.property("large", &[0; 128]), Err(Error::Truncated));
    }
}

impl<'a> Writer<'a> {
    pub fn new(bytes: &'a mut [u8]) -> Result<Self, Error> {
        bytes.get_mut(..56).ok_or(Error::Truncated)?.fill(0);
        Ok(Self {
            bytes,
            used: 56,
            strings: [0; 2048],
            names: 0,
            depth: 0,
            root: false,
        })
    }

    fn append(&mut self, value: &[u8]) -> Result<(), Error> {
        let end = self.used.checked_add(value.len()).ok_or(Error::Truncated)?;
        self.bytes
            .get_mut(self.used..end)
            .ok_or(Error::Truncated)?
            .copy_from_slice(value);
        self.used = end;
        Ok(())
    }
    fn word(&mut self, value: u32) -> Result<(), Error> {
        self.append(&value.to_be_bytes())
    }
    fn align(&mut self) -> Result<(), Error> {
        while self.used & 3 != 0 {
            self.append(&[0])?;
        }
        Ok(())
    }
    pub fn begin(&mut self, name: &str) -> Result<(), Error> {
        if self.depth >= MAX_DEPTH
            || name.as_bytes().contains(&0)
            || name.contains('/')
            || self.depth == 0 && (self.root || !name.is_empty())
            || self.depth != 0 && name.is_empty()
        {
            return Err(Error::InvalidTree);
        }
        self.word(1)?;
        self.append(name.as_bytes())?;
        self.append(&[0])?;
        self.align()?;
        self.root = true;
        self.depth += 1;
        Ok(())
    }
    pub fn property(&mut self, name: &str, value: &[u8]) -> Result<(), Error> {
        if self.depth == 0 || name.is_empty() || name.as_bytes().contains(&0) {
            return Err(Error::InvalidTree);
        }
        let mut offset = 0;
        while offset < self.names {
            let end = self.strings[offset..self.names]
                .iter()
                .position(|&b| b == 0)
                .ok_or(Error::InvalidString)?
                + offset;
            if &self.strings[offset..end] == name.as_bytes() {
                break;
            }
            offset = end + 1;
        }
        if offset == self.names {
            let end = self
                .names
                .checked_add(name.len() + 1)
                .ok_or(Error::Truncated)?;
            self.strings
                .get_mut(self.names..end)
                .ok_or(Error::Truncated)?[..name.len()]
                .copy_from_slice(name.as_bytes());
            self.names = end;
        }
        self.word(3)?;
        self.word(u32::try_from(value.len()).map_err(|_| Error::Truncated)?)?;
        self.word(offset as u32)?;
        self.append(value)?;
        self.align()
    }
    pub fn end(&mut self) -> Result<(), Error> {
        if self.depth == 0 {
            return Err(Error::InvalidTree);
        }
        self.word(2)?;
        self.depth -= 1;
        Ok(())
    }
    pub fn finish(mut self) -> Result<&'a [u8], Error> {
        if !self.root || self.depth != 0 {
            return Err(Error::InvalidTree);
        }
        self.word(9)?;
        let structure = self.used - 56;
        let strings_start = self.used;
        let end = self.used.checked_add(self.names).ok_or(Error::Truncated)?;
        self.bytes
            .get_mut(self.used..end)
            .ok_or(Error::Truncated)?
            .copy_from_slice(&self.strings[..self.names]);
        self.used = end;
        let total = u32::try_from(self.used).map_err(|_| Error::Truncated)?;
        for (index, word) in [
            0xd00d_feed,
            total,
            56,
            strings_start as u32,
            HEADER_SIZE as u32,
            17,
            16,
            0,
            self.names as u32,
            structure as u32,
        ]
        .into_iter()
        .enumerate()
        {
            self.bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        let blob = &self.bytes[..self.used];
        Fdt::new(blob)?;
        Ok(blob)
    }
}
