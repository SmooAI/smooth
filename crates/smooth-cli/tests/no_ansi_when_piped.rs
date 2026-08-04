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
//!
//! Every case asserts the child actually *ran* before asserting on its bytes.
//! A process that dies on startup writes nothing, and "nothing" trivially
//! contains no escape sequences — without a liveness assertion these tests
//! would go green against a `th` that crashes on every invocation.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_th")).args(args).output().expect("th should be runnable")
}

fn assert_no_escapes(args: &[&str], out: &Output) {
    for (stream, bytes) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
        assert!(
            !bytes.contains(&0x1b),
            "`th {}` wrote an ANSI escape to {stream} while piped:\n{}",
            args.join(" "),
            String::from_utf8_lossy(bytes),
        );
    }
}

/// Clap-internal commands: they exit 0 and print, so a crash is unmistakable.
///
/// Ignored on Windows for pearl th-bd84cf, a pre-existing bug unrelated to
/// ANSI: rendering help/version for th's 53-command clap tree overflows the
/// 1 MB Windows main-thread stack, so the child dies before writing anything.
/// Reproduced on plain `main` (PR #331 probe) — un-ignore once th-bd84cf lands.
#[test]
#[cfg_attr(windows, ignore = "pearl th-bd84cf: clap help/version overflows the 1 MB Windows main stack")]
fn clap_output_is_plain_and_the_process_exits_cleanly() {
    for args in [&["--version"][..], &["--help"][..], &["pearls", "--help"][..]] {
        let out = run(args);
        assert!(
            out.status.success(),
            "`th {}` did not exit cleanly ({:?}) — stderr:\n{}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(!out.stdout.is_empty(), "`th {}` produced no stdout", args.join(" "));
        assert_no_escapes(args, &out);
    }
}

/// `auth whoami` is one of this crate's own owo-colors printers (clap's help
/// and version go through clap's formatter, not ours). Its exit status depends
/// on whether credentials exist, so assert liveness via output instead.
#[test]
fn our_own_printers_are_plain_when_piped() {
    let args = &["auth", "whoami"][..];
    let out = run(args);
    assert!(
        !out.stdout.is_empty() || !out.stderr.is_empty(),
        "`th auth whoami` produced no output at all ({:?}) — it likely crashed on startup",
        out.status,
    );
    assert_no_escapes(args, &out);
}

#[test]
fn no_color_env_also_yields_plain_output() {
    let out = Command::new(env!("CARGO_BIN_EXE_th"))
        .args(["auth", "whoami"])
        .env("NO_COLOR", "1")
        .output()
        .expect("th should be runnable");

    assert!(
        !out.stdout.is_empty() || !out.stderr.is_empty(),
        "`th auth whoami` produced no output at all ({:?}) — it likely crashed on startup",
        out.status,
    );
    assert!(!out.stdout.contains(&0x1b), "NO_COLOR=1 output still contained an escape");
}
