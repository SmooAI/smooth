//! The conformance vectors (spec §12): the rules in this crate, run over fixed
//! inputs and written as JSON to `spec/vectors/*.json` at the repo root.
//!
//! Every other client replays these files in its own unit tests — XCTest on
//! Mac and iOS, JUnit on Android — asserting the same outputs from its own
//! implementation. The Rust test below fails when a file no longer matches
//! what this crate computes; rerun it with `SMOOTH_FLOW_CLIENT_BLESS=1` after
//! an intentional rule change, and then fix every client that now disagrees.
//!
//! Each file is `{"spec": "…", "cases": [{"name", "input", "expected"}]}`.

use serde_json::{json, Value};

use crate::close::{self, Scope};
use crate::directory;
use crate::fleet;
use crate::gate::Gate;
use crate::pane::{Direction, Rect, Tab};
use crate::session::{Session, SessionState};
use crate::title;

const HOME: &str = "/Users/me";

fn file(spec: &str, cases: &[Value]) -> Value {
    json!({ "spec": spec, "cases": cases })
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "call sites build the values inline with json!; taking them by value keeps those readable"
)]
fn case(name: &str, input: Value, expected: Value) -> Value {
    json!({ "name": name, "input": input, "expected": expected })
}

fn session(id: &str, kind: &str, state: SessionState) -> Session {
    Session::new(id, kind, state)
}

/// `close.json`: scope from the tab's shape, then whether closing asks.
fn close_vectors() -> Value {
    let agent = session("fs-1", "claude", SessionState::Working);
    let idle_agent = session("fs-2", "codex", SessionState::Idle);
    let idle_shell = session("fs-3", "shell", SessionState::Idle);
    let busy_shell = session("fs-4", "shell", SessionState::Working);
    let done = session("fs-5", "claude", SessionState::Done);
    let mut cases = Vec::new();
    for (name, s, panes, tabs, shown_elsewhere, confirm) in [
        ("empty pane closes silently", None, 2, 1, false, true),
        ("working agent in a split asks", Some(&agent), 2, 1, false, true),
        ("idle agent still asks (it holds context)", Some(&idle_agent), 1, 2, false, true),
        ("idle shell at a prompt closes silently", Some(&idle_shell), 1, 2, false, true),
        ("busy shell asks", Some(&busy_shell), 1, 2, false, true),
        ("finished session closes silently", Some(&done), 2, 1, false, true),
        ("a second view of the session closes silently", Some(&agent), 2, 1, true, true),
        ("confirmation off never asks", Some(&agent), 2, 1, false, false),
        ("last pane of the last tab empties, asking as a pane", Some(&agent), 1, 1, false, true),
    ] {
        let scope = Scope::of(panes, tabs);
        let prompt = close::decide(s, scope, shown_elsewhere, confirm);
        cases.push(case(
            name,
            json!({ "session": s, "panes": panes, "tabs": tabs, "shown_elsewhere": shown_elsewhere, "confirm_enabled": confirm }),
            json!({ "scope": scope, "prompt": prompt }),
        ));
    }
    file("SmoothFlow-Client-Spec §5 close semantics", &cases)
}

/// `title.json`: tab titles.
fn title_vectors() -> Value {
    let mut cases = Vec::new();
    for (name, t, pearl, worktree, project) in [
        ("the pearl wins", "fix the parser", Some("th-1"), "/w/x", ""),
        ("a blank pearl is none", "fix the parser", Some("  "), "/w/x", ""),
        ("a path title becomes its folder", "~/dev/smooai/smooai", None, "", ""),
        ("the root stays the root", "/", None, "", ""),
        ("no title: the worktree's folder", "", None, "/Users/me/dev/x/", ""),
        ("home is ~", "", None, "/Users/me", ""),
        ("no worktree: the project's folder", "", None, "", "/w/smooai"),
        ("nothing at all: the kind", "", None, "", ""),
    ] {
        let s = Session {
            title: t.into(),
            pearl_id: pearl.map(Into::into),
            worktree: worktree.into(),
            project: project.into(),
            ..session("fs-1", "claude", SessionState::Idle)
        };
        cases.push(case(
            name,
            json!({ "session": s, "home": HOME }),
            json!({ "title": title::tab_title(&s, HOME) }),
        ));
    }
    file("SmoothFlow-Client-Spec §5 tab titles", &cases)
}

