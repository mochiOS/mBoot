//! Firmware CPU identities and PSCI binding, without starting CPUs or granting
//! firmware-call authority. Only a trusted boot source can supply that authority.
//! DT CPU addresses are MPIDR affinity fields, never logical CPU indices.
use super::{Fdt, Token};

const MAX_CPUS: usize = 64;
const AFFINITY_MASK: u64 = 0x0000_00ff_00ff_ffff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Tree(super::Error),
    DuplicateProperty,
    InvalidProperty,
    UnsupportedCells,
    InvalidAffinity,
    DuplicateCpu,
    TooManyCpus,
    MissingBootCpu,
    DuplicateBinding,
    UnsupportedEnableMethod,
    UnsupportedConduit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conduit {
    Smc,
    Hvc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cpu {
    pub affinity: u64,
    pub enabled: bool,
    pub uses_psci: bool,
}

/// This is discovery metadata, not an admitted set of runnable vCPUs. The
/// runtime must still check CPU support, ownership, EL2 entry and conduit access.
pub struct Topology {
    cpus: [Cpu; MAX_CPUS],
    count: usize,
    boot: usize,
    conduit: Option<Conduit>,
}

impl Topology {
    pub fn from_fdt(fdt: &Fdt<'_>, boot_mpidr: u64) -> Result<Self, Error> {
        let mut topology = Self {
            cpus: [Cpu {
                affinity: 0,
                enabled: false,
                uses_psci: false,
            }; MAX_CPUS],
            count: 0,
            boot: 0,
            conduit: None,
        };
        let mut root_child = Properties::default();
        let mut cpu = Properties::default();
        let mut in_cpus = false;
        let mut cpus_seen = false;
        let mut in_cpu = false;
        // #address-cells/#size-cells are not inherited. DT defaults apply to
        // the container itself; /cpus must explicitly use zero size cells.
        let mut address_cells = None;
        let mut size_cells = None;
        for token in fdt.tokens() {
            match token.map_err(Error::Tree)? {
                Token::BeginNode { depth: 1, name } => {
                    root_child = Properties::default();
                    in_cpus = name == "cpus";
                    if in_cpus {
                        if cpus_seen {
                            return Err(Error::DuplicateBinding);
                        }
                        cpus_seen = true;
                    }
                }
                Token::BeginNode { depth: 2, name } if in_cpus => {
                    in_cpu = name == "cpu" || name.starts_with("cpu@");
                    cpu = Properties::default();
                }
                Token::Property {
                    depth: 1,
                    name,
                    value,
                } => {
                    root_child.set(name, value)?;
                    if in_cpus {
                        match name {
                            "#address-cells" => set(&mut address_cells, value)?,
                            "#size-cells" => set(&mut size_cells, value)?,
                            _ => {}
                        }
                    }
                }
                Token::Property {
                    depth: 2,
                    name,
                    value,
                } if in_cpu => cpu.set(name, value)?,
                Token::EndNode { depth: 2 } if in_cpu => {
                    in_cpu = false;
                    if !root_child.enabled()? {
                        return Err(Error::MissingBootCpu);
                    }
                    if cpu.device_type != Some(b"cpu\0".as_slice()) {
                        return Err(Error::InvalidProperty);
                    }
                    let cells = address_cells.map(cell).transpose()?.unwrap_or(2);
                    if !matches!(cells, 1 | 2) || size_cells.map(cell).transpose()? != Some(0) {
                        return Err(Error::UnsupportedCells);
                    }
                    let reg = cpu.reg.ok_or(Error::InvalidProperty)?;
                    if reg.len() != cells as usize * 4 {
                        return Err(Error::InvalidProperty);
                    }
                    let affinity = if cells == 1 {
                        u64::from(cell(reg)?)
                    } else {
                        u64::from(cell(&reg[..4])?) << 32 | u64::from(cell(&reg[4..])?)
                    };
                    if affinity & !AFFINITY_MASK != 0 {
                        return Err(Error::InvalidAffinity);
                    }
                    if topology.cpus().iter().any(|cpu| cpu.affinity == affinity) {
                        return Err(Error::DuplicateCpu);
                    }
                    let uses_psci = match cpu.enable_method {
                        None => false,
                        Some(b"psci\0") => true,
                        Some(_) => return Err(Error::UnsupportedEnableMethod),
                    };
                    let index = topology.count;
                    let slot = topology.cpus.get_mut(index).ok_or(Error::TooManyCpus)?;
                    *slot = Cpu {
                        affinity,
                        enabled: cpu.enabled()?,
                        uses_psci,
                    };
                    topology.count += 1;
                }
                Token::EndNode { depth: 1 } => {
                    if let Some(compatible) = root_child.compatible {
                        if (string_list_contains(compatible, b"arm,psci-0.2")?
                            || string_list_contains(compatible, b"arm,psci-1.0")?)
                            && root_child.enabled()?
                        {
                            let conduit = match root_child.method {
                                Some(b"smc\0") => Conduit::Smc,
                                Some(b"hvc\0") => Conduit::Hvc,
                                _ => return Err(Error::UnsupportedConduit),
                            };
                            if topology.conduit.replace(conduit).is_some() {
                                return Err(Error::DuplicateBinding);
                            }
                        }
                    }
                    in_cpus = false;
                }
                _ => {}
            }
        }
        topology.boot = topology
            .cpus()
            .iter()
            .position(|cpu| cpu.enabled && cpu.affinity == boot_mpidr & AFFINITY_MASK)
            .ok_or(Error::MissingBootCpu)?;
        // An AP's PSCI declaration without a matching controller is not a
        // usable boot contract. A single boot CPU may legitimately lack PSCI.
        if topology.conduit.is_none() && topology.cpus().iter().any(|cpu| cpu.uses_psci) {
            return Err(Error::UnsupportedConduit);
        }
        Ok(topology)
    }

    pub fn cpus(&self) -> &[Cpu] {
        &self.cpus[..self.count]
    }
    pub fn boot_cpu(&self) -> Cpu {
        self.cpus[self.boot]
    }
    pub fn psci_conduit(&self) -> Option<Conduit> {
        self.conduit
    }
}

#[derive(Default)]
struct Properties<'a> {
    status: Option<&'a [u8]>,
    compatible: Option<&'a [u8]>,
    method: Option<&'a [u8]>,
    enable_method: Option<&'a [u8]>,
    device_type: Option<&'a [u8]>,
    reg: Option<&'a [u8]>,
}

