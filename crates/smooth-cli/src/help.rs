//! The glowed-up `th` help surface + the universal `ai` explainer.
//! Pearl th-7f1da8; design language: Presence (.claude/skills/smooth-glow-up).
//!
//! Two things live here:
//!
//! 1. **The custom top-level help** — bare `th --help` renders a grouped,
//!    branded screen instead of clap's flat 40-command wall. The groups are a
//!    hand-curated table, and a test cross-checks it against the real clap
//!    tree in BOTH directions, so it can never silently drift when a command
//!    is added or removed. `th --help-full` still prints the native clap help.
//!
//! 2. **The `ai` explainer** — append `ai` to any command path
//!    (`th smoo org ai`, `th pearls ai`, `th ai`) to get a plain-markdown
//!    guide generated from the clap tree (about, usage, subcommands, flags)
//!    plus curated examples. Zero per-module maintenance: new subcommands are
//!    picked up automatically. Output is deliberately unstyled markdown — it
//!    is written to be pasted into (or read by) another AI as much as a human.
//!
//! Styling rules (Presence): the teal→blue gradient is the `th` wordmark's
//! and appears exactly once, on the wordmark; section headers are bold; the
//! single accent color is teal on command names; descriptions are dimmed.
//! Everything routes through `anstream`, so a pipe or NO_COLOR gets plain
//! text.

use anstream::println;
use clap::CommandFactory;
use owo_colors::OwoColorize;

use crate::gradient;

/// The curated top-level help: (section, [(command, one-liner)]).
///
/// The one-liners are intentionally SHORTER than the clap `about` strings —
/// this screen is a map, not a manual. `help_sync` asserts every visible
/// clap subcommand appears here and every row here exists in clap.
const SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "Smoo AI platform",
        &[(
            "smoo",
            "everything that talks to smoo.ai — also installed as the `smoo` binary (auth, crm, config, agents, analytics, …)",
        )],
    ),
    (
        "Big Smooth — the always-on agent",
        &[
            ("up", "start Big Smooth on this host"),
            ("down", "stop it"),
            ("status", "system health at a glance"),
            ("daemon", "run / drive the daemon directly"),
            ("operator", "dogfood the polyglot engine servers"),
            ("web", "open the dashboard in your browser"),
            ("inbox", "reviews + notifications needing you"),
            ("run", "run a pearl through an operative"),
            ("pause", "halt a running operative"),
            ("resume", "resume a paused operative"),
            ("steer", "send mid-run guidance"),
            ("cancel", "stop a run"),
            ("approve", "approve a pending review gate"),
            ("operatives", "list / kill operatives"),
            ("access", "operative access control"),
        ],
    ),
    (
        "Work — pearls, repos, CI",
        &[
            ("pearls", "the built-in work-item tracker"),
            ("project", "pearl projects in the global registry"),
            ("db", "this repo's pearl store: status / backup / path"),
            ("jira", "sync pearls with Jira"),
            ("attest", "run CI checks locally and credit the passes"),
            ("worktree", "git worktree management"),
            ("hooks", "git hook management"),
            ("prime", "print the workflow-rules context block"),
        ],
    ),
    (
        "Agent mail — the machine-local bus",
        &[
            ("agent", "register / claim this session's handle"),
            ("msg", "send and read agent-to-agent mail"),
        ],
    ),
    (
        "Coding",
        &[
            ("code", "the interactive coding TUI (also bare `th`)"),
            ("claude", "supervise Claude Code sessions in tmux"),
            ("skills", "list skills in this workspace"),
            ("harness", "set up Claude Code / Codex / OpenCode with the smooth toolbox"),
            ("mcp", "MCP servers — including `th mcp serve`, the shared agent bus"),
            ("plugin", "file-based CLI-wrapper plugins"),
            ("ext", "SEP extensions"),
        ],
    ),
    (
        "LLM providers",
        &[
            ("model", "provider credentials (Anthropic, Smoo AI gateway, …)"),
            ("providers", "bring-your-own OpenAI-compatible servers"),
            ("cast", "live model groups the provider exposes"),
            ("routing", "which model handles thinking / coding / …"),
        ],
    ),
    (
        "System",
        &[
            ("doctor", "health check, auto-fix, guided setup"),
            ("service", "run Smooth as a background service"),
            ("audit", "tool-usage audit logs"),
            ("tailscale", "tailnet devices Smooth can see"),
        ],
    ),
];

