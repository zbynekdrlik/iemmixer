fn main() {
    // Pass git hash to compiler
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .expect("git rev-parse failed");
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    println!("cargo:rustc-env=GIT_HASH={}", hash);

    // Pass build time
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time went backwards")
        .as_secs();
    println!("cargo:rustc-env=BUILD_TIME={}", now);

    // Rebuild if git HEAD changes (path relative to repo root, not crate dir)
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let git_head = std::path::Path::new(&manifest_dir).join("../../.git/HEAD");
    println!("cargo:rerun-if-changed={}", git_head.display());

    tauri_build::build()
}
