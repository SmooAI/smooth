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

use crate::attention::{self, Attention};
use crate::close::{self, Scope};
use crate::directory;
use crate::fleet;
use crate::gate::Gate;
use crate::harness::{self, Harness, Health};
use crate::keymap::{Action, Chord, Keymap, Platform};
use crate::pane::{Direction, Rect, Tab};
use crate::session::{Session, SessionState};
use crate::surfaces::Surfaces;
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

/// `keymap.json`: chord parsing, both platforms' default tables (the spec §9
/// table, pinned), and the override file with its problems and conflicts.
fn keymap_vectors() -> Value {
    let mut cases = Vec::new();
    for raw in [
        "ctrl+shift+n",
        "Ctrl + Shift + N",
        "alt+ctrl+=",
        "ctrl++",
        "ctrl+plus",
        "super+return",
        "ctrl+pgdn",
        "ctrl+f12",
        "ctrl+ctrl+a",
        "ctrl+a+b",
        "ctrl+banana",
        "ctrl+",
        "",
    ] {
        let c = Chord::parse(raw);
        cases.push(case(
            "parse",
            json!({ "fn": "parse", "text": raw }),
            json!({ "chord": c, "wire": c.as_ref().map(Chord::wire) }),
        ));
    }
    for platform in [Platform::Mac, Platform::Other] {
        let k = Keymap::defaults(platform);
        let table: Vec<Value> = Action::ALL
            .iter()
            .map(|a| {
                let c = k.chord(*a);
                json!({ "action": a.name(), "wire": c.as_ref().map(Chord::wire), "display": c.as_ref().map(|c| c.display(platform)) })
            })
            .collect();
        cases.push(case(
            "defaults",
            json!({ "fn": "defaults", "platform": platform }),
            json!({ "bindings": table, "conflicts": k.conflicts().len() }),
        ));
    }
    for (name, text) in [
        (
            "rebind, unbind, re-typed default, and every kind of problem",
            "# mine\n[other]\nnewTab = \"ctrl+q\"\n[keys]\nnewTab = \"ctrl+shift+y\" # comment\nclosePane = \"\"\nsplitRight = \"ctrl+shift+d\"\nbogus = \"ctrl+x\"\nkill = \"k\"\ninbox = \"ctrl+nope\"\nnot a line\n",
        ),
        ("a conflict is reported, and menu order wins it", "[keys]\nnewTab = \"ctrl+shift+w\"\n"),
    ] {
        let k = Keymap::parse(text, Platform::Other);
        let overrides: Vec<Value> = k
            .overrides
            .iter()
            .map(|(a, c)| json!({ "action": a.name(), "wire": c.as_ref().map(Chord::wire) }))
            .collect();
        let conflicts: Vec<Value> = k
            .conflicts()
            .iter()
            .map(|(c, v)| json!({ "wire": c.wire(), "actions": v.iter().map(|a| a.name()).collect::<Vec<_>>() }))
            .collect();
        let fired = Chord::parse("ctrl+shift+w").and_then(|c| k.action_for(&c)).map(Action::name);
        cases.push(case(
            name,
            json!({ "fn": "parse_file", "platform": Platform::Other, "text": text }),
            json!({ "overrides": overrides, "problems": k.problems, "conflicts": conflicts, "ctrl+shift+w": fired }),
        ));
    }
    file("SmoothFlow-Client-Spec §9 keymap", &cases)
}

