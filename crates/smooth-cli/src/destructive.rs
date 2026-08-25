//! One gate in front of every irreversible remote delete/revoke (pearl
//! th-db25d4 item 8).
//!
//! Before this, `th api teams delete`, `th api knowledge delete`,
//! `th config admin schemas delete` and six siblings issued their DELETE on
//! the first keystroke, against `https://api.smoo.ai` — production — using
//! whichever org id happened to be *persisted* as active. `active_org`
//! resolution falls back to a value written to disk by an `orgs switch` that
//! could have been hours and several contexts ago, and nothing on the way to
//! the wire named it. An operator who switched orgs after lunch could delete a
//! production config environment, and every value under it, without ever
//! seeing which org they were pointed at.
//!
//! The fix is deliberately boring and uniform:
//!
//! 1. **Always print the target first** — org, base URL, and exactly what is
//!    about to go. This is the part that catches the wrong-org mistake, and it
//!    happens even under `--yes`, so automation still leaves a record.
//! 2. **`--dry-run` stops here.** Showing precisely what would be deleted is
//!    worth more than a y/n nobody reads.
//! 3. **Fail closed.** Non-interactive (a script, CI, a piped shell) with no
//!    explicit `--yes` refuses. There is no way to confirm on a pipe, so the
//!    safe answer is the only answer.
//! 4. **Interactive confirms, defaulting to no.** [`Severity::Irreversible`]
//!    targets — the ones whose doc comments say the data does not come back —
//!    make you type the target's own name, so the confirmation cannot be
//!    walked past with a reflex `<enter>`.
//!
//! The decision itself is [`decide`], a pure function, so the matrix is unit
//! tested without a TTY or a live API (same split as `config.rs`'s
//! `credential_tripwire_decision`).

use anstream::{eprintln, println};
use anyhow::{Context, Result};
use owo_colors::OwoColorize;

/// The `--dry-run` / `--yes` pair, flattened into every destructive
/// subcommand so the flags read identically everywhere and a new delete verb
/// gets them by adding one line rather than six.
#[derive(Debug, Clone, Copy, Default, clap::Args)]
pub struct Confirm {
    /// Print the target and exit without deleting.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip the interactive confirmation. Required in scripts/CI.
    #[arg(long)]
    pub yes: bool,
}

/// How much friction the confirmation deserves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The resource can be recreated (a team, a role, a replacement key).
    /// A yes/no confirmation is enough.
    Standard,
    /// The data does not come back — config schemas and environments take
    /// every value stored under them, and knowledge documents are deleted
    /// outright. Requires typing the target back.
    Irreversible,
}

/// What is about to be destroyed, in the words the operator needs to
/// recognise it — including the org, which is the field they get wrong.
pub struct Target<'a> {
    /// "delete", "revoke", …
    pub verb: &'a str,
    /// "team", "config environment", "auth client", …
    pub noun: &'a str,
    /// The id or name being destroyed. For [`Severity::Irreversible`] this is
    /// also what the operator must type back.
    pub id: &'a str,
    /// The resolved org id this DELETE will be issued against.
    pub org: &'a str,
    pub severity: Severity,
}

/// The four ways a destructive request can resolve. Pure, so the matrix is
/// testable without a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// `--dry-run`: report the target, change nothing.
    DryRun,
    /// `--yes` on the command line: the operator already said so.
    Proceed,
    /// Interactive: ask.
    Prompt,
    /// Non-interactive and unconfirmed. Fail closed.
    Refuse,
}

/// `--dry-run` beats everything (it is the "show me" flag, and honouring it
/// even alongside `--yes` means a mistyped script prints instead of deletes).
/// Then an explicit `--yes`. Then a TTY can be asked. Anything else refuses.
#[must_use]
pub const fn decide(dry_run: bool, yes: bool, is_tty: bool) -> Decision {
    if dry_run {
        Decision::DryRun
    } else if yes {
        Decision::Proceed
    } else if is_tty {
        Decision::Prompt
    } else {
        Decision::Refuse
    }
}

/// The API host the DELETE will hit. Mirrors the resolution in `config.rs` so
/// the banner never claims prod while the client talks to a local stack.
fn base_url() -> String {
    std::env::var("SMOOAI_API_URL").unwrap_or_else(|_| "https://api.smoo.ai".to_string())
}

