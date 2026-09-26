//! `iem-migrate`: see `iem-migrate --help` and the library docs.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match iem_migrate::run(&args) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(f) => {
            eprintln!("iem-migrate: {}", f.msg);
            ExitCode::from(f.code)
        }
    }
}
