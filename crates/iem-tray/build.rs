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

    // Rebuild when git HEAD moves. Without a `.git` (cargo-mutants' scratch
    // copy, a source archive) key the rerun on the CI commit instead, so
    // BUILD_TIME does not change on every build and force a full rebuild.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let git_head = std::path::Path::new(&manifest_dir).join("../../.git/HEAD");
    if git_head.exists() {
        println!("cargo:rerun-if-changed={}", git_head.display());
    } else {
        println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    }

    tauri_build::build()
}