/// Curated examples for the `ai` explainer, keyed by the space-joined
/// command path (`""` = root). Everything else in the explainer is generated
/// from the clap tree, so only add rows here when a worked example genuinely
/// helps.
const EXAMPLES: &[(&str, &[&str])] = &[
    (
        "",
        &[
            "th pearls ready                     # what should I work on?",
            "smoo auth login                     # sign in to the Smoo AI platform",
            "smoo crm contacts list --json       # any platform read, machine-readable",
            "th harness enable all               # wire Claude Code / Codex / OpenCode to the toolbox",
        ],
    ),
    (
        "smoo",
        &[
            "smoo auth login",
            "smoo org list && smoo org switch <name>",
            "smoo config get databaseUrl --environment=production",
            "smoo analytics query --preset total_contacts",
            "smoo campaigns send <id>            # preview; add --confirm to really send",
        ],
    ),
    ("smoo org", &["smoo org list", "smoo org switch smoo", "smoo org show"]),
    (
        "smoo config",
        &[
            "smoo config list --environment=production",
            "smoo config set myKey 'value' --environment=development",
            "smoo config diff",
        ],
    ),
    (
        "smoo crm",
        &["smoo crm contacts list --json", "smoo crm deals list", "smoo crm pipeline forecast"],
    ),
    (
        "pearls",
        &[
            "th pearls ready",
            "th pearls create --title=\"Fix X\" --type=bug --priority=2",
            "th pearls update th-xxxxxx --status=in_progress",
            "th pearls close th-xxxxxx && th pearls push",
        ],
    ),
    ("msg", &["th msg send <agent|all> \"body\"", "th msg inbox", "th msg watch --once --json"]),
    ("agent", &["th agent whoami", "th agent claim my-task-name", "th agent list"]),
    ("attest", &["th attest --all", "th attest rust --remote smoo-hub", "th attest --status"]),
    ("harness", &["th harness enable all", "th harness status"]),
];

/// Render the custom top-level help. Called for bare `th --help` / `-h` /
/// `th help`; `th --help-full` bypasses this for the native clap tree.
pub fn print_top_level() {
    let version = env!("TH_VERSION");
    println!("{} {}", gradient::smooth(), format!("v{version}").dimmed());
    println!("{}", "your local agent toolbox + the Smoo AI platform CLI".dimmed());

    let width = SECTIONS
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(10);

    for (section, rows) in SECTIONS {
        println!();
        println!("{}", section.bold());
        for (name, blurb) in *rows {
            println!("  {:width$}  {}", name.cyan(), blurb.dimmed(), width = width);
        }
    }

    println!();
    println!("{}", "Getting around".bold());
    println!("  {}", "th <command> --help        details for any command (or -h for a summary)".dimmed());
    println!(
        "  {}",
        "th <command…> ai           a markdown guide to that command, written for humans and AIs".dimmed()
    );
    println!("  {}", "th --help-full             the full flat command tree (native help)".dimmed());
    println!("  {}", "smoo <resource> <verb>     the platform half under its own name (same binary)".dimmed());
}

