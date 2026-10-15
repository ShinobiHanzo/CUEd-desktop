fn main() {
    // The target triple, so the updater can pick its own release archive.
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=CUED_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../CHANGELOG.md");
}
