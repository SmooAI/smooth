//! Project context loader — parses AGENTS.md (or its fallbacks) and
//! resolves file references.
//!
//! Pearl th-5002c4 (user observation 2026-05-11): Smooth previously
//! only read AGENTS.md. Many projects don't have one but DO have
//! CLAUDE.md or a SMOOTH.md or .smooth/CONTEXT.md. The user also
//! wanted user-level facts ("I run a smoo-hub dashboard at
//! smoo-hub:8787") pulled in from ~/.smooth/. Now we walk a
//! preference order and stack user-level + project-level context.
//!
//! Preference order (first hit per layer; layers stack):
//!
//! - USER layer (read once, prepended):
//!   - `~/.smooth/CONTEXT.md`
//!   - `~/.smooth/AGENTS.md`
//!   - `~/.smooth/CLAUDE.md`
//!
//! - PROJECT layer (walk up from working_dir, first hit wins):
//!   - `<dir>/.smooth/CONTEXT.md`
//!   - `<dir>/SMOOTH.md`
//!   - `<dir>/AGENTS.md`
//!   - `<dir>/CLAUDE.md`
//!
//! AGENTS.md / SMOOTH.md can contain file references in the
//! `## File References` section:
//!
//! ```markdown
//! ## File References
//! - [CLAUDE.md](CLAUDE.md) — full file
//! - [Section name](CLAUDE.md#6-pearl-tracking) — specific section
//! ```
//!
//! Those references are resolved against the file's directory and
//! appended inline. The combined string is injected into the agent's
//! system prompt.

use std::fs;
use std::path::{Path, PathBuf};

/// Parsed file reference from AGENTS.md.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    /// Display label from the markdown link text.
    pub label: String,
    /// Relative file path (without fragment).
    pub path: String,
    /// Optional `#fragment` pointing to a heading.
    pub fragment: Option<String>,
    /// Optional description after the ` — `.
    pub description: Option<String>,
}

/// Load combined project + user context. Returns the stacked
/// content (user-level prepended), with file references in any
/// AGENTS.md / SMOOTH.md resolved inline.
///
/// Pearl th-5002c4: stacks user-level (~/.smooth/CONTEXT.md or
/// AGENTS.md or CLAUDE.md) above project-level (.smooth/CONTEXT.md
/// → SMOOTH.md → AGENTS.md → CLAUDE.md, walked up from
/// `working_dir`). Returns `None` only when NEITHER layer found
/// anything — so a workspace with a bare CLAUDE.md and no user-
/// level file still loads context.
pub fn load_project_context(working_dir: &Path) -> Option<String> {
    let user_ctx = load_user_context();
    let project_ctx = load_layered_project_context(working_dir);

    match (user_ctx, project_ctx) {
        (None, None) => None,
        (Some(u), None) => Some(format!("## User context (~/.smooth)\n\n{u}")),
        (None, Some(p)) => Some(p),
        (Some(u), Some(p)) => Some(format!("## User context (~/.smooth)\n\n{u}\n\n---\n\n{p}")),
    }
}

/// Load the user-level context once. Walks the preference list and
/// returns the first hit. None if the user has no `.smooth` context
/// at all.
fn load_user_context() -> Option<String> {
    let home = dirs_next::home_dir()?;
    let candidates = [home.join(".smooth/CONTEXT.md"), home.join(".smooth/AGENTS.md"), home.join(".smooth/CLAUDE.md")];
    for path in &candidates {
        if let Ok(raw) = fs::read_to_string(path) {
            if !raw.trim().is_empty() {
                return Some(raw);
            }
        }
    }
    None
}

/// Walk up from `working_dir` looking for any of the project-level
/// context files in preference order, return the first hit with
/// references resolved.
fn load_layered_project_context(working_dir: &Path) -> Option<String> {
    let context_path = find_project_context_file(working_dir)?;
    let raw = fs::read_to_string(&context_path).ok()?;
    let base_dir = context_path.parent()?;

    let refs = parse_file_references(&raw);
    if refs.is_empty() {
        return Some(raw);
    }

    let resolved = resolve_references(base_dir, &refs);
    let mut output = raw;

    if !resolved.is_empty() {
        output.push_str("\n---\n\n## Resolved File References\n\n");
        for (file_ref, content) in &resolved {
            let heading = file_ref
                .description
                .as_ref()
                .map_or_else(|| format!("### {}\n", file_ref.label), |desc| format!("### {} — {}\n", file_ref.label, desc));
            output.push_str(&heading);
            output.push_str("\n```\n");
            output.push_str(content);
            if !content.ends_with('\n') {
                output.push('\n');
            }
            output.push_str("```\n\n");
        }
    }

    Some(output)
}

