//! Compiles native/cued-audio (C++17 engine + miniaudio in C) into this crate
//! with the `cc` crate, so `cargo build` needs no CMake.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let native = manifest.join("../../native/cued-audio");
    // canonicalize() yields a verbatim `\\?\` path on Windows, which cl.exe cannot open.
    let native = native.canonicalize().map(strip_verbatim).unwrap_or(native);
    let src = native.join("src");
    let include = native.join("include");
    let miniaudio = native.join("third_party/miniaudio");

    println!("cargo:rerun-if-changed={}", src.display());
    println!("cargo:rerun-if-changed={}", include.display());
    println!("cargo:rerun-if-changed={}", miniaudio.join("miniaudio.h").display());

    // Every .cpp in native/cued-audio/src, sorted for reproducible builds.
    let mut cpp_files: Vec<PathBuf> = std::fs::read_dir(&src)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", src.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "cpp").unwrap_or(false))
        .collect();
    cpp_files.sort();
    assert!(!cpp_files.is_empty(), "no C++ sources found in {}", src.display());

    let mut cxx = cc::Build::new();
    cxx.cpp(true)
        .std("c++17")
        .include(&include)
        .include(&src)
        .include(&miniaudio)
        .files(&cpp_files)
        .warnings(true)
        .extra_warnings(true)
        .flag_if_supported("-Wpedantic")
        .flag_if_supported("/EHsc")
        .opt_level(2);
    for f in &cpp_files {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    cxx.compile("cued_audio_cxx");

    // miniaudio: a single C file; third-party, so its warnings stay out of the log.
    let mut c = cc::Build::new();
    c.file(src.join("miniaudio_impl.c"))
        .include(&miniaudio)
        .warnings(false)
        .flag_if_supported("-w")
        .opt_level(2);
    c.compile("cued_audio_miniaudio");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "linux" | "android" => {
            println!("cargo:rustc-link-lib=dl");
            println!("cargo:rustc-link-lib=pthread");
            println!("cargo:rustc-link-lib=m");
        }
        "macos" | "ios" => {
            println!("cargo:rustc-link-lib=pthread");
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
            println!("cargo:rustc-link-lib=framework=CoreAudio");
            println!("cargo:rustc-link-lib=framework=AudioToolbox");
        }
        "windows" => {
            println!("cargo:rustc-link-lib=ole32");
        }
        _ => {
            println!("cargo:rustc-link-lib=pthread");
            println!("cargo:rustc-link-lib=m");
        }
    }
}

/// `\\?\C:\...` → `C:\...`; a no-op on every other platform.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest.strip_prefix("UNC\\").map(|r| format!("\\\\{r}")).unwrap_or_else(|| rest.to_string())),
        None => p,
    }
}
