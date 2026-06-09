//! Skills — reusable recipes the agent can invoke (pearl th-e0f812).
//!
//! A SKILL is a markdown file with YAML frontmatter describing
//! WHEN to use it (triggers, description) and WHAT it requires
//! (allowed hosts, allowed tools, scope). The body is markdown
//! and ends up prepended to the agent's turn-instructions when
//! the skill is invoked.
//!
//! Smooth reads skills from multiple sources, normalizing YAML
//! dialect differences so a Claude Code skill or opencode skill
//! works as-is:
//!
//! Discovery order (first-match wins on name collision):
//!   1. `<workspace>/.smooth/skills/<name>/SKILL.md`  — project, highest precedence
//!   2. `~/.smooth/skills/<name>/SKILL.md`            — user-level Smooth
//!   3. `~/.claude/skills/<name>/SKILL.md`            — Claude Code (reused as-is)
//!   4. `~/.opencode/skills/<name>/<file>.md`         — opencode
//!
//! This module:
//!   - Defines the normalized `Skill` struct
//!   - Parses YAML frontmatter from each dialect
//!   - Walks the discovery sources and returns the set of
//!     available skills
//!   - DOES NOT handle invocation, runtime integration, or
//!     security policy mapping — those land separately as
//!     the `skill_use` tool and host policy enforcement
//!     pre-grants.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A skill's effective scope. `Sandbox` (default) means the skill
/// runs inside the sandbox; `Host` means it bypasses the sandbox
/// and runs in the supervisor's process directly (for scp, Photos.app,
/// AWS SSO interactive flows, etc.). Network alone is NEVER a
/// reason for `Host` — host policy enforcement proxies network
/// through the host instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillScope {
    /// Runs inside the sandbox with host policy enforcement.
    #[default]
    Sandbox,
    /// Runs in the supervisor's process on the host. Same security
    /// envelope as the supervisor itself.
    Host,
}

/// Where a skill was loaded from. Useful for the user when there
/// are multiple skills with the same name (precedence) or when the
/// user wants to know "where did this come from".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillSource {
    /// `.smooth/skills/<name>/SKILL.md` inside the project tree.
    Project,
    /// `~/.smooth/skills/<name>/SKILL.md` — user-level Smooth.
    UserSmooth,
    /// `~/.claude/skills/<name>/SKILL.md` — Claude Code.
    ClaudeCode,
    /// `~/.opencode/skills/<name>/...` — opencode.
    OpenCode,
    /// Embedded in the smooth binary. Shipped with every install
    /// (currently: `create-skill`). User-authored skills with the
    /// same name OVERRIDE the built-in (the built-in is the lowest
    /// precedence).
    Builtin,
}

impl SkillSource {
    /// Precedence order — lower number wins on name collision.
    #[must_use]
    pub fn precedence(&self) -> u8 {
        match self {
            Self::Project => 0,
            Self::UserSmooth => 1,
            Self::ClaudeCode => 2,
            Self::OpenCode => 3,
            Self::Builtin => 4,
        }
    }
}

/// Normalized skill record. Built from whatever YAML dialect the
/// source ecosystem uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Skill name — used by `skill_use(name)` and shown in the
    /// chief role's system prompt. Required.
    pub name: String,
    /// One-line description used by chief / TUI to pick. Required.
    pub description: String,
    /// Trigger phrases. Chief uses these as LLM-side hints rather
    /// than hard pattern matches; empty list is fine.
    #[serde(default)]
    pub triggers: Vec<String>,
    /// Effective scope (sandbox / host).
    #[serde(default)]
    pub scope: SkillScope,
    /// Hostnames the skill needs host policy enforcement to allow. Becomes a pre-grant
    /// at dispatch time (no user prompt) — declaring a host here is
    /// an explicit declaration of intent.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Tools the skill restricts to. Empty means inherit the
    /// caller's full toolset. Pearl th-cfa1fb's lazy-tool system
    /// integrates with this.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Markdown body — the actual recipe text.
    pub body: String,
    /// Where this skill was loaded from. Set by the discovery
    /// walker; not part of the YAML frontmatter.
    #[serde(default = "default_source")]
    pub source: SkillSource,
    /// Absolute path to the SKILL file. For debugging + the
    /// hypothetical `th skills show` command.
    pub path: PathBuf,
}

fn default_source() -> SkillSource {
    SkillSource::UserSmooth
}

