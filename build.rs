//! Compile the vendored xiph RNNoise (BSD) so the app matches the reference
//! denoiser exactly, including its current trained model. On Windows, also
//! embed the app icon in the exe: the window icon set at runtime doesn't
//! reach Explorer or pinned taskbar shortcuts, which read the exe's resources.

#[allow(dead_code)]
#[path = "src/icon.rs"]
mod icon;

fn main() {
    let mut build = cc::Build::new();
    // MSVC can't compile the VLAs in pitch.c; clang (e.g. the one shipped
    // with ROCm) targets the same MSVC ABI and accepts them.
    build.compiler("clang");
    build
        .define("_USE_MATH_DEFINES", None)
        .include("vendor/rnnoise/src")
        .include("vendor/rnnoise/include")
        .files([
            "vendor/rnnoise/src/celt_lpc.c",
            "vendor/rnnoise/src/denoise.c",
            "vendor/rnnoise/src/kiss_fft.c",
            "vendor/rnnoise/src/parse_lpcnet_weights.c",
            "vendor/rnnoise/src/pitch.c",
            "vendor/rnnoise/src/rnn.c",
            "vendor/rnnoise/src/rnnoise_data.c",
            "vendor/rnnoise/src/rnnoise_tables.c",
            "vendor/rnnoise/src/nnet.c",
            "vendor/rnnoise/src/nnet_default.c",
        ])
        .warnings(false)
        .flag("-Wno-everything");
    build.compile("rnnoise");
    println!("cargo:rerun-if-changed=vendor/rnnoise/src");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let ico = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("openmic.ico");
        std::fs::write(&ico, ico_file(&[16, 20, 24, 32, 40, 48, 64, 256])).unwrap();
        winresource::WindowsResource::new()
            .set_icon(ico.to_str().unwrap())
            .compile()
            .expect("embed the app icon");
        println!("cargo:rerun-if-changed=src/icon.rs");
    }
}

/// An .ico holding the unmuted icon at each size, as 32-bit BMP images.
fn ico_file(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| bmp_image(s)).collect();
    let mut out = Vec::new();
    out.extend(0u16.to_le_bytes()); // reserved
    out.extend(1u16.to_le_bytes()); // type: icon
    out.extend((sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&size, image) in sizes.iter().zip(&images) {
        let dim = if size >= 256 { 0 } else { size as u8 }; // 0 means 256
        out.extend([dim, dim, 0, 0]); // width, height, palette, reserved
        out.extend(1u16.to_le_bytes()); // planes
        out.extend(32u16.to_le_bytes()); // bits per pixel
        out.extend((image.len() as u32).to_le_bytes());
        out.extend(offset.to_le_bytes());
        offset += image.len() as u32;
    }
    images.iter().for_each(|image| out.extend(image));
    out
}

/// A BITMAPINFOHEADER, bottom-up BGRA pixels, then an empty AND mask (the
/// alpha channel already carries transparency).
fn bmp_image(size: u32) -> Vec<u8> {
    let rgba = icon::rgba(size, false);
    let mask_row = size.div_ceil(32) * 4;
    let pixel_bytes = size * size * 4 + mask_row * size;
    let mut out = Vec::with_capacity(40 + pixel_bytes as usize);
    out.extend(40u32.to_le_bytes());
    out.extend((size as i32).to_le_bytes());
    out.extend((2 * size as i32).to_le_bytes()); // colour rows + mask rows
    out.extend(1u16.to_le_bytes());
    out.extend(32u16.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // BI_RGB
    out.extend(pixel_bytes.to_le_bytes());
    out.extend([0; 16]); // resolution and palette counts
    for row in rgba.chunks_exact(size as usize * 4).rev() {
        for p in row.as_chunks::<4>().0 {
            out.extend([p[2], p[1], p[0], p[3]]);
        }
    }
    out.resize(out.len() + (mask_row * size) as usize, 0);
    out
}
