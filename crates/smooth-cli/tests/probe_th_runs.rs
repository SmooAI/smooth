//! Diagnostic probe (pearl th-fd9d98): does the `th` binary actually run?
//!
//! The Windows CI lane has never executed the built binary, so a startup crash
//! there would be invisible. This asserts the bare minimum — clap's own
//! `--version` / `--help` paths exit 0 and print something — on plain `main`,
//! with no other changes, to establish whether that crash predates the
//! anstream work on the th-fd9d98 branch.

use std::process::Command;

#[test]
fn clap_paths_exit_cleanly() {
    for args in [&["--version"][..], &["--help"][..]] {
        let out = Command::new(env!("CARGO_BIN_EXE_th")).args(args).output().expect("th should be runnable");
        assert!(
            out.status.success(),
            "`th {}` did not exit cleanly ({:?}) — stderr:\n{}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(!out.stdout.is_empty(), "`th {}` produced no stdout", args.join(" "));
    }
}

#[test]
fn a_real_subcommand_runs() {
    let out = Command::new(env!("CARGO_BIN_EXE_th")).args(["auth", "whoami"]).output().expect("th should be runnable");
    assert!(
        !out.stdout.is_empty() || !out.stderr.is_empty(),
        "`th auth whoami` produced no output at all ({:?}) — it likely crashed on startup",
        out.status,
    );
}
