use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=DEVELOPER_DIR");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    // The media bridge crates support a full Xcode installation. xcrun also resolves
    // Command Line Tools, whose Swift compatibility archives live under usr/lib.
    if let Ok(output) = Command::new("xcrun").args(["--find", "swiftc"]).output()
        && output.status.success()
    {
        let compiler = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        if let Some(usr) = compiler.parent().and_then(|bin| bin.parent()) {
            let runtime = usr.join("lib/swift/macosx");
            if runtime.is_dir() {
                println!("cargo:rustc-link-search=native={}", runtime.display());
            }
        }
    }
}
