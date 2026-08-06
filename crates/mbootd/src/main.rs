use mbootd::{run, DEFAULT_SOCKET_PATH};
use std::env;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let path = env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_SOCKET_PATH.to_owned());
    match run(Path::new(&path)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mbootd: {error}");
            ExitCode::FAILURE
        }
    }
}
