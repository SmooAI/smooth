//! Project context loader — parses AGENTS.md and resolves file references.
//!
//! AGENTS.md can contain file references in the `## File References` section:
//!
//! ```markdown
//! ## File References
//! - [CLAUDE.md](CLAUDE.md) — full file
//! - [Section name](CLAUDE.md#6-pearl-tracking) — specific section
//! ```
//!
//! This module reads those references, resolves them against the working
//! directory, and returns the combined context string for injection into
//! agent system prompts.

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

/// Load project context from AGENTS.md in the given directory.
///
/// Returns the AGENTS.md content with file references resolved inline.
/// If AGENTS.md doesn't exist, returns `None`.
pub fn load_project_context(working_dir: &Path) -> Option<String> {
    let agents_path = find_agents_md(working_dir)?;
    let raw = fs::read_to_string(&agents_path).ok()?;
    let base_dir = agents_path.parent()?;

    let refs = parse_file_references(&raw);
    if refs.is_empty() {
        return Some(raw);
    }

    let resolved = resolve_references(base_dir, &refs);
    let mut output = raw;

    // Append resolved file content at the end
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

/// Find AGENTS.md by walking up from `start_dir`.
fn find_agents_md(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join("AGENTS.md");
        if candidate.is_file() {
            return Some(candidate);
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
    fn load_returns_none_when_no_agents_md() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        assert!(load_project_context(tmp.path()).is_none());
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
