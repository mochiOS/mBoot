//! Borrowed, allocation-free DTB v17 reader for firmware resource discovery.
//! Parsing grants no ownership of RAM, devices, interrupt IDs or firmware
//! calls. Platform admission must interpret bindings and reserve resources.

#[path = "fdt/arm64.rs"]
pub mod arm64;
#[path = "fdt/gic.rs"]
pub mod gic;
#[path = "fdt/memory.rs"]
pub mod memory;
#[path = "fdt/timer.rs"]
pub mod timer;

const HEADER_SIZE: usize = 40;
const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Truncated,
    InvalidHeader,
    UnsupportedVersion,
    OverlappingBlocks,
    InvalidReservation,
    InvalidToken,
    InvalidTree,
    InvalidString,
    TooDeep,
    InvalidSeed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub address: u64,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token<'a> {
    BeginNode {
        depth: usize,
        name: &'a str,
    },
    Property {
        depth: usize,
        name: &'a str,
        value: &'a [u8],
    },
    EndNode {
        depth: usize,
    },
}

/// All views borrow the original, immutable firmware bytes. No input address
/// is dereferenced, and integers are decoded without alignment assumptions.
pub struct Fdt<'a> {
    blob: &'a [u8],
    structure: &'a [u8],
    strings: &'a [u8],
    reservations: &'a [u8],
    boot_cpu: u32,
}

impl<'a> Fdt<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER_SIZE {
            return Err(Error::Truncated);
        }
        if word(bytes, 0)? != 0xd00d_feed {
            return Err(Error::InvalidHeader);
        }
        let total = word(bytes, 4)? as usize;
        if total < HEADER_SIZE {
            return Err(Error::InvalidHeader);
        }
        let blob = bytes.get(..total).ok_or(Error::Truncated)?;
        let version = word(blob, 20)?;
        let compatible = word(blob, 24)?;
        if version < 17 || compatible > 17 || compatible > version {
            return Err(Error::UnsupportedVersion);
        }
        let structure_start = word(blob, 8)? as usize;
        let strings_start = word(blob, 12)? as usize;
        let reservation_start = word(blob, 16)? as usize;
        if structure_start % 4 != 0
            || reservation_start % 8 != 0
            || [structure_start, strings_start, reservation_start]
                .iter()
                .any(|&start| start < HEADER_SIZE)
        {
            return Err(Error::InvalidHeader);
        }
        let structure = region(blob, structure_start, word(blob, 36)? as usize)?;
        let strings = region(blob, strings_start, word(blob, 32)? as usize)?;
        if structure.len() % 4 != 0 {
            return Err(Error::InvalidHeader);
        }
        let mut reservation_end = reservation_start;
        loop {
            let entry = region(blob, reservation_end, 16)?;
            reservation_end = reservation_end.checked_add(16).ok_or(Error::Truncated)?;
            let address = double_word(entry, 0)?;
            let size = double_word(entry, 8)?;
            if address == 0 && size == 0 {
                break;
            }
            address.checked_add(size).ok_or(Error::InvalidReservation)?;
        }
        let blocks = [
            (structure_start, structure.len()),
            (strings_start, strings.len()),
            (reservation_start, reservation_end - reservation_start),
        ];
        for i in 0..blocks.len() {
            for j in i + 1..blocks.len() {
                let (a, alen) = blocks[i];
                let (b, blen) = blocks[j];
                if alen != 0 && blen != 0 && a < b + blen && b < a + alen {
                    return Err(Error::OverlappingBlocks);
                }
            }
        }
        let fdt = Self {
            blob,
            structure,
            strings,
            reservations: &blob[reservation_start..reservation_end - 16],
            boot_cpu: word(blob, 28)?,
        };
        // Validate the entire structure before exposing even an early property.
        // A consumer cannot accidentally accept a valid prefix of a broken tree.
        for token in fdt.tokens() {
            token?;
        }
        Ok(fdt)
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.blob
    }
    /// Legacy header value. Native ARM affinity must be read from CPU `reg`
    /// properties and matched against the full physical MPIDR, not truncated
    /// to this 32-bit field.
    pub fn boot_cpu_id(&self) -> u32 {
        self.boot_cpu
    }

    /// Decode `/chosen/rng-seed` without logging it. Only a trusted, fresh
    /// firmware handoff may supply entropy authority; a guest-provided DTB
    /// or replayed fixture is not an entropy source. No clock/address fallback.
    pub fn rng_seed(&self) -> Result<Option<[u8; 32]>, Error> {
        let mut chosen = false;
        let mut seen = false;
        let mut seed = None;
        for token in self.tokens() {
            match token? {
                Token::BeginNode { depth: 1, name } => {
                    chosen = name == "chosen";
                    if chosen && core::mem::replace(&mut seen, true) {
                        return Err(Error::InvalidTree);
                    }
                }
                Token::Property {
                    depth: 1,
                    name: "rng-seed",
                    value,
                } if chosen => {
                    if seed.is_some() || !(32..=256).contains(&value.len()) {
                        return Err(Error::InvalidSeed);
                    }
                    let bytes: [u8; 32] = value[..32].try_into().unwrap();
                    if bytes == [0; 32] {
                        return Err(Error::InvalidSeed);
                    }
                    seed = Some(bytes);
                }
                Token::EndNode { depth: 1 } => chosen = false,
                _ => {}
            }
        }
        Ok(seed)
    }

    pub fn reservations(&self) -> impl Iterator<Item = Reservation> + '_ {
        self.reservations.chunks_exact(16).map(|entry| Reservation {
            // Construction has checked every complete, bounded entry.
            address: u64::from_be_bytes(entry[..8].try_into().unwrap()),
            size: u64::from_be_bytes(entry[8..].try_into().unwrap()),
        })
    }

    pub fn tokens(&self) -> Tokens<'a> {
        Tokens {
            structure: self.structure,
            strings: self.strings,
            position: 0,
            depth: 0,
            children: [false; MAX_DEPTH],
            root_seen: false,
            root_closed: false,
            finished: false,
        }
    }
}