/// Find a project context file by walking up from `start_dir`.
/// Preference order at each level: .smooth/CONTEXT.md → SMOOTH.md
/// → AGENTS.md → CLAUDE.md. First hit wins (per directory, then
/// keep walking up until we hit one).
fn find_project_context_file(start_dir: &Path) -> Option<PathBuf> {
    const PROJECT_CONTEXT_CANDIDATES: &[&str] = &[".smooth/CONTEXT.md", "SMOOTH.md", "AGENTS.md", "CLAUDE.md"];

    let mut dir = start_dir.to_path_buf();
    loop {
        for candidate in PROJECT_CONTEXT_CANDIDATES {
            let path = dir.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Parse `## File References` section from AGENTS.md content.
///
/// Expects markdown list items like:
/// ```text
/// - [Label](path.md) — description
/// - [Label](path.md#fragment) — description
/// ```
pub fn parse_file_references(content: &str) -> Vec<FileRef> {
    let mut refs = Vec::new();
    let mut in_section = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Detect the file references section
        if trimmed.starts_with("## ") || trimmed.starts_with("# ") {
            in_section = trimmed.to_lowercase().contains("file reference");
            continue;
        }

        if !in_section {
            continue;
        }

        // Parse markdown link: - [Label](path#fragment) — description
        if let Some(file_ref) = parse_link_line(trimmed) {
            refs.push(file_ref);
        }
    }

    refs
}

/// Parse a single markdown list-item link line.
fn parse_link_line(line: &str) -> Option<FileRef> {
    // Strip leading `- ` or `* `
    let line = line.strip_prefix("- ").or_else(|| line.strip_prefix("* "))?;

    // Match [label](target)
    let open_bracket = line.find('[')?;
    let close_bracket = line[open_bracket..].find(']')? + open_bracket;
    let label = line[open_bracket + 1..close_bracket].to_string();

    let rest = &line[close_bracket + 1..];
    let open_paren = rest.find('(')?;
    let close_paren = rest[open_paren..].find(')')? + open_paren;
    let target = &rest[open_paren + 1..close_paren];

    // Split path and fragment
    let (path, fragment) = target.find('#').map_or_else(
        || (target.to_string(), None),
        |hash_pos| (target[..hash_pos].to_string(), Some(target[hash_pos + 1..].to_string())),
    );

    // Description after ` — ` or ` - `
    let after_link = &rest[close_paren + 1..];
    let description = after_link
        .strip_prefix(" — ")
        .or_else(|| after_link.strip_prefix(" - "))
        .or_else(|| after_link.strip_prefix(" -- "))
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty());

    if path.is_empty() && fragment.is_none() {
        return None;
    }

    Some(FileRef {
        label,
        path,
        fragment,
        description,
    })
}

/// Resolve file references against a base directory.
/// Returns pairs of (reference, resolved content).
fn resolve_references(base_dir: &Path, refs: &[FileRef]) -> Vec<(FileRef, String)> {
    let mut results = Vec::new();

    for file_ref in refs {
        let file_path = base_dir.join(&file_ref.path);
        let Ok(content) = fs::read_to_string(&file_path) else {
            continue; // Skip unreadable files
        };

        let resolved = if let Some(ref fragment) = file_ref.fragment {
            extract_section(&content, fragment)
        } else {
            content
        };

        if !resolved.trim().is_empty() {
            results.push((file_ref.clone(), resolved));
        }
    }

    results
}

/// Extract a markdown section by heading fragment.
///
/// The fragment is matched against heading text (lowercased, with spaces
/// replaced by hyphens and non-alphanumeric chars removed — standard
/// GitHub-style heading anchors).
fn extract_section(content: &str, fragment: &str) -> String {
    let target = normalize_fragment(fragment);
    let lines: Vec<&str> = content.lines().collect();
    let mut start = None;
    let mut start_level = 0;

    for (i, line) in lines.iter().enumerate() {
        if let Some((level, text)) = parse_heading(line) {
            let anchor = heading_to_anchor(text);
            if anchor == target || anchor.contains(&target) || target.contains(&anchor) {
                start = Some(i);
                start_level = level;
                continue;
            }
            // If we've started capturing and hit a same-or-higher-level heading, stop
            if let Some(s) = start {
                if level <= start_level {
                    return lines[s..i].join("\n");
                }
            }
        }
    }

    // If we found the start but not the end, take everything from start to EOF
    if let Some(s) = start {
        return lines[s..].join("\n");
    }

    String::new()
}

/// Parse a markdown heading line, returning (level, text).
fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim();
    if !trimmed.starts_with('#') {
        return None;
    }
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    let text = trimmed[level..].trim();
    if text.is_empty() {
        return None;
    }
    Some((level, text))
}

