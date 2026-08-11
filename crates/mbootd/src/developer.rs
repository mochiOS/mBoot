use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_SOURCE_SIZE: usize = 1024 * 1024;
const MAX_OUTPUT_SIZE: usize = 8 * 1024 * 1024;
const MAX_DIAGNOSTICS_SIZE: usize = 64 * 1024;
const MAX_CHUNK_SIZE: usize = 1536;
static BUILD_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeveloperError {
    Busy,
    InvalidArgument,
    InvalidState,
    Internal,
}

#[derive(Debug)]
struct BuildTransaction {
    id: u64,
    source: Vec<u8>,
    expected_size: usize,
    output: Vec<u8>,
    diagnostics: Vec<u8>,
    compile_status: Option<i32>,
}

#[derive(Debug, Default)]
pub(crate) struct DeveloperBuildState {
    transaction: Option<BuildTransaction>,
}

#[derive(Clone, Debug)]
struct CompilerConfig {
    compiler: PathBuf,
    sysroot: PathBuf,
    crt0: PathBuf,
    runtime: PathBuf,
    linker_script: PathBuf,
}

impl CompilerConfig {
    fn from_environment() -> Self {
        let sdk = env::var_os("MBOOT_MOCHIOS_SDK")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/lib/mochios-sdk"));
        Self {
            compiler: env::var_os("MBOOT_MOCHIOS_GCC")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("x86_64-elf-gcc")),
            sysroot: env::var_os("MBOOT_MOCHIOS_SYSROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| sdk.join("x86_64-elf")),
            crt0: env::var_os("MBOOT_MOCHIOS_CRT0")
                .map(PathBuf::from)
                .unwrap_or_else(|| sdk.join("crt0.o")),
            runtime: env::var_os("MBOOT_MOCHIOS_RUNTIME")
                .map(PathBuf::from)
                .unwrap_or_else(|| sdk.join("libmochi_user_newlib_runtime.a")),
            linker_script: env::var_os("MBOOT_MOCHIOS_LINKER_SCRIPT")
                .map(PathBuf::from)
                .unwrap_or_else(|| sdk.join("linker.ld")),
        }
    }
}

impl DeveloperBuildState {
    pub(crate) fn begin(&mut self, id: u64, size: u64) -> Result<(), DeveloperError> {
        if self.transaction.is_some() {
            return Err(DeveloperError::Busy);
        }
        let size = usize::try_from(size).map_err(|_| DeveloperError::InvalidArgument)?;
        if id == 0 || size == 0 || size > MAX_SOURCE_SIZE {
            return Err(DeveloperError::InvalidArgument);
        }
        self.transaction = Some(BuildTransaction {
            id,
            source: Vec::with_capacity(size),
            expected_size: size,
            output: Vec::new(),
            diagnostics: Vec::new(),
            compile_status: None,
        });
        Ok(())
    }

    pub(crate) fn append_chunk(
        &mut self,
        id: u64,
        offset: u64,
        encoded: &str,
    ) -> Result<(), DeveloperError> {
        let transaction = self.transaction_mut(id)?;
        let offset = usize::try_from(offset).map_err(|_| DeveloperError::InvalidArgument)?;
        if offset != transaction.source.len() {
            return Err(DeveloperError::InvalidArgument);
        }
        let bytes = decode_hex(encoded)?;
        if bytes.is_empty()
            || bytes.len() > MAX_CHUNK_SIZE
            || transaction.source.len().saturating_add(bytes.len()) > transaction.expected_size
        {
            return Err(DeveloperError::InvalidArgument);
        }
        transaction.source.extend_from_slice(&bytes);
        Ok(())
    }

    pub(crate) fn compile(&mut self, id: u64) -> Result<CompileResult, DeveloperError> {
        let transaction = self.transaction_mut(id)?;
        if transaction.source.len() != transaction.expected_size
            || transaction.compile_status.is_some()
        {
            return Err(DeveloperError::InvalidState);
        }
        let config = CompilerConfig::from_environment();
        let build_number = BUILD_COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = env::temp_dir().join(format!(
            "mbootd-developer-{}-{build_number}",
            std::process::id()
        ));
        fs::create_dir(&directory).map_err(|_| DeveloperError::Internal)?;
        let result = compile_in_directory(&config, &directory, &transaction.source);
        let _ = fs::remove_dir_all(&directory);
        let (status, output, diagnostics) = result.map_err(|_| DeveloperError::Internal)?;
        transaction.compile_status = Some(status);
        transaction.output = output;
        transaction.diagnostics = diagnostics;
        Ok(CompileResult {
            status,
            output_size: transaction.output.len(),
            diagnostics_size: transaction.diagnostics.len(),
        })
    }

