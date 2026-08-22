use mbootd::{run, run_x11_proxy_helper, DEFAULT_SOCKET_PATH, X11_PROXY_HELPER_ARGUMENT};
use std::env;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut arguments = env::args().skip(1);
    let first = arguments.next();
    if first.as_deref() == Some(X11_PROXY_HELPER_ARGUMENT) {
        let Some(entrypoint) = arguments.next() else {
            eprintln!("mbootd: X11 proxy helper is missing its entrypoint");
            return ExitCode::FAILURE;
        };
        if arguments.next().is_some() {
            eprintln!("mbootd: X11 proxy helper received unexpected arguments");
            return ExitCode::FAILURE;
        }
        return match run_x11_proxy_helper(&entrypoint) {
            Ok(status) if status.success() => ExitCode::SUCCESS,
            Ok(status) => ExitCode::from(status.code().unwrap_or(1).clamp(1, 255) as u8),
            Err(error) => {
                eprintln!("mbootd: X11 proxy helper: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let path = first.unwrap_or_else(|| DEFAULT_SOCKET_PATH.to_owned());
    match run(Path::new(&path)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mbootd: {error}");
            ExitCode::FAILURE
        }
    }
}