/// `harness.json`: the New Session kind picker.
fn harness_vectors() -> Value {
    let mut missing = Harness::new("codex");
    missing.installed = false;
    missing.reason = Some("codex not on PATH".into());
    let mut missing_no_reason = Harness::new("gemini");
    missing_no_reason.installed = false;
    let mut degraded = Harness::new("opencode");
    degraded.display_name = "OpenCode".into();
    degraded.health = Some(Health {
        verdict: "degraded".into(),
        reason: Some("hooks untrusted".into()),
        fix: Some("th harness enable opencode".into()),
    });
    let mut hidden = Harness::new("aider");
    hidden.hidden = true;
    let mut claude = Harness::new("claude");
    claude.display_name = "Claude Code".into();
    let all = vec![missing, Harness::new("shell"), claude, degraded, hidden, missing_no_reason];
    let only_missing = vec![all[0].clone()];
    let mut cases = Vec::new();
    for (name, list, shell) in [
        ("engine order, hidden dropped, shell last, disabled and degraded", &all, true),
        ("no shell row when the caller has none (fan-out)", &all, false),
        ("nothing startable but shell", &only_missing, true),
        ("nothing at all", &Vec::new(), false),
    ] {
        let rows = harness::picker(list, shell);
        cases.push(case(
            name,
            json!({ "harnesses": list, "include_shell": shell }),
            json!({ "rows": rows, "default_kind": harness::default_kind(&rows) }),
        ));
    }
    file("SmoothFlow-Client-Spec §6 kind picker", &cases)
}

/// `attention.json`: which attentions are approvable, and what they show.
fn attention_vectors() -> Value {
    let a = |reason: &str, detail: Option<&str>, request_id: Option<&str>| Attention {
        reason: reason.into(),
        detail: detail.map(Into::into),
        request_id: request_id.map(Into::into),
        resume_at: None,
    };
    let mut cases = Vec::new();
    for (name, att) in [
        ("permission with a request id", Some(a("permission", Some("rm -rf target"), Some("r1")))),
        ("question with a request id", Some(a("question", Some("Which branch?"), Some("r2")))),
        ("no detail still says something", Some(a("permission", None, Some("r3")))),
        ("no request id is not approvable", Some(a("permission", Some("x"), None))),
        ("a blank request id is not approvable", Some(a("permission", Some("x"), Some("  ")))),
        ("usage limit is not approvable", Some(a("usage_limit", None, Some("r4")))),
        ("no attention", None),
    ] {
        cases.push(case(
            name,
            json!({ "attention": att }),
            json!({ "approval": attention::approval(att.as_ref()) }),
        ));
    }
    file("SmoothFlow-Client-Spec §7 approvals", &cases)
}

/// `surfaces.json`: tabs of panes, scripted, with the window's state after each step.
fn surfaces_vectors() -> Value {
    let scripts: Vec<(&str, Vec<Value>)> = vec![
        (
            "close collapses pane, then tab, then empties the last",
            vec![
                json!({ "op": "show", "session": "fs-a" }),
                json!({ "op": "split", "direction": "right" }),
                json!({ "op": "close_pane" }),
                json!({ "op": "new_tab", "session": "fs-b" }),
                json!({ "op": "close_pane" }),
                json!({ "op": "close_pane" }),
                json!({ "op": "close_pane" }),
            ],
        ),
        (
            "tabs insert after the active one, cycle and wrap, close whole",
            vec![
                json!({ "op": "new_tab", "session": "a" }),
                json!({ "op": "new_tab", "session": "b" }),
                json!({ "op": "cycle", "delta": 1 }),
                json!({ "op": "new_tab", "session": "c" }),
                json!({ "op": "cycle", "delta": -1 }),
                json!({ "op": "split", "direction": "down" }),
                json!({ "op": "close_tab" }),
                json!({ "op": "close_tab" }),
                json!({ "op": "close_tab" }),
                json!({ "op": "close_tab" }),
            ],
        ),
        (
            "a session going away empties its panes",
            vec![
                json!({ "op": "show", "session": "a" }),
                json!({ "op": "split", "direction": "right" }),
                json!({ "op": "new_tab", "session": "a" }),
                json!({ "op": "forget", "session": "a" }),
            ],
        ),
    ];
    let mut cases = Vec::new();
    for (name, ops) in scripts {
        let mut s = Surfaces::new();
        let mut steps = Vec::new();
        for op in &ops {
            let dir = serde_json::from_value::<Direction>(op["direction"].clone()).ok();
            let session = op["session"].as_str();
            let before = json!({ "scope": s.close_scope(), "shown_elsewhere": s.shown_elsewhere() });
            let returned = match op["op"].as_str().unwrap_or("") {
                "show" => {
                    if let Some(x) = session {
                        s.show(x);
                    }
                    Value::Null
                }
                "split" => {
                    if let Some(d) = dir {
                        s.split(d);
                    }
                    Value::Null
                }
                "new_tab" => {
                    s.new_tab(session);
                    Value::Null
                }
                "cycle" => {
                    s.cycle(op["delta"].as_i64().and_then(|d| isize::try_from(d).ok()).unwrap_or(0));
                    Value::Null
                }
                "close_pane" => json!(s.close_pane()),
                "close_tab" => json!(s.close_tab()),
                "forget" => {
                    if let Some(x) = session {
                        s.forget(x);
                    }
                    Value::Null
                }
                _ => Value::Null,
            };
            let tabs: Vec<Value> = s
                .tabs
                .iter()
                .map(|t| json!({ "panes": t.panes(), "focused": t.focused, "sessions": t.sessions }))
                .collect();
            steps.push(json!({ "before": before, "returned": returned, "tabs": tabs, "active": s.active }));
        }
        cases.push(case(
            name,
            json!({ "start": "one tab, one empty pane (id 1)", "ops": ops }),
            json!({ "steps": steps }),
        ));
    }
    file("SmoothFlow-Client-Spec §5 tabs and close scope", &cases)
}