/// Parse a SKILL.md (or SKILL.markdown) file: YAML frontmatter
/// delimited by `---` lines at the top, then markdown body.
///
/// Returns `Ok(None)` when the file is missing frontmatter
/// entirely (the file might be a stub or notes, not a skill).
/// Returns `Err` only on real I/O or parse errors.
pub fn parse_skill_file(path: &Path, source: SkillSource) -> anyhow::Result<Option<Skill>> {
    let raw = fs::read_to_string(path).with_context_path(path)?;
    parse_skill_string(&raw, path, source)
}

/// Parse a skill from an in-memory string. Public for tests.
pub fn parse_skill_string(raw: &str, path: &Path, source: SkillSource) -> anyhow::Result<Option<Skill>> {
    // Frontmatter must start at byte 0 with `---\n` (or `---\r\n`).
    // Anything else means no frontmatter — return None.
    let Some(stripped) = raw.strip_prefix("---\n").or_else(|| raw.strip_prefix("---\r\n")) else {
        return Ok(None);
    };
    // Find the closing `---` on its own line.
    let close =
        find_frontmatter_close(stripped).ok_or_else(|| anyhow::anyhow!("SKILL file at {} opened YAML frontmatter but never closed it", path.display()))?;
    let yaml = &stripped[..close];
    let body = stripped[close..]
        .split_once('\n')
        .map(|(_, rest)| rest.trim_start_matches('\n').to_string())
        .unwrap_or_default();

    // Normalize across dialects.
    let parsed: NormalizedFrontmatter =
        serde_yml::from_str(yaml).map_err(|e| anyhow::anyhow!("SKILL file at {}: YAML frontmatter parse error: {e}", path.display()))?;

    // Required: name + description. Skip silently if either is
    // missing — some markdown files in ~/.claude/ may have YAML
    // frontmatter for other purposes (article metadata, etc.).
    let Some(name) = parsed.name.or_else(|| skill_name_from_path(path)) else {
        return Ok(None);
    };
    let Some(description) = parsed.description else { return Ok(None) };

    Ok(Some(Skill {
        name,
        description,
        triggers: parsed.triggers.unwrap_or_default(),
        scope: parsed.scope.unwrap_or_default(),
        allowed_hosts: parsed.allowed_hosts.unwrap_or_default(),
        allowed_tools: parsed.allowed_tools.unwrap_or_default(),
        body,
        source,
        path: path.to_path_buf(),
    }))
}

/// Inferred name from the parent directory — Claude Code's
/// convention is `~/.claude/skills/<name>/SKILL.md` so when the
/// frontmatter omits `name`, the parent dir name IS the name.
fn skill_name_from_path(path: &Path) -> Option<String> {
    path.parent()?.file_name()?.to_str().map(|s| s.to_string())
}

/// Locate the closing `---` line in a frontmatter block (the input
/// is the bytes AFTER the opening `---\n`). Returns the byte offset
/// of the closing `---` line's start.
fn find_frontmatter_close(s: &str) -> Option<usize> {
    let mut offset = 0usize;
    for line in s.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" || trimmed == "..." {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

/// Raw frontmatter shape that handles every dialect we've seen.
/// Most fields are `Option<…>` so missing keys parse cleanly.
#[derive(Debug, Deserialize)]
struct NormalizedFrontmatter {
    name: Option<String>,
    description: Option<String>,
    triggers: Option<Vec<String>>,
    scope: Option<SkillScope>,
    #[serde(default, rename = "allowed-hosts", alias = "allowed_hosts")]
    allowed_hosts: Option<Vec<String>>,
    #[serde(default, rename = "allowed-tools", alias = "allowed_tools")]
    allowed_tools: Option<Vec<String>>,
}

/// Walk the discovery sources and return every skill found.
///
/// Name-collision resolution: skills are scanned in precedence
/// order (project → user-smooth → claude → opencode). The FIRST
/// skill seen for a given name wins; subsequent skills with the
/// same name are dropped silently. Use `discover_with_overrides`
/// if you want to see the full multi-source list.
pub fn discover(workspace_root: &Path) -> Vec<Skill> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut skills: Vec<Skill> = Vec::new();

    for skill in discover_with_overrides(workspace_root) {
        if seen.insert(skill.name.clone()) {
            skills.push(skill);
        }
    }
    skills
}