impl<'a> Properties<'a> {
    fn set(&mut self, name: &str, value: &'a [u8]) -> Result<(), Error> {
        match name {
            "status" => set(&mut self.status, value),
            "compatible" => set(&mut self.compatible, value),
            "method" => set(&mut self.method, value),
            "enable-method" => set(&mut self.enable_method, value),
            "device_type" => set(&mut self.device_type, value),
            "reg" => set(&mut self.reg, value),
            _ => Ok(()),
        }
    }
    fn enabled(&self) -> Result<bool, Error> {
        match self.status {
            None | Some(b"ok\0" | b"okay\0") => Ok(true),
            Some(status)
                if status.last() == Some(&0) && !status[..status.len() - 1].contains(&0) =>
            {
                Ok(false)
            }
            Some(_) => Err(Error::InvalidProperty),
        }
    }
}

fn set<'a>(slot: &mut Option<&'a [u8]>, value: &'a [u8]) -> Result<(), Error> {
    if slot.replace(value).is_some() {
        Err(Error::DuplicateProperty)
    } else {
        Ok(())
    }
}
fn cell(bytes: &[u8]) -> Result<u32, Error> {
    Ok(u32::from_be_bytes(
        bytes.try_into().map_err(|_| Error::InvalidProperty)?,
    ))
}
fn string_list_contains(bytes: &[u8], needle: &[u8]) -> Result<bool, Error> {
    if bytes.last() != Some(&0) {
        return Err(Error::InvalidProperty);
    }
    let mut found = false;
    for value in bytes[..bytes.len() - 1].split(|&byte| byte == 0) {
        if value.is_empty() || core::str::from_utf8(value).is_err() {
            return Err(Error::InvalidProperty);
        }
        found |= value == needle;
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::super::tests::{blob, words};
    use super::*;
    use std::vec::Vec;

    struct Tree {
        structure: Vec<u8>,
        strings: Vec<u8>,
    }
    impl Tree {
        fn new() -> Self {
            Self {
                structure: Vec::new(),
                strings: Vec::new(),
            }
        }
        fn begin(&mut self, name: &str) {
            self.structure.extend(words(&[1]));
            self.structure.extend(name.bytes());
            self.structure.push(0);
            while self.structure.len() % 4 != 0 {
                self.structure.push(0);
            }
        }
        fn end(&mut self) {
            self.structure.extend(words(&[2]));
        }
        fn property(&mut self, name: &str, value: &[u8]) {
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
        fn finish(mut self) -> Vec<u8> {
            self.structure.extend(words(&[9]));
            blob(&self.structure, &self.strings)
        }
    }

    fn sample(ids: &[u64], cells: u32, conduit: &[u8], disabled_last: bool) -> Vec<u8> {
        let mut tree = Tree::new();
        tree.begin("");
        tree.begin("psci");
        tree.property("compatible", b"arm,psci-1.0\0arm,psci-0.2\0");
        tree.property("method", conduit);
        tree.end();
        tree.begin("cpus");
        tree.property("#address-cells", &cells.to_be_bytes());
        tree.property("#size-cells", &0u32.to_be_bytes());
        for (i, id) in ids.iter().enumerate() {
            tree.begin(&std::format!("cpu@{id:x}"));
            tree.property("device_type", b"cpu\0");
            let reg = id.to_be_bytes();
            tree.property("reg", if cells == 1 { &reg[4..] } else { &reg });
            tree.property("enable-method", b"psci\0");
            if disabled_last && i + 1 == ids.len() {
                tree.property("status", b"disabled\0");
            }
            tree.end();
        }
        tree.end();
        tree.end();
        tree.finish()
    }

    #[test]
    fn clustered_affinities_and_boot_identity_are_not_flattened() {
        let ids = [0, 1, 2, 3, 0x100, 0x101, 0x102, 0x103];
        let bytes = sample(&ids, 1, b"smc\0", false);
        let fdt = Fdt::new(&bytes).unwrap();
        let topology = Topology::from_fdt(&fdt, (1 << 31) | 0x102).unwrap();
        assert_eq!(
            topology
                .cpus()
                .iter()
                .map(|cpu| cpu.affinity)
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(topology.boot_cpu().affinity, 0x102);
        assert_eq!(topology.psci_conduit(), Some(Conduit::Smc));
        let id = 0x12_0000_0103;
        let bytes = sample(&[id], 2, b"hvc\0", false);
        let topology = Topology::from_fdt(&Fdt::new(&bytes).unwrap(), id).unwrap();
        assert_eq!(topology.boot_cpu().affinity, id);
        assert_eq!(topology.psci_conduit(), Some(Conduit::Hvc));
    }

    #[test]
    fn invalid_ambiguous_or_disabled_identities_are_rejected() {
        for (ids, boot, disabled, expected) in [
            (&[0, 0][..], 0, false, Error::DuplicateCpu),
            (&[0, 0x100][..], 0x100, true, Error::MissingBootCpu),
            (&[0][..], 1, false, Error::MissingBootCpu),
            (&[1 << 24][..], 0, false, Error::InvalidAffinity),
            (&[1 << 40][..], 0, false, Error::InvalidAffinity),
        ] {
            let bytes = sample(ids, 2, b"smc\0", disabled);
            assert!(
                matches!(Topology::from_fdt(&Fdt::new(&bytes).unwrap(), boot), Err(e) if e == expected)
            );
        }
        let ids: Vec<_> = (0..65).collect();
        let bytes = sample(&ids, 1, b"smc\0", false);
        assert!(matches!(
            Topology::from_fdt(&Fdt::new(&bytes).unwrap(), 0),
            Err(Error::TooManyCpus)
        ));
        for method in [b"smc".as_slice(), b"smc\0hvc\0", b"unknown\0"] {
            let bytes = sample(&[0], 1, method, false);
            assert!(matches!(
                Topology::from_fdt(&Fdt::new(&bytes).unwrap(), 0),
                Err(Error::UnsupportedConduit)
            ));
        }
    }

    #[test]
    fn contradictory_cpu_properties_do_not_select_the_last_value() {
        let mut tree = Tree::new();
        tree.begin("");
        tree.begin("cpus");
        tree.property("#address-cells", &1u32.to_be_bytes());
        tree.property("#address-cells", &2u32.to_be_bytes());
        tree.end();
        tree.end();
        let bytes = tree.finish();
        assert!(matches!(
            Topology::from_fdt(&Fdt::new(&bytes).unwrap(), 0),
            Err(Error::DuplicateProperty)
        ));
    }

    #[test]
    fn actual_qemu_firmware_cpu_and_psci_bindings() {
        if let Some(path) = std::env::var_os("MBOOT_FDT_FIXTURE") {
            let bytes = std::fs::read(path).unwrap();
            let topology = Topology::from_fdt(&Fdt::new(&bytes).unwrap(), 0).unwrap();
            assert_eq!(topology.boot_cpu().affinity, 0);
            assert!(topology.cpus().iter().all(|cpu| cpu.enabled));
            assert_eq!(topology.psci_conduit(), Some(Conduit::Smc));
            std::println!("Real QEMU firmware CPU affinity / PSCI binding PASS");
        }
    }
}
