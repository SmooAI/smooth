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
    /// A trusted SEP extension's `[resources] skills` directory. Extensions are
    /// user-installed, so they sit above the imported Claude/opencode ecosystems
    /// but below the user's own project / `~/.smooth` skills.
    Extension,
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
            Self::Extension => 2,
            Self::ClaudeCode => 3,
            Self::OpenCode => 4,
            Self::Builtin => 5,
        }
    }

    /// Short display label for the source (`project`, `user-smooth`,
    /// `claude-code`, `opencode`, `extension`, `builtin`).
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::UserSmooth => "user-smooth",
            Self::ClaudeCode => "claude-code",
            Self::OpenCode => "opencode",
            Self::Extension => "extension",
            Self::Builtin => "builtin",
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

    // One component per `join`, deliberately. A single `join(".smooth/skills")`
    // opens fine on Windows but produces a MIXED-separator root
    // (`C:\ws\.smooth/skills`), and every discovered skill path inherits it —
    // `C:\ws\.smooth/skills\add-show\SKILL.md`. Those paths go into the
    // persona for the model to read back, and any code comparing a discovered
    // path against one built normally never matches. th-a59af5.
    let project_dir = workspace_root.join(".smooth").join("skills");
    collect_from(&project_dir, SkillSource::Project, &mut skills);

    if let Some(home) = dirs_next::home_dir() {
        collect_from(&home.join(".smooth").join("skills"), SkillSource::UserSmooth, &mut skills);
        collect_from(&home.join(".claude").join("skills"), SkillSource::ClaudeCode, &mut skills);
        // opencode uses `~/.opencode/agents/<name>/...` in some
        // versions and `~/.opencode/skills/<name>/...` in others;
        // scan both.
        collect_from(&home.join(".opencode").join("skills"), SkillSource::OpenCode, &mut skills);
        collect_from(&home.join(".opencode").join("agents"), SkillSource::OpenCode, &mut skills);
    }

    // SEP extensions contribute their `[resources] skills` dirs (trusted only).
    // This is the unification seam: smooth-cast stays the one canonical skill
    // catalog; extensions feed it rather than parsing skills themselves.
    skills.extend(resources_discover(workspace_root));

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

/// Discover skills contributed by trusted SEP extensions.
///
/// Each installed extension may declare `[resources] skills = "<dir>"` in its
/// `extension.toml`; every SKILL under that dir (resolved against the extension
/// root) becomes a [`SkillSource::Extension`] skill. Only **trusted** extensions
/// contribute — a skill body is prepended to the agent's prompt, so loading an
/// untrusted extension's skills is the same prompt-injection surface as loading
/// its code, and gates on the same content-hashed trust store the host uses.
///
/// This is the unification seam (SEP Phase 5): the engine's extension discovery
/// finds the dirs, smooth-cast's parser turns them into the one canonical
/// `Skill` type. Extensions never parse skills themselves.
#[must_use]
pub fn resources_discover(workspace_root: &Path) -> Vec<Skill> {
    use smooth_operator::extension::manifest::{default_global_dir, discover as discover_extensions, project_dir};
    use smooth_policy::ext_trust::{hash_extension, TrustStore};

    let global = default_global_dir();
    let project = project_dir(workspace_root);
    let (extensions, _failures) = discover_extensions(global.as_deref(), Some(project.as_path()));
    let trust = TrustStore::load();

    let mut skills = Vec::new();
    for ext in extensions {
        let Some(rel) = ext.manifest.resources.skills.as_deref() else { continue };
        let hash = hash_extension(&ext.root).unwrap_or_default();
        if !trust.is_trusted(&ext.manifest.name, &hash) {
            tracing::debug!(name = %ext.manifest.name, "skipping untrusted extension's skills");
            continue;
        }
        collect_from(&ext.root.join(rel), SkillSource::Extension, &mut skills);
    }
    skills
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

// ---------------------------------------------------------------------------
// Invocation rendering (pearl th-e0f812)
// ---------------------------------------------------------------------------

/// Default char budget for the skill catalog injected into a system
/// prompt. Names + descriptions + triggers only — bodies are loaded on
/// demand via `skill_use`, so this stays small. ~4k chars ≈ 1k tokens.
pub const DEFAULT_CATALOG_BUDGET: usize = 4000;

/// Render the skill catalog (names + one-line descriptions + triggers)
/// for a system-prompt section. Bodies are NOT included — the agent
/// calls `skill_use(name)` to load a body on demand. Output is capped
/// at `budget` chars; skills that don't fit are dropped and a trailing
/// note reports how many were omitted. Returns `None` when there are no
/// skills to show.
#[must_use]
pub fn render_catalog(skills: &[Skill], budget: usize) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut out = String::from(
        "You have SKILLS available — reusable recipes that encode the right way to do a task. \
Before improvising a multi-step workflow, check this list. If one matches the user's intent, \
call `skill_use(\"<name>\")` to load its full instructions, then follow them.\n\n",
    );
    let mut shown = 0usize;
    for skill in skills {
        let line = if skill.triggers.is_empty() {
            format!("- {}: {}\n", skill.name, skill.description)
        } else {
            format!("- {}: {} (triggers: {})\n", skill.name, skill.description, skill.triggers.join(", "))
        };
        // Always show at least one skill even if it alone exceeds the
        // budget — a truncated catalog is more useful than none.
        if shown > 0 && out.len() + line.len() > budget {
            break;
        }
        out.push_str(&line);
        shown += 1;
    }
    let omitted = skills.len() - shown;
    if omitted > 0 {
        use std::fmt::Write as _;
        let _ = writeln!(out, "- …and {omitted} more (run `th skills list` to see all)");
    }
    Some(out)
}

