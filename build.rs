//! Compile the vendored xiph RNNoise (BSD) so the app matches the reference
//! denoiser exactly, including its current trained model.

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
}
