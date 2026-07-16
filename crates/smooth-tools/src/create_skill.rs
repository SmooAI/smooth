//! `create_skill` — let the agent author its own reusable skills.
//!
//! A skill is a `SKILL.md` (YAML frontmatter — `name` / `description` /
//! `triggers` — plus a markdown body of instructions). The daemon discovers
//! skills at agent-build time and folds their index into the persona; the agent
//! then `read_file`s a body on demand. This tool is the WRITE half: when the
//! user teaches Big Smooth a repeatable recipe ("remember how to do X", "make a
//! skill for Y"), it persists one to `~/.smooth/skills/<name>/SKILL.md`.
//!
//! The written frontmatter is serialized with the SAME `serde_yml` the skills
//! catalog parses back, so a create → discover round-trip is lossless. No shell;
//! the write is atomic (temp file + rename).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

use crate::util::req_str;

/// `create_skill` — author a new reusable skill for the agent itself.
pub struct CreateSkillTool;

/// The YAML frontmatter shape written to disk. Matches the fields the skills
/// parser reads (`name` / `description` / `triggers`); `triggers` is omitted
/// entirely when empty so the file stays clean.
#[derive(Serialize)]
struct Frontmatter {
    name: String,
    description: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    triggers: Vec<String>,
}

#[async_trait]
impl Tool for CreateSkillTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "create_skill".into(),
            description: "Author a new reusable skill for yourself. A skill is a SKILL.md (name + description + triggers + instructions) that you'll automatically discover and can follow on future turns. Use this when the user teaches you a repeatable recipe ('remember how to do X', 'make a skill for Y') or when you figure out a multi-step procedure worth saving."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Skill name in kebab-case (lowercase letters, digits, single hyphens), e.g. \"deploy-web\". Becomes the directory name; no slashes or dots."
                    },
                    "description": {
                        "type": "string",
                        "description": "One line: what the skill does AND when to use it. This is what you'll see in your skills index to decide whether to load it."
                    },
                    "body": {
                        "type": "string",
                        "description": "The markdown instructions — the actual recipe/steps to follow."
                    },
                    "triggers": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional phrases that hint when this skill applies, e.g. [\"deploy the site\", \"ship web\"]."
                    },
                    "overwrite": {
                        "type": "boolean",
                        "description": "Replace an existing skill of the same name (default: false — refuses to overwrite)."
                    }
                },
                "required": ["name", "description", "body"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        // Writes to disk — don't run alongside other tools.
        false
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let root = skills_root()?;
        write_skill_to(&root, &arguments)
    }
}

/// The testable core: author a skill under `root` (the skills directory).
/// [`CreateSkillTool::execute`] supplies `~/.smooth/skills`; tests supply a
/// tempdir, so no process-global state (HOME/env) is touched. Pure sync I/O.
fn write_skill_to(root: &Path, arguments: &Value) -> anyhow::Result<String> {
    let name = req_str(arguments, "name")?;
    validate_name(&name)?;
    let description = req_str(arguments, "description")?;
    if description.trim().is_empty() {
        anyhow::bail!("`description` must not be empty");
    }
    let body = req_str(arguments, "body")?;
    if body.trim().is_empty() {
        anyhow::bail!("`body` must not be empty");
    }
    let triggers = parse_triggers(arguments)?;
    let overwrite = arguments.get("overwrite").and_then(Value::as_bool).unwrap_or(false);

    let dir = root.join(&name);
    let path = dir.join("SKILL.md");

    if path.exists() && !overwrite {
        anyhow::bail!(
            "a skill named `{name}` already exists at {}. Pass overwrite=true to replace it, or choose a different name.",
            path.display()
        );
    }

    let contents = render_skill_file(&name, &description, &body, triggers)?;
    write_atomic(&dir, &path, &contents)?;

    Ok(format!(
        "Saved skill `{name}` to {}. It's now part of your skills catalog — read it back any time with read_file, and it appears in your skills index going forward.",
        path.display()
    ))
}