/// The `ai` explainer: `path` is the command chain WITHOUT the trailing
/// `ai` (empty = root). Returns false when the path names no real command,
/// so the caller can fall through to clap's own error.
pub fn print_ai_explainer(path: &[String]) -> bool {
    let root = crate::Cli::command();
    let mut node = &root;
    for part in path {
        match node
            .get_subcommands()
            .find(|c| c.get_name() == part.as_str() || c.get_all_aliases().any(|a| a == part.as_str()))
        {
            Some(next) => node = next,
            None => return false,
        }
    }

    let full: String = std::iter::once("th").chain(path.iter().map(String::as_str)).collect::<Vec<_>>().join(" ");
    println!("# {full}");
    println!();
    let about = node
        .get_long_about()
        .or_else(|| node.get_about())
        .map_or_else(|| "Smoo AI CLI.".to_string(), std::string::ToString::to_string);
    println!("{about}");

    let subs: Vec<_> = node.get_subcommands().filter(|c| !c.is_hide_set() && c.get_name() != "help").collect();
    if !subs.is_empty() {
        println!();
        println!("## Subcommands");
        println!();
        for c in &subs {
            let one_liner = c
                .get_about()
                .map_or_else(String::new, |a| a.to_string().lines().next().unwrap_or("").to_string());
            let aliases: Vec<_> = c.get_visible_aliases().collect();
            let alias_note = if aliases.is_empty() {
                String::new()
            } else {
                format!(" (alias: {})", aliases.join(", "))
            };
            println!("- `{full} {}`{} — {}", c.get_name(), alias_note, one_liner);
        }
    }

    let args: Vec<_> = node
        .get_arguments()
        .filter(|a| !a.is_hide_set() && a.get_id() != "help" && a.get_id() != "version")
        .collect();
    if !args.is_empty() {
        println!();
        println!("## Flags");
        println!();
        for a in &args {
            let name = a.get_long().map_or_else(|| a.get_id().to_string(), |l| format!("--{l}"));
            let hint = a
                .get_help()
                .map_or_else(String::new, |h| h.to_string().lines().next().unwrap_or("").to_string());
            println!("- `{name}` — {hint}");
        }
    }

    let key = path.join(" ");
    if let Some((_, examples)) = EXAMPLES.iter().find(|(k, _)| *k == key) {
        println!();
        println!("## Examples");
        println!();
        println!("```bash");
        for e in *examples {
            println!("{e}");
        }
        println!("```");
    }

    println!();
    println!("## Conventions");
    println!();
    println!("- `smoo <resource> <verb>` == `th smoo …` — the platform half of the binary; everything under it authenticates via `smoo auth login`, everything outside it works offline.");
    println!(
        "- Read verbs take `--json` for stable machine-readable output; empty results are stated as confirmed answers, and truncation is always reported."
    );
    println!("- Destructive or spend actions preview first and require an explicit flag (e.g. `smoo campaigns send … --confirm`).");
    println!("- Append `ai` to any command path for this view; `--help` on any command for the native reference.");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible_top_level() -> Vec<String> {
        crate::Cli::command()
            .get_subcommands()
            .filter(|c| !c.is_hide_set() && c.get_name() != "help")
            .map(|c| c.get_name().to_string())
            .collect()
    }

    /// The curated help and the real clap tree may never drift: every visible
    /// command appears in exactly one section, and every row names a real
    /// command.
    #[test]
    fn help_sync_both_directions() {
        let tree = visible_top_level();
        let mut curated: Vec<&str> = SECTIONS.iter().flat_map(|(_, rows)| rows.iter().map(|(n, _)| *n)).collect();
        curated.sort_unstable();
        let dup = curated.windows(2).find(|w| w[0] == w[1]);
        assert!(dup.is_none(), "command listed twice in SECTIONS: {dup:?}");

        for name in &tree {
            assert!(
                curated.contains(&name.as_str()),
                "`{name}` is a visible command but missing from the curated help — add it to a section in help.rs"
            );
        }
        for name in &curated {
            assert!(
                tree.iter().any(|t| t == name),
                "curated help lists `{name}` but no such visible command exists — remove or fix it"
            );
        }
    }

    #[test]
    fn ai_explainer_resolves_paths_and_aliases() {
        assert!(print_ai_explainer(&[]));
        assert!(print_ai_explainer(&["smoo".into()]));
        assert!(print_ai_explainer(&["smoo".into(), "org".into()]));
        // visible alias resolves too
        assert!(print_ai_explainer(&["smoo".into(), "orgs".into()]));
        assert!(!print_ai_explainer(&["smoo".into(), "nonsense".into()]));
    }

    /// CLI-Spec conformance: inside the `smoo` platform tree, every `list`
    /// verb must offer `--json`. (The local-tool tree is tracked as spec debt
    /// in the exemption list — shrink it, never grow it.)
    #[test]
    fn spec_every_platform_list_has_json() {
        fn walk(cmd: &clap::Command, path: String, violations: &mut Vec<String>) {
            for sub in cmd.get_subcommands() {
                let p = format!("{path} {}", sub.get_name());
                if sub.get_name() == "list" && !sub.get_arguments().any(|a| a.get_id() == "json") {
                    violations.push(p.clone());
                }
                walk(sub, p, violations);
            }
        }
        let root = crate::Cli::command();
        let smoo = root.get_subcommands().find(|c| c.get_name() == "smoo").expect("smoo node");
        let mut violations = Vec::new();
        walk(smoo, "smoo".to_string(), &mut violations);
        // Known debt goes here WITH a pearl reference, and only ever shrinks.
        let exempt: &[&str] = &[];
        violations.retain(|v| !exempt.contains(&v.as_str()));
        assert!(violations.is_empty(), "platform `list` verbs without --json (CLI-Spec §flags): {violations:?}");
    }
}
