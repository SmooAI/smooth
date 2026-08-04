//! `th` must not emit ANSI escapes when stdout is not a terminal.
//!
//! owo-colors styles unconditionally — `.cyan()` has no tty check — so every
//! printer in this crate goes through `anstream`, which strips escapes when
//! stdout is redirected. That is what keeps piped output, `$(…)` capture, and
//! agent hooks (`th pearls prime` under a Claude Code SessionStart hook) from
//! filling up with escape soup.
//!
//! A `Command` always gives the child a pipe, so these runs are non-tty by
//! construction. Colored-on-a-real-tty is covered by `gradient::color_enabled`.

use std::process::Command;

/// Commands that print styled output and touch no pearl store, network, or
/// on-disk state — safe to shell out to in a test.
const READ_ONLY_COMMANDS: &[&[&str]] = &[&["auth", "whoami"], &["--help"], &["pearls", "--help"]];

fn assert_no_escapes(args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_th")).args(args).output().expect("th should be runnable");

    for (stream, bytes) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
        assert!(
            !bytes.contains(&0x1b),
            "`th {}` wrote an ANSI escape to {stream} while piped:\n{}",
            args.join(" "),
            String::from_utf8_lossy(bytes),
        );
    }
}

#[test]
fn read_only_commands_emit_no_ansi_when_piped() {
    for args in READ_ONLY_COMMANDS {
        assert_no_escapes(args);
    }
}

#[test]
fn no_color_env_also_yields_plain_output() {
    let out = Command::new(env!("CARGO_BIN_EXE_th"))
        .args(["auth", "whoami"])
        .env("NO_COLOR", "1")
        .output()
        .expect("th should be runnable");

    assert!(!out.stdout.contains(&0x1b), "NO_COLOR=1 output still contained an escape");
}
