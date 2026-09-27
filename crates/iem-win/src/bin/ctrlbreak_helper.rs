//! Test helper of `tests/ctrl_break.rs` (feature `test-helper`, Windows): it
//! waits on tokio's Ctrl-Break listener as `iem-server` does, prints `ready`
//! once the listener is installed, exits 0 on Ctrl-Break and 3 when none
//! comes within 30 s (so a failed test never leaves it running).

#[cfg(windows)]
fn main() {
    use std::io::Write;
    use std::time::Duration;

    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        std::process::exit(4);
    };
    let code = runtime.block_on(async {
        let Ok(mut ctrl_break) = tokio::signal::windows::ctrl_break() else {
            return 5;
        };
        println!("ready");
        if std::io::stdout().flush().is_err() {
            return 6;
        }
        match tokio::time::timeout(Duration::from_secs(30), ctrl_break.recv()).await {
            Ok(_) => 0,
            Err(_) => 3,
        }
    });
    std::process::exit(code);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("iem-win-ctrlbreak-helper runs on Windows only");
    std::process::exit(2);
}