/// `gate.json`: which center tabs are allowed.
fn gate_vectors() -> Value {
    let mut cases = Vec::new();
    for (name, kind, branch, source, packet) in [
        ("nothing loaded yet, row has a branch", "claude", Some("main"), Some("hooks"), None),
        ("shell in a worktree gets diff and PR", "shell", Some("main"), None, None),
        (
            "detached HEAD: diff but no PR",
            "claude",
            None,
            Some("hooks"),
            Some((Some("abc123"), Some("HEAD"))),
        ),
        ("packet without HEAD: not a repo", "aider", Some("main"), Some("scrape"), Some((None, None))),
        (
            "scraped harness: thin activity",
            "aider",
            Some("dev"),
            Some("scrape"),
            Some((Some("abc123"), Some("dev"))),
        ),
        ("unknown harness keeps full activity", "mystery", None, None, None),
    ] {
        let g = Gate::of(kind, branch, source, packet);
        let tabs = [
            crate::gate::CenterTab::Terminal,
            crate::gate::CenterTab::Diff,
            crate::gate::CenterTab::Pr,
            crate::gate::CenterTab::Activity,
        ];
        let why: Vec<Value> = tabs
            .iter()
            .map(|t| json!({ "tab": t, "allowed": g.allows(*t), "why_not": g.why_not(*t) }))
            .collect();
        cases.push(case(
            name,
            json!({ "kind": kind, "branch": branch, "state_source": source, "packet": packet.map(|(h, b)| json!({ "head": h, "branch": b })) }),
            json!({ "gate": g, "tabs": why }),
        ));
    }
    file("SmoothFlow-Client-Spec §4 center tab gating", &cases)
}

/// `directory.json`: the Directory field's helpers.
fn directory_vectors() -> Value {
    let mut cases = Vec::new();
    for p in ["/Users/me/dev/x", "/Users/me", "/Users/meta/x", "/tmp"] {
        cases.push(case(
            "abbreviate",
            json!({ "fn": "abbreviate", "path": p, "home": HOME }),
            json!(directory::abbreviate(p, HOME)),
        ));
    }
    for t in ["~", "~/dev", " /tmp/x ", "smooth", "dev/x", ""] {
        cases.push(case(
            "expanded_path",
            json!({ "fn": "expanded_path", "typed": t, "home": HOME }),
            json!(directory::expanded_path(t, HOME)),
        ));
    }
    for (i, d, n) in [(0usize, -1isize, 3usize), (2, 1, 3), (1, 1, 3), (0, 1, 0), (5, -2, 3)] {
        cases.push(case(
            "moved",
            json!({ "fn": "moved", "index": i, "delta": d, "count": n }),
            json!(directory::moved(i, d, n)),
        ));
    }
    file("SmoothFlow-Client-Spec §6 Directory field", &cases)
}

/// `fleet.json`: sidebar grouping and counts.
fn fleet_vectors() -> Value {
    let s = |id: &str, kind: &str, project: &str, state| Session {
        project: project.into(),
        ..session(id, kind, state)
    };
    let fleet = vec![
        s("a", "claude", "/w/smooth", SessionState::Working),
        s("b", "shell", "/w/smooth", SessionState::Idle),
        s("c", "codex", "/w/smooai", SessionState::NeedsYou),
        s("d", "claude", "/w/smooth", SessionState::Done),
        s("e", "shell", "/w/smooai", SessionState::Limited),
        s("f", "gemini", "/w/refs", SessionState::Starting),
        s("g", "claude", "/w/refs", SessionState::Dead),
    ];
    let only_shells = vec![s("x", "shell", "/w/a", SessionState::Idle), s("y", "shell", "/w/b", SessionState::Working)];
    let cases = vec![
        case(
            "groups by project in first-seen order, shells last; counts",
            json!({ "sessions": fleet }),
            json!({ "groups": fleet::grouped(&fleet), "counts": fleet::counts(&fleet) }),
        ),
        case(
            "only shells is one shells group",
            json!({ "sessions": only_shells }),
            json!({ "groups": fleet::grouped(&only_shells), "counts": fleet::counts(&only_shells) }),
        ),
    ];
    file("SmoothFlow-Client-Spec §4 fleet sidebar", &cases)
}

