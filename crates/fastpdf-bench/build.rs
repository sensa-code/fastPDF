// Records the compiler version and build profile in corpus reports, so
// benchmark runs from different toolchains are never compared unknowingly.

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let version = std::process::Command::new(rustc)
        .arg("-V")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    println!("cargo:rustc-env=FASTPDF_RUSTC_VERSION={}", version.trim());
    let profile = std::env::var("PROFILE").unwrap_or_default();
    println!("cargo:rustc-env=FASTPDF_BUILD_PROFILE={profile}");
    println!("cargo:rerun-if-changed=build.rs");
}
