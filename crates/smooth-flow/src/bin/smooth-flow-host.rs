//! `smooth-flow-host` — the session host as a standalone binary, for
//! `smooth-flow`'s own integration tests (`tests/session_host.rs`), which
//! can't build `smooth-daemon`. What ships is `smooth-daemon flow-host`;
//! both call `smooth_flow::session_host::server::run`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, id] if flag == "--id" => smooth_flow::session_host::server::run(id),
        _ => {
            eprintln!("usage: smooth-flow-host --id <fs-xxxxxxxx>  (spawn request on stdin)");
            ExitCode::from(2)
        }
    }
}