pub struct Tokens<'a> {
    structure: &'a [u8],
    strings: &'a [u8],
    position: usize,
    depth: usize,
    children: [bool; MAX_DEPTH],
    root_seen: bool,
    root_closed: bool,
    finished: bool,
}

impl<'a> Tokens<'a> {
    fn aligned_end(&self, end: usize) -> Result<usize, Error> {
        let aligned = end.checked_add(3).ok_or(Error::Truncated)? & !3;
        // Firmware/libfdt can leave nonzero padding after replacing a value.
        // Bound these bytes but never interpret or include them in a property.
        region(self.structure, end, aligned - end)?;
        Ok(aligned)
    }

    fn read(&mut self) -> Result<Option<Token<'a>>, Error> {
        loop {
            let token = word(self.structure, self.position)?;
            self.position += 4;
            match token {
                1 => {
                    if self.root_closed {
                        return Err(Error::InvalidTree);
                    }
                    if self.depth == MAX_DEPTH {
                        return Err(Error::TooDeep);
                    }
                    let (name, end) = string(self.structure, self.position)?;
                    self.position = self.aligned_end(end)?;
                    if self.depth == 0 {
                        if self.root_seen || !name.is_empty() {
                            return Err(Error::InvalidTree);
                        }
                        self.root_seen = true;
                    } else {
                        if name.is_empty() || name.as_bytes().contains(&b'/') {
                            return Err(Error::InvalidTree);
                        }
                        self.children[self.depth - 1] = true;
                    }
                    self.children[self.depth] = false;
                    let depth = self.depth;
                    self.depth += 1;
                    return Ok(Some(Token::BeginNode { depth, name }));
                }
                2 => {
                    if self.depth == 0 {
                        return Err(Error::InvalidTree);
                    }
                    self.depth -= 1;
                    if self.depth == 0 {
                        self.root_closed = true;
                    }
                    return Ok(Some(Token::EndNode { depth: self.depth }));
                }
                3 => {
                    if self.depth == 0 || self.children[self.depth - 1] {
                        return Err(Error::InvalidTree);
                    }
                    let length = word(self.structure, self.position)? as usize;
                    let name_offset = word(self.structure, self.position + 4)? as usize;
                    self.position += 8;
                    let value = region(self.structure, self.position, length)?;
                    self.position = self
                        .aligned_end(self.position.checked_add(length).ok_or(Error::Truncated)?)?;
                    let (name, _) = string(self.strings, name_offset)?;
                    if name.is_empty() {
                        return Err(Error::InvalidString);
                    }
                    return Ok(Some(Token::Property {
                        depth: self.depth - 1,
                        name,
                        value,
                    }));
                }
                4 => {}
                9 => {
                    if !self.root_seen
                        || !self.root_closed
                        || self.depth != 0
                        || self.position != self.structure.len()
                    {
                        return Err(Error::InvalidTree);
                    }
                    self.finished = true;
                    return Ok(None);
                }
                _ => return Err(Error::InvalidToken),
            }
        }
    }
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Result<Token<'a>, Error>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        match self.read() {
            Ok(Some(token)) => Some(Ok(token)),
            Ok(None) => None,
            Err(error) => {
                self.finished = true;
                Some(Err(error))
            }
        }
    }
}