/// Print the target banner. Runs on every path, `--yes` included — an
/// unattended run should still leave the org it acted on in the log.
fn announce(t: &Target) {
    println!();
    println!(
        "  {} {} {} {}",
        "about to".red().bold(),
        t.verb.red().bold(),
        t.noun.dimmed(),
        t.id.cyan().bold()
    );
    println!("    {}  {}", "org ".dimmed(), t.org.yellow());
    println!("    {}  {}", "host".dimmed(), base_url().dimmed());
    if t.severity == Severity::Irreversible {
        println!("    {}  {}", "note".dimmed(), "irreversible — this data does not come back".red());
    }
    println!();
}

/// [`gate`] taking the flattened [`Confirm`] flags.
///
/// # Errors
/// See [`gate`].
pub fn gate_with(t: &Target, c: Confirm) -> Result<bool> {
    gate(t, c.dry_run, c.yes)
}

/// Shorthand for the common shape: `verb`-a-`noun`-by-id in an org.
///
/// # Errors
/// See [`gate`].
pub fn confirm_delete(noun: &str, id: &str, org: &str, c: Confirm) -> Result<bool> {
    gate_with(
        &Target {
            verb: "delete",
            noun,
            id,
            org,
            severity: Severity::Standard,
        },
        c,
    )
}

/// Gate a destructive remote call.
///
/// Returns `Ok(true)` when the caller should proceed, `Ok(false)` for a
/// dry run (nothing to do, exit success), and `Err` when the operator
/// declined or could not be asked.
///
/// # Errors
/// - Non-interactive without `--yes` (fail closed)
/// - The operator answered no, or mistyped the confirmation token
/// - Reading the confirmation from the terminal failed
pub fn gate(t: &Target, dry_run: bool, yes: bool) -> Result<bool> {
    use std::io::IsTerminal;
    announce(t);
    match decide(dry_run, yes, std::io::stdin().is_terminal()) {
        Decision::DryRun => {
            println!("  {} dry-run — nothing was {}d", "●".dimmed(), t.verb);
            println!();
            Ok(false)
        }
        Decision::Proceed => Ok(true),
        Decision::Prompt => {
            let confirmed = match t.severity {
                Severity::Standard => dialoguer::Confirm::new()
                    .with_prompt(format!("{} {} `{}` in org {}?", t.verb, t.noun, t.id, t.org))
                    .default(false)
                    .interact()
                    .context("read delete confirmation")?,
                Severity::Irreversible => {
                    let typed: String = dialoguer::Input::new()
                        .with_prompt(format!("type `{}` to confirm (anything else aborts)", t.id))
                        .allow_empty(true)
                        .interact_text()
                        .context("read delete confirmation")?;
                    typed.trim() == t.id
                }
            };
            if confirmed {
                Ok(true)
            } else {
                eprintln!("  {} aborted — nothing was {}d", "✗".yellow(), t.verb);
                anyhow::bail!("aborted by operator")
            }
        }
        Decision::Refuse => anyhow::bail!(
            "refusing to {} {} `{}` in org {} without confirmation: not a terminal, so there is no way to ask. \
             Re-run with `--dry-run` to see the target, or `--yes` to confirm you mean this org.",
            t.verb,
            t.noun,
            t.id,
            t.org
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole matrix. The row that matters is the last one: a script or CI
    /// job with no `--yes` must NOT delete.
    #[test]
    fn decide_matrix() {
        // dry-run wins over everything, including --yes.
        assert_eq!(decide(true, false, true), Decision::DryRun);
        assert_eq!(decide(true, true, false), Decision::DryRun);
        // explicit --yes proceeds with or without a terminal.
        assert_eq!(decide(false, true, false), Decision::Proceed);
        assert_eq!(decide(false, true, true), Decision::Proceed);
        // a terminal gets asked.
        assert_eq!(decide(false, false, true), Decision::Prompt);
        // no terminal, no --yes: fail closed.
        assert_eq!(decide(false, false, false), Decision::Refuse);
    }

    /// Fail CLOSED, not open — the defect this gate exists to prevent is a
    /// delete that runs because nothing stopped it.
    #[test]
    fn non_interactive_unconfirmed_never_proceeds() {
        assert_ne!(decide(false, false, false), Decision::Proceed);
    }
}
