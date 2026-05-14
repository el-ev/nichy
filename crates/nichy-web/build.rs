use std::process::Command;

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let version = Command::new(&rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "rustc (unknown)".into());
    println!("cargo:rustc-env=NICHY_RUSTC_VERSION={version}");

    let rust_root = std::env::var("RUST_ROOT").ok();
    let rustc_hash = rust_root
        .as_ref()
        .and_then(|root| {
            Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .current_dir(root)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default();
    println!("cargo:rustc-env=NICHY_RUSTC_HASH={rustc_hash}");
}