fn region(bytes: &[u8], start: usize, length: usize) -> Result<&[u8], Error> {
    bytes
        .get(start..start.checked_add(length).ok_or(Error::Truncated)?)
        .ok_or(Error::Truncated)
}
fn word(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    Ok(u32::from_be_bytes(
        region(bytes, offset, 4)?
            .try_into()
            .map_err(|_| Error::Truncated)?,
    ))
}
fn double_word(bytes: &[u8], offset: usize) -> Result<u64, Error> {
    Ok(u64::from_be_bytes(
        region(bytes, offset, 8)?
            .try_into()
            .map_err(|_| Error::Truncated)?,
    ))
}
fn string(bytes: &[u8], offset: usize) -> Result<(&str, usize), Error> {
    let tail = bytes.get(offset..).ok_or(Error::Truncated)?;
    let length = tail
        .iter()
        .position(|&v| v == 0)
        .ok_or(Error::InvalidString)?;
    let name = core::str::from_utf8(&tail[..length]).map_err(|_| Error::InvalidString)?;
    Ok((name, offset + length + 1))
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    pub(super) struct Tree {
        structure: Vec<u8>,
        strings: Vec<u8>,
    }
    impl Tree {
        pub(super) fn new() -> Self {
            Self {
                structure: Vec::new(),
                strings: Vec::new(),
            }
        }
        pub(super) fn begin(&mut self, name: &str) {
            self.structure.extend(words(&[1]));
            self.structure.extend(name.bytes());
            self.structure.push(0);
            while self.structure.len() % 4 != 0 {
                self.structure.push(0);
            }
        }
        pub(super) fn end(&mut self) {
            self.structure.extend(words(&[2]));
        }
        pub(super) fn property(&mut self, name: &str, value: &[u8]) {
            let offset = self.strings.len() as u32;
            self.strings.extend(name.bytes());
            self.strings.push(0);
            self.structure
                .extend(words(&[3, value.len() as u32, offset]));
            self.structure.extend_from_slice(value);
            while self.structure.len() % 4 != 0 {
                self.structure.push(0);
            }
        }
        pub(super) fn finish(mut self) -> Vec<u8> {
            self.structure.extend(words(&[9]));
            blob(&self.structure, &self.strings)
        }
    }

    fn put_word(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    pub(super) fn words(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect()
    }
    pub(super) fn blob(structure: &[u8], strings: &[u8]) -> Vec<u8> {
        let mut bytes = std::vec![0u8; 72];
        bytes[40..48].copy_from_slice(&0x4400_0000u64.to_be_bytes());
        bytes[48..56].copy_from_slice(&4096u64.to_be_bytes());
        bytes.extend_from_slice(structure);
        bytes.extend_from_slice(strings);
        let total = bytes.len() as u32;
        for (offset, value) in [
            (0, 0xd00d_feed),
            (4, total),
            (8, 72),
            (12, 72 + structure.len() as u32),
            (16, 40),
            (20, 17),
            (24, 16),
            (28, 0),
            (32, strings.len() as u32),
            (36, structure.len() as u32),
        ] {
            put_word(&mut bytes, offset, value);
        }
        bytes
    }
    fn sample() -> Vec<u8> {
        // Root property, child property and complete tree terminator.
        let mut structure = words(&[1, 0, 3, 5, 0]);
        structure.extend_from_slice(b"demo\0\0\0\0");
        structure.extend_from_slice(&words(&[1]));
        structure.extend_from_slice(b"cpu@0\0\0\0");
        structure.extend_from_slice(&words(&[3, 4, 11, 0, 2, 2, 9]));
        blob(&structure, b"compatible\0reg\0")
    }

    #[test]
    fn firmware_seed_is_bounded_unique_and_never_fabricated() {
        for value in [&[7; 32][..], &[9; 64][..]] {
            let mut tree = Tree::new();
            tree.begin("");
            tree.begin("chosen");
            tree.property("rng-seed", value);
            tree.end();
            tree.end();
            let bytes = tree.finish();
            assert_eq!(
                Fdt::new(&bytes).unwrap().rng_seed().unwrap(),
                Some(value[..32].try_into().unwrap())
            );
        }
        for value in [&[0; 32][..], &[1; 31][..], &[1; 257][..]] {
            let mut tree = Tree::new();
            tree.begin("");
            tree.begin("chosen");
            tree.property("rng-seed", value);
            tree.end();
            tree.end();
            assert_eq!(
                Fdt::new(&tree.finish()).unwrap().rng_seed(),
                Err(Error::InvalidSeed)
            );
        }
        let mut tree = Tree::new();
        tree.begin("");
        tree.begin("chosen");
        tree.property("rng-seed", &[1; 32]);
        tree.property("rng-seed", &[2; 32]);
        tree.end();
        tree.end();
        assert_eq!(
            Fdt::new(&tree.finish()).unwrap().rng_seed(),
            Err(Error::InvalidSeed)
        );
        assert_eq!(Fdt::new(&sample()).unwrap().rng_seed(), Ok(None));
    }

    #[test]
    fn unaligned_views_decode_big_endian_tokens_and_reservations() {
        let bytes = sample();
        let mut unaligned = std::vec![0];
        unaligned.extend_from_slice(&bytes);
        let fdt = Fdt::new(&unaligned[1..]).unwrap();
        assert_eq!(fdt.bytes(), bytes);
        assert_eq!(fdt.boot_cpu_id(), 0);
        assert_eq!(
            fdt.reservations().collect::<Vec<_>>(),
            std::vec![Reservation {
                address: 0x4400_0000,
                size: 4096
            }]
        );
        let tokens = fdt.tokens().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(tokens.len(), 6);
        assert_eq!(
            tokens[1],
            Token::Property {
                depth: 0,
                name: "compatible",
                value: b"demo\0"
            }
        );
        assert_eq!(
            tokens[2],
            Token::BeginNode {
                depth: 1,
                name: "cpu@0"
            }
        );

        if let Some(path) = std::env::var_os("MBOOT_FDT_FIXTURE") {
            let real = std::fs::read(path).unwrap();
            let fdt = Fdt::new(&real).unwrap();
            assert!(fdt.tokens().any(|token| matches!(token, Ok(Token::Property { name: "compatible", value, .. })
                if value.split(|&v| v == 0).any(|v| v == b"arm,cortex-a15-gic" || v == b"arm,gic-400"))));
            std::println!("Real QEMU firmware DTB bounds and tokens PASS");
        }
    }

    #[test]
    fn truncated_and_overlapping_blocks_are_rejected() {
        let bytes = sample();
        for end in 0..bytes.len() {
            assert!(Fdt::new(&bytes[..end]).is_err());
        }
        for (offset, value) in [
            (4, u32::MAX),
            (8, 41),
            (12, 72),
            (16, 72),
            (36, u32::MAX),
            (32, u32::MAX),
        ] {
            let mut malformed = bytes.clone();
            put_word(&mut malformed, offset, value);
            assert!(Fdt::new(&malformed).is_err());
        }
        let mut future = bytes.clone();
        put_word(&mut future, 24, 18);
        assert!(matches!(Fdt::new(&future), Err(Error::UnsupportedVersion)));
    }

    #[test]
    fn broken_structure_never_exposes_a_valid_prefix() {
        for structure in [
            words(&[1, 0, 2]),
            words(&[1, 0, 9]),
            words(&[2, 9]),
            words(&[1, 0, 2, 1, 0, 2, 9]),
            words(&[1, 0, 2, 9, 4]),
            words(&[1, 0, 1, 0x6100_0000, 2, 3, 0, 0, 2, 9]),
        ] {
            assert!(Fdt::new(&blob(&structure, b"prop\0")).is_err());
        }
        let mut deep = Vec::new();
        deep.extend_from_slice(&words(&[1, 0]));
        for _ in 0..MAX_DEPTH {
            deep.extend_from_slice(&words(&[1, 0x6100_0000]));
        }
        assert!(matches!(Fdt::new(&blob(&deep, b"")), Err(Error::TooDeep)));
    }

    #[test]
    fn invalid_reservations_and_strings_fail_without_exposing_padding() {
        let mut bytes = sample();
        bytes[40..48].copy_from_slice(&(u64::MAX - 1).to_be_bytes());
        assert!(matches!(Fdt::new(&bytes), Err(Error::InvalidReservation)));
        let mut bytes = sample();
        bytes[72 + 25] = 1;
        assert_eq!(
            Fdt::new(&bytes).unwrap().tokens().nth(1).unwrap().unwrap(),
            Token::Property {
                depth: 0,
                name: "compatible",
                value: b"demo\0"
            }
        );
        let mut bytes = sample();
        *bytes.last_mut().unwrap() = 0xff;
        assert!(matches!(Fdt::new(&bytes), Err(Error::InvalidString)));
    }
}