/// Validate a skill name: kebab-case only (`^[a-z0-9]+(-[a-z0-9]+)*$`). This
/// rejects path traversal (`/`, `..`), uppercase, spaces, and leading/trailing
/// or doubled hyphens — the name is used as a directory, so it must be safe.
fn validate_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("`name` must not be empty");
    }
    let mut prev_hyphen = true; // treat start as "after a hyphen" → forbids leading '-'
    for ch in name.chars() {
        match ch {
            'a'..='z' | '0'..='9' => prev_hyphen = false,
            '-' => {
                if prev_hyphen {
                    anyhow::bail!("`name` must be kebab-case (no leading, trailing, or doubled hyphens): got `{name}`");
                }
                prev_hyphen = true;
            }
            _ => anyhow::bail!(
                "`name` must be kebab-case — lowercase letters, digits, and single hyphens only (no slashes, dots, spaces, or uppercase): got `{name}`"
            ),
        }
    }
    if prev_hyphen {
        anyhow::bail!("`name` must not end with a hyphen: got `{name}`");
    }
    Ok(())
}

/// Extract the optional `triggers` array as a `Vec<String>`, rejecting
/// non-string elements. Absent ⇒ empty.
fn parse_triggers(arguments: &Value) -> anyhow::Result<Vec<String>> {
    match arguments.get("triggers") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("every element of `triggers` must be a string"))
            })
            .collect(),
        Some(_) => anyhow::bail!("`triggers` must be an array of strings"),
    }
}

/// Render the full SKILL.md text: `---` YAML frontmatter `---` then the body.
fn render_skill_file(name: &str, description: &str, body: &str, triggers: Vec<String>) -> anyhow::Result<String> {
    let fm = Frontmatter {
        name: name.to_owned(),
        description: description.to_owned(),
        triggers,
    };
    let yaml = serde_yml::to_string(&fm).map_err(|e| anyhow::anyhow!("serializing skill frontmatter: {e}"))?;
    // serde_yml emits a trailing newline on the mapping; frame it with the
    // fences and ensure the body ends with exactly one newline.
    Ok(format!("---\n{yaml}---\n\n{}\n", body.trim_end_matches('\n')))
}

/// `~/.smooth/skills` — the user-level skills root the daemon discovers.
fn skills_root() -> anyhow::Result<PathBuf> {
    let home = dirs_next::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve the home directory to locate ~/.smooth/skills"))?;
    Ok(home.join(".smooth").join("skills"))
}