/// `pane.json`: scripted sequences of pane operations in a 100×100 tab, with
/// the tab's state after each step.
fn pane_vectors() -> Value {
    let bounds = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };
    let scripts: Vec<(&str, Vec<Value>)> = vec![
        (
            "split right, split down, walk around, close",
            vec![
                json!({ "op": "split", "direction": "right", "new": 2 }),
                json!({ "op": "split", "direction": "down", "new": 3 }),
                json!({ "op": "focus", "direction": "left" }),
                json!({ "op": "focus", "direction": "right" }),
                json!({ "op": "focus", "direction": "down" }),
                json!({ "op": "close" }),
                json!({ "op": "close" }),
                json!({ "op": "close" }),
            ],
        ),
        (
            "split left and up insert before the focused pane",
            vec![
                json!({ "op": "split", "direction": "left", "new": 2 }),
                json!({ "op": "split", "direction": "up", "new": 3 }),
                json!({ "op": "focus", "direction": "down" }),
                json!({ "op": "focus", "direction": "right" }),
            ],
        ),
        (
            "zoom blocks focus moves; equalize ends zoom",
            vec![
                json!({ "op": "zoom" }),
                json!({ "op": "split", "direction": "right", "new": 2 }),
                json!({ "op": "zoom" }),
                json!({ "op": "focus", "direction": "left" }),
                json!({ "op": "equalize" }),
                json!({ "op": "focus", "direction": "left" }),
            ],
        ),
    ];
    let mut cases = Vec::new();
    for (name, ops) in scripts {
        let mut tab = Tab::new(1, Some("fs-1"));
        let mut steps = Vec::new();
        for op in &ops {
            let dir = |o: &Value| serde_json::from_value::<Direction>(o["direction"].clone()).ok();
            let returned = match op["op"].as_str().unwrap_or("") {
                "split" => {
                    if let (Some(d), Some(n)) = (dir(op), op["new"].as_u64().and_then(|n| u32::try_from(n).ok())) {
                        tab.split(d, n);
                    }
                    Value::Null
                }
                "close" => json!(tab.close_focused()),
                "focus" => {
                    if let Some(d) = dir(op) {
                        tab.focus(d, bounds);
                    }
                    Value::Null
                }
                "zoom" => {
                    tab.toggle_zoom();
                    Value::Null
                }
                "equalize" => {
                    tab.equalize();
                    Value::Null
                }
                _ => Value::Null,
            };
            steps.push(json!({
                "panes": tab.panes(),
                "focused": tab.focused,
                "zoomed": tab.zoomed,
                "sessions": tab.sessions,
                "returned": returned,
            }));
        }
        cases.push(case(
            name,
            json!({ "start": { "pane": 1, "session": "fs-1" }, "bounds": bounds, "ops": ops }),
            json!({ "steps": steps }),
        ));
    }
    file("SmoothFlow-Client-Spec §5 panes (top-down geometry; ties go to the lower pane id)", &cases)
}

/// Every vector file, by name.
#[must_use]
pub fn all() -> Vec<(&'static str, Value)> {
    vec![
        ("close.json", close_vectors()),
        ("title.json", title_vectors()),
        ("gate.json", gate_vectors()),
        ("directory.json", directory_vectors()),
        ("fleet.json", fleet_vectors()),
        ("pane.json", pane_vectors()),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/vectors")
    }

    /// The committed vectors are exactly what this crate computes. Set
    /// `SMOOTH_FLOW_CLIENT_BLESS=1` to rewrite them after a deliberate change.
    #[test]
    fn committed_vectors_match_the_rules() {
        let bless = std::env::var("SMOOTH_FLOW_CLIENT_BLESS").is_ok_and(|v| v == "1");
        let dir = dir();
        if bless {
            std::fs::create_dir_all(&dir).unwrap();
        }
        for (name, value) in all() {
            let path = dir.join(name);
            let rendered = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
            if bless {
                std::fs::write(&path, &rendered).unwrap();
                continue;
            }
            let committed = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("{} is missing — run with SMOOTH_FLOW_CLIENT_BLESS=1", path.display()))
                // A Windows checkout may still hand us CRLF; the vectors are the same.
                .replace("\r\n", "\n");
            assert_eq!(
                committed, rendered,
                "{name} is stale — rerun with SMOOTH_FLOW_CLIENT_BLESS=1 and update every client"
            );
        }
    }
}