/// Render a skill's body for injection into the conversation when the
/// agent calls `skill_use`. Prepends a header with the description and,
/// when the skill declares them, its scope / allowed_tools / allowed_hosts
/// so the model knows the constraints it's meant to work within.
///
/// Enforcement of `allowed_tools` / `allowed_hosts` is NOT done here —
/// this only surfaces the declaration to the model. Hard enforcement
/// lands with the auto-mode permission model (pearl th-515a13).
#[must_use]
pub fn render_invocation(skill: &Skill) -> String {
    let mut out = format!("# Skill: {}\n\n{}\n\n", skill.name, skill.description);

    let mut constraints: Vec<String> = Vec::new();
    if skill.scope == SkillScope::Host {
        constraints.push("runs on the host (outside the sandbox)".to_string());
    }
    if !skill.allowed_tools.is_empty() {
        constraints.push(format!("prefer these tools: {}", skill.allowed_tools.join(", ")));
    }
    if !skill.allowed_hosts.is_empty() {
        constraints.push(format!("may reach these hosts: {}", skill.allowed_hosts.join(", ")));
    }
    if !constraints.is_empty() {
        // ponytail: advisory only — real allowed_tools/allowed_hosts
        // enforcement arrives with auto-mode (pearl th-515a13). Until
        // then the header just tells the model the intended envelope.
        out.push_str("> Constraints: ");
        out.push_str(&constraints.join("; "));
        out.push_str(".\n\n");
    }

    out.push_str("Follow these instructions:\n\n");
    out.push_str(&skill.body);
    out
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
        let skill_dir = tmp.path().join(".smooth").join("skills").join("add-show");
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
        let project_dir = tmp.path().join(".smooth").join("skills").join("dupe");
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

    fn skill(name: &str, desc: &str, triggers: &[&str]) -> Skill {
        Skill {
            name: name.to_string(),
            description: desc.to_string(),
            triggers: triggers.iter().map(|s| s.to_string()).collect(),
            scope: SkillScope::Sandbox,
            allowed_hosts: Vec::new(),
            allowed_tools: Vec::new(),
            body: "do the thing".to_string(),
            source: SkillSource::UserSmooth,
            path: PathBuf::from("/tmp/x/SKILL.md"),
        }
    }

    #[test]
    fn render_catalog_empty_is_none() {
        assert!(render_catalog(&[], DEFAULT_CATALOG_BUDGET).is_none());
    }

    #[test]
    fn render_catalog_lists_names_descriptions_triggers() {
        let skills = vec![skill("add-show", "Add a show to the watchlist", &["add show", "add movie"])];
        let out = render_catalog(&skills, DEFAULT_CATALOG_BUDGET).expect("some");
        assert!(out.contains("add-show"));
        assert!(out.contains("Add a show to the watchlist"));
        assert!(out.contains("triggers: add show, add movie"));
        assert!(out.contains("skill_use"), "must tell the model how to invoke");
        // Body text must NOT leak into the catalog — that's the whole
        // point of the on-demand load.
        assert!(!out.contains("do the thing"));
    }

    #[test]
    fn render_catalog_respects_budget() {
        // 50 skills with long descriptions, tiny budget → only a few
        // shown, rest reported as omitted.
        let skills: Vec<Skill> = (0..50)
            .map(|i| skill(&format!("skill-{i}"), "a fairly wordy description that eats budget quickly", &[]))
            .collect();
        let out = render_catalog(&skills, 400).expect("some");
        assert!(out.contains("more (run `th skills list`"), "expected omission note, got: {out}");
        // At least the intro + one skill, but nowhere near all 50.
        assert!(out.matches("- skill-").count() < 50);
    }

    #[test]
    fn render_catalog_shows_at_least_one_even_over_budget() {
        let skills = vec![skill("big", "x".repeat(1000).as_str(), &[])];
        let out = render_catalog(&skills, 10).expect("some");
        assert!(out.contains("big"), "must show at least one skill even past budget");
    }

    #[test]
    fn render_invocation_includes_body_and_header() {
        let s = skill("add-show", "Add a show", &[]);
        let out = render_invocation(&s);
        assert!(out.contains("# Skill: add-show"));
        assert!(out.contains("Add a show"));
        assert!(out.contains("do the thing"), "body must be present");
    }

    #[test]
    fn render_invocation_surfaces_constraints() {
        let mut s = skill("scp-thing", "copies files", &[]);
        s.scope = SkillScope::Host;
        s.allowed_tools = vec!["bash".to_string()];
        s.allowed_hosts = vec!["smoo-hub".to_string()];
        let out = render_invocation(&s);
        assert!(out.contains("host"), "host scope should be surfaced");
        assert!(out.contains("bash"), "allowed_tools should be surfaced");
        assert!(out.contains("smoo-hub"), "allowed_hosts should be surfaced");
    }

    #[test]
    fn render_invocation_omits_constraints_line_when_none() {
        let s = skill("plain", "no constraints", &[]);
        let out = render_invocation(&s);
        assert!(!out.contains("> Constraints"), "no constraint line when nothing declared");
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

    #[test]
    fn resources_discover_gates_extension_skills_on_trust() {
        use smooth_policy::ext_trust::{hash_extension, TrustStore};

        // Isolate BOTH the extension store and the trust store under one
        // SMOOTH_HOME. `default_global_dir` (engine) and the trust store both
        // resolve through it, so the extension lands in the discovered global
        // scope and the trust file sits alongside it. Single test → no env race.
        let home = tempfile::tempdir().expect("tempdir");
        std::env::set_var("SMOOTH_HOME", home.path());

        let ext_root = home.path().join("extensions").join("demo");
        std::fs::create_dir_all(ext_root.join("skills").join("hi")).unwrap();
        std::fs::write(
            ext_root.join("extension.toml"),
            "name = \"demo\"\nversion = \"0.1.0\"\n[run]\ncommand = \"node\"\n[resources]\nskills = \"skills\"\n",
        )
        .unwrap();
        std::fs::write(
            ext_root.join("skills").join("hi").join("SKILL.md"),
            "---\nname: ext-hi\ndescription: contributed by an extension\n---\nBody.\n",
        )
        .unwrap();

        // Untrusted → contributes nothing (prompt-injection surface stays closed).
        let workspace = tempfile::tempdir().expect("ws");
        assert!(
            resources_discover(workspace.path()).is_empty(),
            "untrusted extension must not contribute skills"
        );

        // Trust it against its current content hash → the skill appears.
        let hash = hash_extension(&ext_root).unwrap();
        let mut trust = TrustStore::load();
        trust.set("demo", &ext_root.to_string_lossy(), &hash, true);
        trust.save().unwrap();

        let found = resources_discover(workspace.path());
        assert_eq!(found.len(), 1, "trusted extension contributes its skill");
        assert_eq!(found[0].name, "ext-hi");
        assert_eq!(found[0].source, SkillSource::Extension);

        std::env::remove_var("SMOOTH_HOME");
    }
}
