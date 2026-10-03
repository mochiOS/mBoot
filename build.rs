use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::fs::File;
use std::path::PathBuf;

fn main() {
    build_boot_logo();

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

fn build_boot_logo() {
    const MAX_DIMENSION: u32 = 256;

    println!("cargo:rerun-if-env-changed=MBOOT_UI_LOGO");
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is unavailable"),
    );
    let source = env::var_os("MBOOT_UI_LOGO")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../resources/system/icons/mochimochi-kun.png"));
    println!("cargo:rerun-if-changed={}", source.display());

    let (width, height, pixels) = decode_rgba(&source);
    let longest = width.max(height);
    let (output_width, output_height) = if longest <= MAX_DIMENSION {
        (width, height)
    } else {
        (
            (u64::from(width) * u64::from(MAX_DIMENSION) / u64::from(longest)) as u32,
            (u64::from(height) * u64::from(MAX_DIMENSION) / u64::from(longest)) as u32,
        )
    };
    let resized = resize_rgba(
        &pixels,
        width,
        height,
        output_width.max(1),
        output_height.max(1),
    );

    let output_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is unavailable"));
    fs::write(output_dir.join("boot-logo.rgba"), resized)
        .expect("failed to write converted boot logo");
    fs::write(
        output_dir.join("boot_logo.rs"),
        format!(
            "pub const BOOT_LOGO_WIDTH: u32 = {output_width};\n\
             pub const BOOT_LOGO_HEIGHT: u32 = {output_height};\n\
             pub static BOOT_LOGO_RGBA: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/boot-logo.rgba\"));\n"
        ),
    )
    .expect("failed to write boot logo metadata");
}

fn decode_rgba(path: &PathBuf) -> (u32, u32, Vec<u8>) {
    let mut decoder = png::Decoder::new(File::open(path).expect("failed to open boot logo"));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .expect("failed to read boot logo metadata");
    let mut decoded = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut decoded)
        .expect("failed to decode boot logo");
    let source = &decoded[..info.buffer_size()];
    let pixel_count = usize::try_from(info.width)
        .expect("boot logo width is too large")
        .checked_mul(usize::try_from(info.height).expect("boot logo height is too large"))
        .expect("boot logo dimensions overflow");
    let mut rgba = Vec::with_capacity(pixel_count * 4);

    match info.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(source),
        png::ColorType::Rgb => {
            for pixel in source.chunks_exact(3) {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for pixel in source.chunks_exact(2) {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
        }
        png::ColorType::Grayscale => {
            for value in source {
                rgba.extend_from_slice(&[*value, *value, *value, 255]);
            }
        }
        png::ColorType::Indexed => panic!("boot logo palette was not expanded"),
    }
    assert_eq!(rgba.len(), pixel_count * 4, "invalid boot logo data");
    (info.width, info.height, rgba)
}

fn resize_rgba(
    source: &[u8],
    width: u32,
    height: u32,
    output_width: u32,
    output_height: u32,
) -> Vec<u8> {
    if width == output_width && height == output_height {
        return source.to_vec();
    }
    let output_len = usize::try_from(output_width)
        .expect("converted boot logo width is too large")
        .checked_mul(
            usize::try_from(output_height).expect("converted boot logo height is too large"),
        )
        .and_then(|pixels| pixels.checked_mul(4))
        .expect("converted boot logo dimensions overflow");
    let mut output = vec![0; output_len];
    for y in 0..output_height {
        let source_y = sample_coordinate(y, output_height, height);
        for x in 0..output_width {
            let source_x = sample_coordinate(x, output_width, width);
            let pixel = sample_premultiplied(source, width, height, source_x, source_y);
            let offset = (usize::try_from(y).unwrap() * usize::try_from(output_width).unwrap()
                + usize::try_from(x).unwrap())
                * 4;
            output[offset..offset + 4].copy_from_slice(&pixel);
        }
    }
    output
}

fn sample_coordinate(index: u32, output_size: u32, source_size: u32) -> u64 {
    const ONE: u64 = 1 << 16;
    let centered =
        (u64::from(index) * 2 + 1) * u64::from(source_size) * ONE / (u64::from(output_size) * 2);
    centered
        .saturating_sub(ONE / 2)
        .min(u64::from(source_size - 1) * ONE)
}

fn sample_premultiplied(source: &[u8], width: u32, height: u32, x: u64, y: u64) -> [u8; 4] {
    const ONE: u64 = 1 << 16;
    let x0 = u32::try_from(x >> 16).unwrap().min(width - 1);
    let y0 = u32::try_from(y >> 16).unwrap().min(height - 1);
    let x1 = x0.saturating_add(1).min(width - 1);
    let y1 = y0.saturating_add(1).min(height - 1);
    let fraction_x = x & (ONE - 1);
    let fraction_y = y & (ONE - 1);
    let weights = [
        (ONE - fraction_x) * (ONE - fraction_y),
        fraction_x * (ONE - fraction_y),
        (ONE - fraction_x) * fraction_y,
        fraction_x * fraction_y,
    ];
    let coordinates = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
    let mut alpha = 0_u64;
    let mut premultiplied = [0_u64; 3];
    for (weight, (sample_x, sample_y)) in weights.into_iter().zip(coordinates) {
        let offset = (usize::try_from(sample_y).unwrap() * usize::try_from(width).unwrap()
            + usize::try_from(sample_x).unwrap())
            * 4;
        let sample_alpha = u64::from(source[offset + 3]);
        alpha += sample_alpha * weight;
        for channel in 0..3 {
            premultiplied[channel] += u64::from(source[offset + channel]) * sample_alpha * weight;
        }
    }
    let weight_scale = ONE * ONE;
    let output_alpha = ((alpha + weight_scale / 2) / weight_scale).min(255);
    if alpha == 0 {
        return [0, 0, 0, 0];
    }
    [
        ((premultiplied[0] + alpha / 2) / alpha).min(255) as u8,
        ((premultiplied[1] + alpha / 2) / alpha).min(255) as u8,
        ((premultiplied[2] + alpha / 2) / alpha).min(255) as u8,
        output_alpha as u8,
    ]
}
