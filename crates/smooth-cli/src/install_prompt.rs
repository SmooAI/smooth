//! `th install-prompt [--copy]` — the prompt that installs `th` into a coding
//! agent (SMOODEV-3915).
//!
//! The canonical text lives in `docs/Start-Here/Install-Prompt.md`, the page
//! the README links to. It is embedded at compile time and the first fenced
//! `text` block is extracted, so the CLI and the docs can never disagree.

use anstream::eprintln;
use anyhow::{anyhow, Result};
use owo_colors::OwoColorize;

/// The docs page, embedded at compile time.
const INSTALL_PROMPT_MD: &str = include_str!("../../../docs/Start-Here/Install-Prompt.md");

/// The body of the first ```` ```text ```` fence in `md`, without the fence
/// lines and with a trailing newline. `None` when there is no such fence or it
/// never closes. Tolerates CRLF line endings (a Windows checkout).
#[must_use]
pub fn extract_prompt(md: &str) -> Option<String> {
    let mut lines = md.lines().map(|l| l.trim_end_matches('\r'));
    lines.by_ref().find(|l| l.trim() == "```text")?;
    let mut body = String::new();
    for line in lines {
        if line.trim() == "```" {
            return if body.trim().is_empty() { None } else { Some(body) };
        }
        body.push_str(line);
        body.push('\n');
    }
    None
}

/// The install prompt, as `th install-prompt` prints it.
pub fn prompt() -> Result<String> {
    extract_prompt(INSTALL_PROMPT_MD).ok_or_else(|| anyhow!("docs/Start-Here/Install-Prompt.md has no ```text block — this build is broken"))
}

/// `th install-prompt`: the prompt on stdout; with `copy`, also on the
/// clipboard (the note goes to stderr so redirection stays clean).
pub fn cmd_install_prompt(copy: bool) -> Result<()> {
    let text = prompt()?;
    print!("{text}");
    if copy {
        if crate::harness_clear::copy_to_clipboard(&text) {
            eprintln!("{} copied — paste it into Claude Code, Codex or OpenCode", "✓".green().bold());
        } else {
            eprintln!("{} couldn't reach a clipboard (pbcopy / wl-copy / xclip / clip.exe)", "!".yellow().bold());
            eprintln!("  copy the text above, or run `th install-prompt > prompt.txt`");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_page_yields_the_prompt() {
        let p = prompt().expect("the docs page has a text fence");
        assert!(p.starts_with("Install or update th"), "{p}");
        for needle in [
            "brew install smooai/tools/th",
            "th harness enable claude-code",
            "th harness enable opencode",
            "th harness enable codex",
            "th harness status",
            "auto-compact 300k",
            "autoCompactWindow",
            "model_auto_compact_token_limit = 300000",
            "th doctor --setup-statusline",
            "th model login",
            "/reload-plugins",
        ] {
            assert!(p.contains(needle), "prompt is missing `{needle}`");
        }
        assert!(!p.contains("```"), "fence lines must be stripped");
        assert!(p.ends_with('\n'));
    }

    /// The README carries a copy of the prompt so it reads on GitHub without a
    /// click; it must match the docs page word for word.
    #[test]
    fn readme_copy_matches_the_docs_page() {
        const README: &str = include_str!("../../../README.md");
        let readme = extract_prompt(README).expect("README has a ```text block");
        assert_eq!(
            readme,
            prompt().expect("docs prompt"),
            "README.md and docs/Start-Here/Install-Prompt.md disagree"
        );
    }

    #[test]
    fn extracts_only_the_first_text_fence() {
        let md = "# T\n\n```bash\nnot this\n```\n\n```text\nline one\n  indented\n```\n\n```text\nsecond\n```\n";
        assert_eq!(extract_prompt(md).as_deref(), Some("line one\n  indented\n"));
    }

    #[test]
    fn handles_crlf() {
        let md = "intro\r\n```text\r\nhello\r\nworld\r\n```\r\n";
        assert_eq!(extract_prompt(md).as_deref(), Some("hello\nworld\n"));
    }

    #[test]
    fn missing_or_unclosed_or_empty_fence_is_none() {
        assert_eq!(extract_prompt("no fence here"), None);
        assert_eq!(extract_prompt("```text\nnever closed\n"), None);
        assert_eq!(extract_prompt("```text\n\n   \n```\n"), None);
        assert_eq!(extract_prompt("```bash\nwrong language\n```\n"), None);
    }
}
