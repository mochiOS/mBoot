use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=MBOOT_LAUNCH_MANIFEST");
    let digest = if env::var_os("CARGO_FEATURE_UEFI_APP").is_some() {
        let path = env::var("MBOOT_LAUNCH_MANIFEST")
            .expect("MBOOT_LAUNCH_MANIFEST is required for the UEFI binary");
        println!("cargo:rerun-if-changed={path}");
        Sha256::digest(fs::read(path).expect("failed to read the mBoot Launch Manifest")).into()
    } else {
        [0; 32]
    };

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is unavailable"))
        .join("launch_manifest_digest.rs");
    fs::write(
        output,
        format!("pub const EMBEDDED_LAUNCH_MANIFEST_SHA256: [u8; 32] = {digest:?};\n"),
    )
    .expect("failed to write the embedded Launch Manifest digest");
}
