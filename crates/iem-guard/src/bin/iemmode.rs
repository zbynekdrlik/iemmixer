//! `iemmode`: the guard's only client (S6 design note §5.1). The subcommands
//! land with the daemon (S6 plan, Task 10); until then the binary only names
//! itself and refuses to run (exit 2, usage).

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!(
        "iemmode {}: the guard client is not built yet (S6 plan, Task 10)",
        env!("CARGO_PKG_VERSION")
    );
    ExitCode::from(2)
}
