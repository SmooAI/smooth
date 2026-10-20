//! `th --version` and `th --help` must exit cleanly on every OS (th-bd84cf).
//!
//! Windows gives the process main thread a 1 MB stack, and rendering clap's
//! help/version for th's 53-command tree overflowed it: the child died with
//! 0xC00000FD before writing a byte. `main` now runs everything on an 8 MB
//! thread. These tests run the real binary, so the Windows CI lane is the
//! actual regression check — they must never be `#[ignore]`d there.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_th")).args(args).output().expect("th should be runnable")
}

fn assert_clean_exit(args: &[&str]) -> Output {
    let out = run(args);
    assert!(
        out.status.success(),
        "`th {}` did not exit cleanly ({:?}) — stderr:\n{}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(!out.stdout.is_empty(), "`th {}` produced no stdout", args.join(" "));
    out
}

#[test]
fn version_exits_cleanly() {
    for args in [&["--version"][..], &["-V"][..]] {
        let out = assert_clean_exit(args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(env!("CARGO_PKG_VERSION")),
            "`th {}` printed no version: {stdout}",
            args.join(" ")
        );
    }
}

#[test]
fn help_exits_cleanly() {
    for args in [&["--help"][..], &["-h"][..], &["help"][..]] {
        assert_clean_exit(args);
    }
}

/// `--help-full` falls through to clap's native renderer over the whole tree,
/// the deepest recursion `th` does.
#[test]
fn full_clap_help_exits_cleanly() {
    assert_clean_exit(&["--help-full"]);
}

#[test]
fn subcommand_help_exits_cleanly() {
    assert_clean_exit(&["pearls", "--help"]);
}

/// Errors from the runner thread still reach the process exit code: an
/// unknown subcommand is clap's usage error, exit 2.
#[test]
fn usage_errors_keep_their_exit_code() {
    let out = run(&["definitely-not-a-command"]);
    assert_eq!(out.status.code(), Some(2), "stderr:\n{}", String::from_utf8_lossy(&out.stderr));
}