    pub(crate) fn read(
        &self,
        id: u64,
        stream: &str,
        offset: u64,
        maximum: u64,
    ) -> Result<ReadResult, DeveloperError> {
        let transaction = self.transaction(id)?;
        if transaction.compile_status.is_none() {
            return Err(DeveloperError::InvalidState);
        }
        let contents = match stream {
            "output" => &transaction.output,
            "diagnostics" => &transaction.diagnostics,
            _ => return Err(DeveloperError::InvalidArgument),
        };
        let offset = usize::try_from(offset).map_err(|_| DeveloperError::InvalidArgument)?;
        let maximum = usize::try_from(maximum)
            .map_err(|_| DeveloperError::InvalidArgument)?
            .min(MAX_CHUNK_SIZE);
        if maximum == 0 || offset > contents.len() {
            return Err(DeveloperError::InvalidArgument);
        }
        let end = offset.saturating_add(maximum).min(contents.len());
        Ok(ReadResult {
            total_size: contents.len(),
            data: contents[offset..end].to_vec(),
        })
    }

    pub(crate) fn cancel(&mut self, id: u64) -> Result<(), DeveloperError> {
        if self.transaction(id).is_err() {
            return Err(DeveloperError::InvalidState);
        }
        self.transaction = None;
        Ok(())
    }

    fn transaction(&self, id: u64) -> Result<&BuildTransaction, DeveloperError> {
        self.transaction
            .as_ref()
            .filter(|transaction| transaction.id == id)
            .ok_or(DeveloperError::InvalidState)
    }

    fn transaction_mut(&mut self, id: u64) -> Result<&mut BuildTransaction, DeveloperError> {
        self.transaction
            .as_mut()
            .filter(|transaction| transaction.id == id)
            .ok_or(DeveloperError::InvalidState)
    }
}

pub(crate) struct CompileResult {
    pub(crate) status: i32,
    pub(crate) output_size: usize,
    pub(crate) diagnostics_size: usize,
}

pub(crate) struct ReadResult {
    pub(crate) total_size: usize,
    pub(crate) data: Vec<u8>,
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn decode_hex(encoded: &str) -> Result<Vec<u8>, DeveloperError> {
    if encoded.len() % 2 != 0 {
        return Err(DeveloperError::InvalidArgument);
    }
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_digit(pair[0]).ok_or(DeveloperError::InvalidArgument)?;
            let low = hex_digit(pair[1]).ok_or(DeveloperError::InvalidArgument)?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn compile_in_directory(
    config: &CompilerConfig,
    directory: &Path,
    source: &[u8],
) -> io::Result<(i32, Vec<u8>, Vec<u8>)> {
    let source_path = directory.join("main.c");
    let output_path = directory.join("program.elf");
    fs::write(&source_path, source)?;
    let output = Command::new(&config.compiler)
        .arg(format!("--sysroot={}", config.sysroot.display()))
        .arg("-isystem")
        .arg(config.sysroot.join("include"))
        .args([
            "-ffreestanding",
            "-O2",
            "-static",
            "-nostdlib",
            "-nostartfiles",
        ])
        .arg(format!("-Wl,-T,{}", config.linker_script.display()))
        .args(["-Wl,-no-pie", "-Wl,-z,noexecstack"])
        .arg("-Wl,--start-group")
        .arg(&config.crt0)
        .arg(&source_path)
        .arg(&config.runtime)
        .args(["-lc", "-lm", "-lgcc", "-Wl,--end-group"])
        .arg("-o")
        .arg(&output_path)
        .output()?;
    let status = output.status.code().unwrap_or(1);
    let mut diagnostics = output.stderr;
    diagnostics.extend_from_slice(&output.stdout);
    diagnostics.truncate(MAX_DIAGNOSTICS_SIZE);
    let binary = if status == 0 {
        let binary = fs::read(output_path)?;
        if binary.len() > MAX_OUTPUT_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "compiler output too large",
            ));
        }
        binary
    } else {
        Vec::new()
    };
    Ok((status, binary, diagnostics))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_requires_contiguous_bounded_chunks() {
        let mut state = DeveloperBuildState::default();
        state.begin(7, 4).unwrap();
        state.append_chunk(7, 0, "6162").unwrap();
        assert_eq!(
            state.append_chunk(7, 3, "63"),
            Err(DeveloperError::InvalidArgument)
        );
        state.append_chunk(7, 2, "6364").unwrap();
    }

    #[test]
    fn hex_round_trip() {
        let bytes = [0, 1, 0x7f, 0x80, 0xff];
        assert_eq!(decode_hex(&encode_hex(&bytes)).unwrap(), bytes);
    }
}
