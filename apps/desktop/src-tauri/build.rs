fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
    let mut attributes = tauri_build::Attributes::new();
    // On Windows the app manifest (Common Controls v6, per-monitor DPI awareness) is embedded through
    // the linker so every binary of this crate gets it, test binaries included: tauri-build embeds
    // it only into the app, and a test that links tauri then fails to start with
    // STATUS_ENTRYPOINT_NOT_FOUND.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        attributes = attributes.windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
        let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        let manifest = std::path::Path::new(&dir).join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    }
    tauri_build::try_build(attributes).expect("tauri build");
}