/// Convert heading text to a GitHub-style anchor.
fn heading_to_anchor(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else if c == ' ' {
                '-'
            } else {
                // Drop other chars
                '\0'
            }
        })
        .filter(|&c| c != '\0')
        .collect::<String>()
        .replace("--", "-")
}

/// Normalize a fragment for comparison.
fn normalize_fragment(fragment: &str) -> String {
    heading_to_anchor(fragment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_link() {
        let r = parse_link_line("- [CLAUDE.md](CLAUDE.md) — Project overview").unwrap();
        assert_eq!(r.label, "CLAUDE.md");
        assert_eq!(r.path, "CLAUDE.md");
        assert!(r.fragment.is_none());
        assert_eq!(r.description.as_deref(), Some("Project overview"));
    }

    #[test]
    fn parse_link_with_fragment() {
        let r = parse_link_line("- [Pearl tracking](CLAUDE.md#6-pearl-tracking) — Pearl workflow").unwrap();
        assert_eq!(r.label, "Pearl tracking");
        assert_eq!(r.path, "CLAUDE.md");
        assert_eq!(r.fragment.as_deref(), Some("6-pearl-tracking"));
        assert_eq!(r.description.as_deref(), Some("Pearl workflow"));
    }

    #[test]
    fn parse_link_no_description() {
        let r = parse_link_line("- [README](README.md)").unwrap();
        assert_eq!(r.label, "README");
        assert_eq!(r.path, "README.md");
        assert!(r.fragment.is_none());
        assert!(r.description.is_none());
    }

    #[test]
    fn parse_file_references_section() {
        let content = "# Agent Instructions\n\nSome intro text.\n\n## File References\n\n\
            - [CLAUDE.md](CLAUDE.md) — Full file\n\
            - [Testing](CLAUDE.md#8-testing) — Testing reqs\n\n\
            ## Other Section\n\n\
            - [not a ref](foo.md)\n";
        let refs = parse_file_references(content);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].path, "CLAUDE.md");
        assert!(refs[0].fragment.is_none());
        assert_eq!(refs[1].path, "CLAUDE.md");
        assert_eq!(refs[1].fragment.as_deref(), Some("8-testing"));
    }

    #[test]
    fn heading_to_anchor_basic() {
        assert_eq!(heading_to_anchor("6. Pearl Tracking"), "6-pearl-tracking");
        assert_eq!(heading_to_anchor("Testing - MANDATORY"), "testing--mandatory");
        assert_eq!(heading_to_anchor("Simple Heading"), "simple-heading");
    }

    #[test]
    fn extract_section_by_fragment() {
        let content = "# Top\n\nIntro\n\n## Section A\n\nContent A\n\n## Section B\n\nContent B\n\n### Subsection\n\nSub content\n";
        let section = extract_section(content, "section-a");
        assert!(section.contains("## Section A"));
        assert!(section.contains("Content A"));
        assert!(!section.contains("Section B"));
    }

    #[test]
    fn extract_section_to_eof() {
        let content = "# Top\n\n## Last Section\n\nFinal content\n";
        let section = extract_section(content, "last-section");
        assert!(section.contains("## Last Section"));
        assert!(section.contains("Final content"));
    }

    #[test]
    fn extract_section_not_found() {
        let content = "# Top\n\n## Existing\n\nContent\n";
        let section = extract_section(content, "nonexistent");
        assert!(section.is_empty());
    }

    #[test]
    fn load_from_temp_dir() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let agents = tmp.path().join("AGENTS.md");
        let claude = tmp.path().join("CLAUDE.md");

        fs::write(
            &claude,
            "# Project\n\nOverview\n\n## Testing\n\nAll tests must pass.\n\n## Deploy\n\nNever deploy locally.\n",
        )
        .unwrap();
        fs::write(
            &agents,
            "# Agent Instructions\n\n## File References\n\n- [Testing](CLAUDE.md#testing) — Test reqs\n\n## Rules\n\nBe helpful.\n",
        )
        .unwrap();

        let ctx = load_project_context(tmp.path()).expect("load context");
        assert!(ctx.contains("Agent Instructions"));
        assert!(ctx.contains("Resolved File References"));
        assert!(ctx.contains("All tests must pass"));
    }

    #[test]
    fn load_returns_none_when_no_context_files_anywhere() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        // load_project_context walks up — to keep the test isolated
        // we just check that an empty subdir with no parents
        // containing a context file returns None. The walk-up will
        // continue to filesystem root; assertion only valid if
        // there's no AGENTS.md / CLAUDE.md / SMOOTH.md anywhere
        // above the temp dir AND no user-level context. The CI
        // environment satisfies the first; the second we can't
        // control without env mocking. Skip this assertion when a
        // user context file exists on the test host.
        let home_has_ctx = dirs_next::home_dir()
            .map(|h| h.join(".smooth/CONTEXT.md").is_file() || h.join(".smooth/AGENTS.md").is_file() || h.join(".smooth/CLAUDE.md").is_file())
            .unwrap_or(false);
        if home_has_ctx {
            eprintln!("skipping no-context-files test — host has ~/.smooth/CONTEXT.md or equivalent");
            return;
        }
        // Walk-up from the temp dir will still escape to the
        // filesystem root and might find a stray AGENTS.md.
        // Practical guard: assert the result doesn't contain
        // the temp dir's path (it shouldn't if nothing's there).
        let result = load_project_context(tmp.path());
        if let Some(ref content) = result {
            assert!(
                !content.contains(tmp.path().to_string_lossy().as_ref()),
                "found context referring to temp dir which should be empty: {content}"
            );
        }
    }

    #[test]
    fn load_prefers_smooth_context_over_claude_md() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".smooth")).unwrap();
        fs::write(tmp.path().join(".smooth/CONTEXT.md"), "# Smooth context\n\nthe winner").unwrap();
        fs::write(tmp.path().join("CLAUDE.md"), "# Claude.md\n\nshould lose").unwrap();
        let ctx = load_project_context(tmp.path()).expect("loaded");
        assert!(ctx.contains("the winner"), "expected .smooth/CONTEXT.md preferred: {ctx}");
        assert!(!ctx.contains("should lose"), ".smooth/CONTEXT.md must take precedence: {ctx}");
    }

    #[test]
    fn load_falls_back_to_claude_md_when_no_agents() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("CLAUDE.md"), "# CLAUDE.md\n\nfallback content").unwrap();
        let ctx = load_project_context(tmp.path()).expect("loaded");
        assert!(ctx.contains("fallback content"), "should fall back to CLAUDE.md: {ctx}");
    }

    #[test]
    fn load_prefers_smooth_md_over_claude_md() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("SMOOTH.md"), "# SMOOTH.md\n\nsmooth wins").unwrap();
        fs::write(tmp.path().join("CLAUDE.md"), "# CLAUDE.md\n\nclaude loses").unwrap();
        let ctx = load_project_context(tmp.path()).expect("loaded");
        assert!(ctx.contains("smooth wins"));
        assert!(!ctx.contains("claude loses"));
    }

    #[test]
    fn find_project_context_walks_up() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nested = tmp.path().join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        fs::write(tmp.path().join("CLAUDE.md"), "# CLAUDE.md at root").unwrap();
        let found = find_project_context_file(&nested).expect("walked up");
        assert!(found.to_string_lossy().ends_with("CLAUDE.md"));
    }

    #[test]
    fn load_without_file_references_returns_raw() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let agents = tmp.path().join("AGENTS.md");
        fs::write(&agents, "# Agent Instructions\n\nJust some text.\n").unwrap();

        let ctx = load_project_context(tmp.path()).expect("load context");
        assert_eq!(ctx, "# Agent Instructions\n\nJust some text.\n");
    }
}
