fn main() {
    // Pass git hash to compiler
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .expect("git rev-parse failed");
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    println!("cargo:rustc-env=GIT_HASH={}", hash);

    // Pass git branch name to compiler
    // CI sets GITHUB_REF_NAME (checkout is detached HEAD, so git command returns "HEAD")
    let branch = std::env::var("GITHUB_REF_NAME").unwrap_or_else(|_| {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        if output == "HEAD" {
            "unknown".to_string()
        } else {
            output
        }
    });
    println!("cargo:rustc-env=GIT_BRANCH={}", branch);

    // Pass build time as unix timestamp
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time went backwards")
        .as_secs();
    println!("cargo:rustc-env=BUILD_TIME={}", now);

    // Rebuild if git HEAD changes (path relative to repo root, not crate dir)
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let git_head = std::path::Path::new(&manifest_dir).join("../../.git/HEAD");
    println!("cargo:rerun-if-changed={}", git_head.display());
}
