//! `iemmixer-guard`: the guard daemon (S6 design note §5.1). The request loop,
//! the switch runner and `install` land with the daemon (S6 plan, Task 10);
//! until then the binary only names itself and refuses to run.

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!(
        "iemmixer-guard {}: the daemon is not built yet (S6 plan, Task 10)",
        env!("CARGO_PKG_VERSION")
    );
    ExitCode::from(2)
}
