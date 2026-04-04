//! Extension system and skill registry for the Smooth Coding TUI.
//!
//! Provides [`Extension`], [`Skill`], and [`ExtensionRegistry`] for loading
//! markdown-based prompt workflows and extension commands.
//! Skills are invoked via `/skill:name` in the TUI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Handler type for extension commands.
pub type ExtensionCommandHandler = Box<dyn Fn(&str) -> anyhow::Result<String> + Send + Sync>;

/// A smooth-code extension that adds tools, commands, and skills.
pub struct Extension {
    /// Unique name of the extension.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Commands provided by this extension.
    pub commands: Vec<ExtensionCommand>,
    /// Skills provided by this extension.
    pub skills: Vec<Skill>,
}

/// A command contributed by an extension.
pub struct ExtensionCommand {
    /// The command name (without the leading `/`).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// The handler function.
    pub handler: ExtensionCommandHandler,
}

/// A skill is a markdown-based reusable prompt workflow.
/// Invoked via `/skill:name` in the TUI.
pub struct Skill {
    /// Unique name of the skill.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Markdown template with `{{variable}}` placeholders.
    pub prompt_template: String,
}

impl Skill {
    /// Load a skill from a markdown file with YAML-style frontmatter.
    ///
    /// Expected format:
    /// ```text
    /// ---
    /// name: skill-name
    /// description: What it does
    /// ---
    ///
    /// Markdown body with {{variable}} placeholders.
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or the frontmatter is malformed.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content)
    }

    /// Parse skill content (frontmatter + body).
    ///
    /// # Errors
    ///
    /// Returns an error if the frontmatter delimiters or required fields are missing.
    pub fn parse(content: &str) -> anyhow::Result<Self> {
        let content = content.trim();
        let Some(rest) = content.strip_prefix("---") else {
            anyhow::bail!("skill file must start with '---' frontmatter delimiter");
        };
        let Some((frontmatter, body)) = rest.split_once("---") else {
            anyhow::bail!("skill file must have closing '---' frontmatter delimiter");
        };

        let mut name = None;
        let mut description = None;

        for line in frontmatter.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim();
                let value = value.trim();
                match key {
                    "name" => name = Some(value.to_string()),
                    "description" => description = Some(value.to_string()),
                    _ => {} // Ignore unknown keys
                }
            }
        }

        let name = name.ok_or_else(|| anyhow::anyhow!("skill frontmatter missing 'name' field"))?;
        let description = description.ok_or_else(|| anyhow::anyhow!("skill frontmatter missing 'description' field"))?;

        Ok(Self {
            name,
            description,
            prompt_template: body.trim().to_string(),
        })
    }

    /// Render the skill template with variable substitution.
    ///
    /// Replaces `{{key}}` with the corresponding value from `vars`.
    /// Unknown variables are left as-is in the output.
    pub fn render(&self, vars: &HashMap<String, String>) -> String {
        let mut result = self.prompt_template.clone();
        for (key, value) in vars {
            result = result.replace(&format!("{{{{{key}}}}}"), value);
        }
        result
    }
}

/// Registry of loaded extensions and skills.
pub struct ExtensionRegistry {
    extensions: Vec<Extension>,
    skills_dir: PathBuf,
}

impl Default for ExtensionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtensionRegistry {
    /// Create a new empty registry with the default skills directory (`~/.smooth/skills/`).
    #[must_use]
    pub fn new() -> Self {
        let skills_dir = dirs_next::home_dir().map_or_else(|| PathBuf::from(".smooth/skills"), |h| h.join(".smooth/skills"));
        Self {
            extensions: Vec::new(),
            skills_dir,
        }
    }

    /// Register an extension.
    pub fn register(&mut self, ext: Extension) {
        self.extensions.push(ext);
    }