/// `diff.json` (spec §14, th-26f5b9): the Diff viewer's pure rules.
#[allow(clippy::too_many_lines, reason = "one table of cases per rule, kept together")]
fn diff_vectors() -> Value {
    use crate::diff::{self as d, Base, File, Hunk, Line, LineKind};
    let line = |k: LineKind| Line {
        kind: k,
        old: None,
        new: None,
        text: String::new(),
    };
    let mut cases = Vec::new();
    for (name, kinds) in [
        ("context only", vec![LineKind::Ctx, LineKind::Ctx]),
        (
            "a change block pairs del i with add i",
            vec![LineKind::Ctx, LineKind::Del, LineKind::Del, LineKind::Add, LineKind::Ctx],
        ),
        ("more adds than dels", vec![LineKind::Del, LineKind::Add, LineKind::Add, LineKind::Add]),
        (
            "adds with no dels are right-only",
            vec![LineKind::Ctx, LineKind::Add, LineKind::Add, LineKind::Ctx],
        ),
        ("dels at the end are left-only", vec![LineKind::Ctx, LineKind::Del, LineKind::Del]),
        ("an add then a del is two blocks", vec![LineKind::Add, LineKind::Del]),
    ] {
        let lines: Vec<Line> = kinds.into_iter().map(line).collect();
        let rows = d::side_by_side(&lines);
        cases.push(case(
            name,
            json!({ "fn": "side_by_side", "kinds": lines.iter().map(|l| l.kind).collect::<Vec<_>>() }),
            json!({ "rows": rows }),
        ));
    }
    for (name, paths) in [
        (
            "dirs first, case-insensitive, chains compressed",
            vec![
                "src/b.rs",
                "README.md",
                "src/a.rs",
                "apps/x/y/z.swift",
                "Cargo.toml",
                "src/ui/v.rs",
                "src/Z.rs",
                "docs/a.md",
            ],
        ),
        ("flat", vec!["b.txt", "A.txt", "c.txt"]),
        ("one deep file", vec!["a/b/c/d.rs"]),
    ] {
        cases.push(case(
            name,
            json!({ "fn": "tree", "paths": paths }),
            json!({ "rows": d::tree(&paths), "file_order": d::file_order(&paths) }),
        ));
    }
    let f = |path: &str, hunks: usize| File {
        path: path.into(),
        old_path: None,
        status: "modified".into(),
        added: 3,
        deleted: 1,
        binary: false,
        noise: None,
        collapsed_by_default: false,
        hunks_omitted: None,
        hunks: (0..hunks)
            .map(|i| Hunk {
                id: format!("{path}#{i}"),
                lines: vec![],
            })
            .collect(),
    };
    let lock = File {
        noise: Some("lockfile".into()),
        collapsed_by_default: true,
        hunks_omitted: Some("collapsed".into()),
        ..f("Cargo.lock", 0)
    };
    let budget = File {
        hunks_omitted: Some("budget".into()),
        ..f("big.rs", 0)
    };
    let binary = File {
        binary: true,
        ..f("logo.png", 0)
    };
    let mode = File {
        status: "mode_changed".into(),
        added: 0,
        deleted: 0,
        ..f("run.sh", 0)
    };
    for (name, file, viewed) in [
        ("a plain file is expanded", f("a.rs", 2), false),
        ("viewed folds it", f("a.rs", 2), true),
        ("noise starts collapsed and must be fetched", lock.clone(), false),
        ("a budget stub is expanded but must be fetched", budget, false),
        ("binary has nothing inline", binary, false),
        ("a mode-only change has nothing inline", mode, false),
    ] {
        cases.push(case(
            name,
            json!({ "fn": "display", "file": file, "viewed": viewed }),
            json!({ "display": d::display(&file, viewed), "viewed_key": d::viewed_key(&file) }),
        ));
    }
    let files = vec![f("src/b.rs", 2), lock, f("src/a.rs", 1), f("README.md", 1)];
    let paths: Vec<&str> = files.iter().map(|x| x.path.as_str()).collect();
    let order = d::file_order(&paths);
    let collapsed: Vec<bool> = files.iter().map(|x| d::display(x, false).collapsed).collect();
    let mut steps = Vec::new();
    let mut at = None;
    for forward in [true, true, true, true, true, false, false] {
        at = d::next_hunk(&files, &order, &collapsed, at, forward).or(at);
        steps.push(json!({ "forward": forward, "at": at }));
    }
    cases.push(case(
        "n / p walk visible hunks in tree order and stop at the ends",
        json!({ "fn": "next_hunk", "paths": paths, "hunks": files.iter().map(|x| x.hunks.len()).collect::<Vec<_>>(), "collapsed": collapsed, "order": order }),
        json!({ "steps": steps }),
    ));
    let mut fsteps = Vec::new();
    let mut fat = None;
    for forward in [true, true, true, true, true, false] {
        fat = d::next_file(&order, fat, forward).or(fat);
        fsteps.push(json!({ "forward": forward, "at": fat }));
    }
    cases.push(case(
        "] / [ walk every file in tree order",
        json!({ "fn": "next_file", "order": order }),
        json!({ "steps": fsteps }),
    ));
    for kind in ["claude", "codex", "shell"] {
        cases.push(case(
            "default base",
            json!({ "fn": "default_base", "kind": kind }),
            json!(d::default_base(kind)),
        ));
    }
    for (base, label) in [
        (Base::Turn, None),
        (Base::Uncommitted, None),
        (Base::Branch, Some("merge base with origin/main")),
        (Base::Branch, Some("merge base with master")),
        (Base::Branch, Some("HEAD")),
    ] {
        let r = label.and_then(d::branch_ref_from_label);
        cases.push(case(
            "base label",
            json!({ "fn": "base_label", "base": base, "from_label": label }),
            json!(d::base_label(base, r)),
        ));
    }
    cases.push(case(
        "the viewer's bare keys",
        json!({ "fn": "keys" }),
        json!(d::KEYS.iter().map(|(k, a)| json!({ "key": k, "action": a })).collect::<Vec<_>>()),
    ));
    file("SmoothFlow-Client-Spec §14 Diff viewer", &cases)
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
        ("keymap.json", keymap_vectors()),
        ("harness.json", harness_vectors()),
        ("attention.json", attention_vectors()),
        ("surfaces.json", surfaces_vectors()),
        ("diff.json", diff_vectors()),
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