/// Like [`discover`] but returns ALL skills from all sources,
/// even when names collide. Sorted in precedence order so the
/// first occurrence per name is the winner.
pub fn discover_with_overrides(workspace_root: &Path) -> Vec<Skill> {
    let mut skills: Vec<Skill> = Vec::new();

    let project_dir = workspace_root.join(".smooth/skills");
    collect_from(&project_dir, SkillSource::Project, &mut skills);

    if let Some(home) = dirs_next::home_dir() {
        collect_from(&home.join(".smooth/skills"), SkillSource::UserSmooth, &mut skills);
        collect_from(&home.join(".claude/skills"), SkillSource::ClaudeCode, &mut skills);
        // opencode uses `~/.opencode/agents/<name>/...` in some
        // versions and `~/.opencode/skills/<name>/...` in others;
        // scan both.
        collect_from(&home.join(".opencode/skills"), SkillSource::OpenCode, &mut skills);
        collect_from(&home.join(".opencode/agents"), SkillSource::OpenCode, &mut skills);
    }

    // Builtin skills ship with the binary. They land last so any
    // user-authored skill at the same name overrides them.
    skills.extend(builtin_skills());

    skills.sort_by_key(|s| s.source.precedence());
    skills
}

/// Skills shipped embedded in the smooth binary. Currently just
/// `create-skill` — the meta-skill that helps the user author new
/// skills. Pearl th-e0f812.
fn builtin_skills() -> Vec<Skill> {
    const CREATE_SKILL_BODY: &str = include_str!("../builtin-skills/create-skill/SKILL.md");
    let mut out = Vec::new();
    let virtual_path = PathBuf::from("<builtin>/create-skill/SKILL.md");
    if let Ok(Some(skill)) = parse_skill_string(CREATE_SKILL_BODY, &virtual_path, SkillSource::Builtin) {
        out.push(skill);
    }
    out
}

/// Scan a single skills root directory and append every valid
/// skill found. Silently skips malformed files (logs the error
/// via `tracing`) so one broken file doesn't poison the rest.
fn collect_from(root: &Path, source: SkillSource, out: &mut Vec<Skill>) {
    if !root.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Look for SKILL.md or SKILL.markdown inside the skill dir,
        // then fall back to any single .md file (opencode some
        // skills are flat).
        let candidates = ["SKILL.md", "SKILL.markdown", "skill.md", "skill.markdown"];
        let mut skill_file: Option<PathBuf> = None;
        for name in candidates {
            let p = path.join(name);
            if p.is_file() {
                skill_file = Some(p);
                break;
            }
        }
        if skill_file.is_none() {
            // Fall back: a single .md file in the dir is the skill.
            if let Ok(mds) = fs::read_dir(&path) {
                let md_files: Vec<PathBuf> = mds
                    .flatten()
                    .filter_map(|e| {
                        let p = e.path();
                        if p.extension().and_then(|s| s.to_str()) == Some("md") {
                            Some(p)
                        } else {
                            None
                        }
                    })
                    .collect();
                if md_files.len() == 1 {
                    skill_file = md_files.into_iter().next();
                }
            }
        }
        let Some(skill_path) = skill_file else { continue };
        match parse_skill_file(&skill_path, source.clone()) {
            Ok(Some(skill)) => out.push(skill),
            Ok(None) => {
                tracing::debug!(path = %skill_path.display(), "skipped — no frontmatter or missing name/description");
            }
            Err(e) => {
                tracing::warn!(path = %skill_path.display(), error = %e, "skill parse error — skipping");
            }
        }
    }
}

trait WithContextPath {
    fn with_context_path(self, path: &Path) -> anyhow::Result<String>;
}