    /// Load all `.md` files from a directory as skills.
    ///
    /// Returns the number of skills successfully loaded.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub fn load_skills_dir(&mut self, dir: &Path) -> anyhow::Result<usize> {
        let mut count = 0;
        let entries = std::fs::read_dir(dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                match Skill::from_file(&path) {
                    Ok(skill) => {
                        // Wrap in a standalone extension
                        self.extensions.push(Extension {
                            name: format!("skill:{}", skill.name),
                            description: skill.description.clone(),
                            commands: Vec::new(),
                            skills: vec![skill],
                        });
                        count += 1;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load skill from {}: {e}", path.display());
                    }
                }
            }
        }
        Ok(count)
    }

    /// Find a skill by name across all registered extensions.
    #[must_use]
    pub fn find_skill(&self, name: &str) -> Option<&Skill> {
        self.extensions.iter().flat_map(|ext| &ext.skills).find(|s| s.name == name)
    }

    /// List all available skills across all registered extensions.
    #[must_use]
    pub fn list_skills(&self) -> Vec<&Skill> {
        self.extensions.iter().flat_map(|ext| &ext.skills).collect()
    }

    /// List all extension commands as `(name, description)` pairs.
    #[must_use]
    pub fn list_commands(&self) -> Vec<(&str, &str)> {
        self.extensions
            .iter()
            .flat_map(|ext| &ext.commands)
            .map(|c| (c.name.as_str(), c.description.as_str()))
            .collect()
    }

    /// Return a reference to the configured skills directory.
    #[must_use]
    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SKILL: &str = "\
---
name: code-review
description: Perform a thorough code review
---

Please review the following code for:
- Security vulnerabilities
- Performance issues

Focus on {{file_path}} if specified, otherwise review recent changes.
";

    #[test]
    fn skill_from_file_parses_frontmatter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("review.md");
        std::fs::write(&path, SAMPLE_SKILL).expect("write");

        let skill = Skill::from_file(&path).expect("parse");
        assert_eq!(skill.name, "code-review");
        assert_eq!(skill.description, "Perform a thorough code review");
        assert!(skill.prompt_template.contains("Security vulnerabilities"));
    }

    #[test]
    fn skill_render_substitutes_variables() {
        let skill = Skill::parse(SAMPLE_SKILL).expect("parse");
        let mut vars = HashMap::new();
        vars.insert("file_path".to_string(), "src/main.rs".to_string());

        let rendered = skill.render(&vars);
        assert!(rendered.contains("src/main.rs"));
        assert!(!rendered.contains("{{file_path}}"));
    }

    #[test]
    fn skill_render_leaves_unknown_variables() {
        let skill = Skill::parse(SAMPLE_SKILL).expect("parse");
        let vars = HashMap::new(); // no substitutions

        let rendered = skill.render(&vars);
        assert!(rendered.contains("{{file_path}}"));
    }

    #[test]
    fn registry_register_and_list_skills() {
        let mut reg = ExtensionRegistry::new();
        let skill = Skill::parse(SAMPLE_SKILL).expect("parse");
        reg.register(Extension {
            name: "test-ext".to_string(),
            description: "Test extension".to_string(),
            commands: Vec::new(),
            skills: vec![skill],
        });

        let skills = reg.list_skills();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "code-review");
    }

    #[test]
    fn registry_find_skill_by_name() {
        let mut reg = ExtensionRegistry::new();
        let skill = Skill::parse(SAMPLE_SKILL).expect("parse");
        reg.register(Extension {
            name: "test-ext".to_string(),
            description: "Test extension".to_string(),
            commands: Vec::new(),
            skills: vec![skill],
        });

        assert!(reg.find_skill("code-review").is_some());
        assert!(reg.find_skill("nonexistent").is_none());
    }

    #[test]
    fn load_skills_dir_loads_md_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("review.md"), SAMPLE_SKILL).expect("write");
        std::fs::write(dir.path().join("not-a-skill.txt"), "ignored").expect("write");

        let mut reg = ExtensionRegistry::new();
        let count = reg.load_skills_dir(dir.path()).expect("load");
        assert_eq!(count, 1);
        assert!(reg.find_skill("code-review").is_some());
    }

    #[test]
    fn extension_command_invocation() {
        let mut reg = ExtensionRegistry::new();
        reg.register(Extension {
            name: "greet-ext".to_string(),
            description: "Greeting extension".to_string(),
            commands: vec![ExtensionCommand {
                name: "greet".to_string(),
                description: "Say hello".to_string(),
                handler: Box::new(|args| Ok(format!("Hello, {args}!"))),
            }],
            skills: Vec::new(),
        });

        let cmds = reg.list_commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].0, "greet");

        // Invoke the handler directly
        let ext = &reg.extensions[0];
        let cmd = &ext.commands[0];
        let result = (cmd.handler)("world").expect("handler");
        assert_eq!(result, "Hello, world!");
    }
}
