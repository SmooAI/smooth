//! Read a secret without it ever touching argv (SMOODEV-3606).
//!
//! A value passed as a command-line argument is visible to every user on the
//! box through `ps`, lands in shell history, and is copied verbatim into agent
//! transcripts and CI logs. So every `th` flag that carries a password, client
//! secret, or secret-tier config value reads it from one of three places
//! instead: a masked TTY prompt, stdin, or a file.
//!
//! Piped input is the scripting path (`op read … | th smoo config set KEY
//! --value-stdin`). It strips exactly the trailing newline a pipe adds — `echo`
//! appends one, and a stored `secret\n` silently breaks every consumer that
//! compares bytes (the CLAUDE.md §13 `gh secret set` incident) — and refuses
//! an empty value rather than storing nothing.

use std::io::{IsTerminal, Read};

use anyhow::{bail, Context, Result};

/// Strip the trailing line ending a pipe or a text editor adds, and refuse an
/// empty result. Interior whitespace and newlines are kept: a PEM key is
/// multi-line on purpose.
///
/// # Errors
/// When the value is empty or whitespace-only.
pub fn normalize(raw: String, what: &str) -> Result<String> {
    let trimmed = raw.strip_suffix("\r\n").or_else(|| raw.strip_suffix('\n')).unwrap_or(&raw);
    if trimmed.trim().is_empty() {
        bail!("{what} is empty — nothing was read");
    }
    Ok(trimmed.to_string())
}

/// Read the whole of stdin as a secret. Refuses when stdin is a terminal:
/// "waiting on a TTY for EOF" looks exactly like a hang, and the caller has a
/// masked prompt for that case.
///
/// # Errors
/// When stdin is a terminal, unreadable, or empty.
pub fn from_stdin(what: &str) -> Result<String> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        bail!("{what}: stdin is a terminal — pipe the value in (e.g. `… | th …`), or omit the flag to be prompted");
    }
    let mut s = String::new();
    stdin.read_to_string(&mut s).with_context(|| format!("read {what} from stdin"))?;
    normalize(s, what)
}

/// Read a secret from a file.
///
/// # Errors
/// When the file cannot be read or is empty.
pub fn from_file(path: &std::path::Path, what: &str) -> Result<String> {
    let s = std::fs::read_to_string(path).with_context(|| format!("read {what} from {}", path.display()))?;
    normalize(s, what)
}

/// Prompt on the terminal. `masked` hides the input (no echo).
///
/// # Errors
/// When stdin is not a terminal (nothing to prompt on), or the read fails.
pub fn prompt(label: &str, masked: bool) -> Result<String> {
    if !std::io::stdin().is_terminal() {
        bail!("{label}: no value given and stdin is not a terminal — pass it on stdin with the `-stdin` flag, or from a file");
    }
    let theme = dialoguer::theme::ColorfulTheme::default();
    let raw = if masked {
        dialoguer::Password::with_theme(&theme).with_prompt(label).interact()
    } else {
        dialoguer::Input::<String>::with_theme(&theme).with_prompt(label).interact_text()
    }
    .with_context(|| format!("prompt for {label}"))?;
    normalize(raw, label)
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn strips_exactly_one_trailing_newline() {
        assert_eq!(normalize("s3cret\n".into(), "v").unwrap(), "s3cret");
        assert_eq!(normalize("s3cret\r\n".into(), "v").unwrap(), "s3cret");
        assert_eq!(normalize("s3cret".into(), "v").unwrap(), "s3cret");
        // A multi-line secret keeps its interior newlines (and only loses the last).
        assert_eq!(normalize("a\nb\n".into(), "v").unwrap(), "a\nb");
    }

    #[test]
    fn refuses_empty_input() {
        assert!(normalize(String::new(), "v").is_err());
        assert!(normalize("\n".into(), "v").is_err());
        assert!(normalize("   \n".into(), "v").is_err());
    }
}