impl WithContextPath for std::io::Result<String> {
    fn with_context_path(self, path: &Path) -> anyhow::Result<String> {
        self.map_err(|e| anyhow::anyhow!("reading SKILL file {}: {e}", path.display()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const ADD_SHOW_SKILL: &str = r#"---
name: add-show
description: Add a TV show or movie to the smoo-hub dashboard watchlist
triggers:
  - add show
  - add movie
  - watchlist
scope: host
allowed_hosts:
  - smoo-hub
  - api.tvmaze.com
---

# add-show

When the user asks to add a show:

1. Look up the poster from TVMaze
2. Resize with sips
3. scp to smoo-hub
4. POST to /api/shows
"#;

    #[test]
    fn parse_canonical_skill() {
        let path = PathBuf::from("/tmp/skills/add-show/SKILL.md");
        let skill = parse_skill_string(ADD_SHOW_SKILL, &path, SkillSource::UserSmooth)
            .expect("parse")
            .expect("some");
        assert_eq!(skill.name, "add-show");
        assert!(skill.description.contains("watchlist"));
        assert_eq!(skill.triggers.len(), 3);
        assert_eq!(skill.scope, SkillScope::Host);
        assert!(skill.allowed_hosts.contains(&"smoo-hub".to_string()));
        assert!(skill.body.contains("Look up the poster from TVMaze"));
    }

    #[test]
    fn missing_frontmatter_returns_none() {
        let raw = "# Just a markdown file\n\nNo frontmatter, not a skill.";
        let path = PathBuf::from("/tmp/notes.md");
        let skill = parse_skill_string(raw, &path, SkillSource::UserSmooth).expect("parse");
        assert!(skill.is_none(), "non-skill markdown should return None: {skill:?}");
    }

    #[test]
    fn missing_description_returns_none() {
        // No description = silently skip. Catches generic article
        // YAML frontmatter (e.g. some opencode files have just
        // `title:`) without erroring.
        let raw = "---\nname: thing\ntitle: not a skill\n---\n\nbody";
        let path = PathBuf::from("/tmp/skills/thing/SKILL.md");
        let skill = parse_skill_string(raw, &path, SkillSource::UserSmooth).expect("parse");
        assert!(skill.is_none());
    }

    #[test]
    fn name_inferred_from_parent_dir() {
        // Some skills omit `name` and rely on the directory name —
        // Claude Code's docs encourage this so authors don't repeat
        // themselves.
        let raw = "---\ndescription: inferred name\n---\n\nbody";
        let path = PathBuf::from("/tmp/skills/my-skill/SKILL.md");
        let skill = parse_skill_string(raw, &path, SkillSource::ClaudeCode).expect("parse").expect("some");
        assert_eq!(skill.name, "my-skill");
    }

    #[test]
    fn supports_hyphenated_alias_for_allowed_hosts() {
        // Claude Code uses `allowed-tools:` (hyphen); Smooth uses
        // `allowed_tools:`. Same for hosts. Both parse.
        let raw = r#"---
name: x
description: y
allowed-hosts:
  - example.com
allowed-tools:
  - bash
---

body"#;
        let path = PathBuf::from("/tmp/skills/x/SKILL.md");
        let skill = parse_skill_string(raw, &path, SkillSource::ClaudeCode).expect("parse").expect("some");
        assert_eq!(skill.allowed_hosts, vec!["example.com"]);
        assert_eq!(skill.allowed_tools, vec!["bash"]);
    }

    #[test]
    fn unclosed_frontmatter_is_error() {
        let raw = "---\nname: x\ndescription: y\n\nno close marker, just body";
        let path = PathBuf::from("/tmp/skills/x/SKILL.md");
        let err = parse_skill_string(raw, &path, SkillSource::UserSmooth).unwrap_err();
        assert!(err.to_string().contains("never closed"));
    }

    #[test]
    fn discover_from_temp_project_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let skill_dir = tmp.path().join(".smooth/skills/add-show");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), ADD_SHOW_SKILL).unwrap();
        let skills = discover(tmp.path());
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"add-show"), "expected add-show in {names:?}");
    }

    #[test]
    fn project_skill_wins_over_user_smooth_skill() {
        // discover() should pick the project version when both
        // exist with the same name.
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_dir = tmp.path().join(".smooth/skills/dupe");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(project_dir.join("SKILL.md"), "---\nname: dupe\ndescription: PROJECT VERSION\n---\n\nbody").unwrap();
        // We can't easily mock ~/.smooth/, so the precedence test
        // here just checks that the discovered project skill has
        // the project source + body.
        let skills = discover(tmp.path());
        let dupe = skills.iter().find(|s| s.name == "dupe").expect("found");
        assert_eq!(dupe.source, SkillSource::Project);
        assert!(dupe.description.contains("PROJECT VERSION"));
    }

    #[test]
    fn precedence_ordering_is_stable() {
        assert!(SkillSource::Project.precedence() < SkillSource::UserSmooth.precedence());
        assert!(SkillSource::UserSmooth.precedence() < SkillSource::ClaudeCode.precedence());
        assert!(SkillSource::ClaudeCode.precedence() < SkillSource::OpenCode.precedence());
        assert!(SkillSource::OpenCode.precedence() < SkillSource::Builtin.precedence());
    }

    #[test]
    fn builtin_create_skill_loads() {
        // Smooth ships with `create-skill` embedded — every install
        // gets the meta-skill that bootstraps a user's skill library.
        let built = builtin_skills();
        assert!(!built.is_empty(), "must ship at least one built-in skill");
        let create_skill = built.iter().find(|s| s.name == "create-skill").expect("create-skill must be built-in");
        assert!(create_skill.description.to_lowercase().contains("skill"));
        assert!(!create_skill.triggers.is_empty(), "create-skill needs triggers");
        assert_eq!(create_skill.source, SkillSource::Builtin);
        assert!(create_skill.body.contains("Process"), "body should be the markdown recipe");
    }
}