/// Atomically write `contents` to `path` (temp file in `dir` + rename), creating
/// `dir` first. Rename is atomic on the same filesystem, so a concurrent reader
/// never sees a half-written SKILL.md.
fn write_atomic(dir: &std::path::Path, path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| anyhow::anyhow!("creating {}: {e}", dir.display()))?;
    let tmp = dir.join(".SKILL.md.tmp");
    std::fs::write(&tmp, contents).map_err(|e| anyhow::anyhow!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| anyhow::anyhow!("finalizing {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn validate_name_accepts_kebab_case() {
        for ok in ["deploy", "deploy-web", "a1", "add-a-show", "x9-y2"] {
            assert!(validate_name(ok).is_ok(), "{ok} should be valid");
        }
    }

    #[test]
    fn validate_name_rejects_traversal_and_bad_shapes() {
        for bad in [
            "",
            "-lead",
            "trail-",
            "double--hyphen",
            "Up",
            "has space",
            "a/b",
            "..",
            "../x",
            "a.b",
            "a_b",
            "a/../b",
        ] {
            assert!(validate_name(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn create_writes_wellformed_skill_that_round_trips() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("deploy-web/SKILL.md");
        let out = write_skill_to(
            root.path(),
            &json!({
                "name": "deploy-web",
                "description": "Deploy the web app: run CI, then trigger the GH action. Use when asked to ship web.",
                "body": "1. Run `pnpm check-all`.\n2. Trigger the deploy workflow.",
                "triggers": ["deploy web", "ship the site"]
            }),
        )
        .unwrap();
        assert!(out.contains("deploy-web"), "confirms the name: {out}");
        assert!(path.is_file(), "SKILL.md written");

        // Round-trip: the file parses back through the canonical skills parser.
        let skill = smooth_cast::skills::parse_skill_file(&path, smooth_cast::skills::SkillSource::UserSmooth)
            .unwrap()
            .expect("frontmatter parses");
        assert_eq!(skill.name, "deploy-web");
        assert!(skill.description.contains("Deploy the web app"));
        assert_eq!(skill.triggers, vec!["deploy web", "ship the site"]);
        assert!(skill.body.contains("pnpm check-all"), "body preserved: {:?}", skill.body);
    }

    #[test]
    fn create_omits_triggers_when_none_given() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("no-trig/SKILL.md");
        write_skill_to(
            root.path(),
            &json!({"name": "no-trig", "description": "no trigger phrases here", "body": "Just do it."}),
        )
        .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("triggers:"), "no triggers key when none supplied: {raw}");
        let skill = smooth_cast::skills::parse_skill_file(&path, smooth_cast::skills::SkillSource::UserSmooth)
            .unwrap()
            .unwrap();
        assert!(skill.triggers.is_empty());
    }

    #[test]
    fn create_refuses_overwrite_without_flag_and_allows_with_it() {
        let root = tempfile::tempdir().unwrap();
        write_skill_to(root.path(), &json!({"name": "dup", "description": "first version", "body": "v1"})).unwrap();

        // Second create without overwrite → error, original untouched.
        let err = write_skill_to(root.path(), &json!({"name": "dup", "description": "second", "body": "v2"})).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        let path = root.path().join("dup/SKILL.md");
        assert!(std::fs::read_to_string(&path).unwrap().contains("v1"), "original body preserved");

        // With overwrite=true → replaces.
        write_skill_to(root.path(), &json!({"name": "dup", "description": "second", "body": "v2", "overwrite": true})).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("v2"), "overwrite replaced the body");
    }

    #[test]
    fn create_rejects_bad_name_before_touching_disk() {
        let root = tempfile::tempdir().unwrap();
        let err = write_skill_to(root.path(), &json!({"name": "../evil", "description": "x", "body": "y"})).unwrap_err();
        assert!(err.to_string().contains("kebab-case"), "{err}");
        // A rejected name writes nothing under the root.
        assert!(std::fs::read_dir(root.path()).unwrap().next().is_none(), "nothing written for a bad name");
    }

    #[test]
    fn create_rejects_missing_required_and_blank_fields() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        assert!(write_skill_to(r, &json!({"name": "x", "description": "d"})).is_err(), "missing body");
        assert!(
            write_skill_to(r, &json!({"name": "x", "description": "  ", "body": "b"})).is_err(),
            "blank description"
        );
        assert!(
            write_skill_to(r, &json!({"name": "x", "description": "d", "body": "   "})).is_err(),
            "blank body"
        );
        assert!(
            write_skill_to(r, &json!({"name": "x", "description": "d", "body": "b", "triggers": [7]})).is_err(),
            "non-string trigger"
        );
    }

    #[test]
    fn render_escapes_yaml_metacharacters_in_description() {
        // A description with a colon + quotes must still parse back to the exact
        // string — serde_yml quoting handles it.
        let tricky = "Fixes the: \"weird\" case — a, b, c";
        let contents = render_skill_file("x", tricky, "body", vec![]).unwrap();
        let skill = smooth_cast::skills::parse_skill_string(&contents, std::path::Path::new("/tmp/x/SKILL.md"), smooth_cast::skills::SkillSource::UserSmooth)
            .unwrap()
            .unwrap();
        assert_eq!(skill.description, tricky, "description survives YAML round-trip");
    }
}
