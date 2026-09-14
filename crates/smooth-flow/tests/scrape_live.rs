//! Live proof of the scraped built-ins against the REAL CLIs (th-e77603).
//!
//! Opt-in and `#[ignore]`d: it needs the CLIs installed, a prepared `$HOME`
//! per harness and an OpenAI-compatible endpoint. Each harness runs on a
//! private engine (`EngineDriver::private`: own flow.db, own tmux server,
//! scratch git worktree) with its BUILT-IN manifest plus a `[launch.env]`
//! pointing the pane at the prepared home, and walks:
//!
//! 1. launch with a prompt → (first-run questions read needs_you and are
//!    answered) → working → idle
//! 2. steer → working → idle
//! 3. a tool/edit request → needs_you → deny → back to idle
//! 4. kill + resume → the manifest's resume argv relaunches and comes back
//!
//! ```bash
//! SMOOTH_SCRAPE_LIVE=aider,goose,crush,cline \
//! SMOOTH_SCRAPE_LIVE_HOMES=/scratch/home \          # <dir>/<harness> is $HOME
//! SMOOTH_SCRAPE_LIVE_PATH=/scratch/bin:/usr/bin:/bin \
//! SMOOTH_SCRAPE_LIVE_ENV='OPENAI_API_BASE=http://127.0.0.1:18777/v1 …' \
//! SMOOTH_SCRAPE_LIVE_MODELS='aider=openai/mock-model,goose=mock-model' \
//! SMOOTH_SCRAPE_LIVE_OUT=/scratch/live \            # timelines + panes
//! cargo test -p smooai-smooth-flow --test scrape_live -- --ignored --nocapture
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use regex::Regex;
use smooth_flow::harness::{Registry, ScrapeRules};
use smooth_flow::harness_validate::{EngineDriver, FlowDriver};
use smooth_flow::SessionState;

/// What to press when a dialog is up: `(pane regex, keys)`, first match wins.
fn answers(harness: &str, deny: bool) -> Vec<(&'static str, Vec<&'static str>)> {
    match (harness, deny) {
        // Never accept "Open documentation url…?": its default opens a browser on the desk.
        ("aider", false) => vec![
            (r"(?im)open documentation url.*:\s*$", vec!["n", "Enter"]),
            (r"(?im)\(y\)es/\(n\)o.*:\s*$", vec!["Enter"]),
        ],
        ("aider", true) => vec![(r"(?im)\(y\)es/\(n\)o.*:\s*$", vec!["n", "Enter"])],
        ("goose", false) => vec![(r"(?im)^◆\s+share anonymous", vec!["Right", "Enter"])],
        ("goose", true) => vec![(r"(?im)^◆\s+goose would like", vec!["Down", "Down", "Enter"])],
        ("crush", false) => vec![(r"(?i)would you like to initialize", vec!["Right", "Enter"])],
        ("crush", true) => vec![(r"(?i)permission required", vec!["Right", "Right", "Enter"])],
        ("cline", false) => vec![(r"(?i)press enter to open, any other key", vec!["Escape"])],
        ("cline", true) => vec![
            (r"(?i)approve tool call\?", vec!["n"]),
            (r"(?i)press enter to open, any other key", vec!["Escape"]),
        ],
        _ => vec![],
    }
}

/// The request that makes each CLI ask for permission (the mock server
/// answers "tool" with a shell tool call and "newfile" with an aider edit).
fn permission_prompt(harness: &str) -> &'static str {
    if harness == "aider" {
        "make a newfile please"
    } else {
        "use a tool please"
    }
}

struct Run {
    harness: String,
    driver: EngineDriver,
    id: String,
    rules: ScrapeRules,
    started: Instant,
    log: String,
    last: Option<(SessionState, Option<String>)>,
}

impl Run {
    fn note(&mut self, msg: &str) {
        let _ = writeln!(self.log, "{:>7.1}s  {msg}", self.started.elapsed().as_secs_f64());
        eprintln!("[{}] {:>7.1}s  {msg}", self.harness, self.started.elapsed().as_secs_f64());
    }

    fn pane(&mut self) -> String {
        self.driver.snapshot(&self.id).unwrap_or_default()
    }

    /// Tick until `done(state)`, answering dialogs from `table`. Returns the
    /// states seen (deduplicated, in order).
    fn drive(&mut self, what: &str, budget: Duration, table: &[(&str, Vec<&str>)], done: impl Fn(&[SessionState]) -> bool) -> Vec<SessionState> {
        let deadline = Instant::now() + budget;
        let mut seen: Vec<SessionState> = Vec::new();
        let mut answered = 0;
        loop {
            self.driver.wait(Duration::from_millis(700));
            let probe = self.driver.probe(&self.id).unwrap();
            let pane = self.pane();
            let rule = self.rules.detect(&pane).rule;
            if self.last.as_ref() != Some(&(probe.state, rule.clone())) {
                self.note(&format!("{what}: state {:?} (text-only rule {:?})", probe.state, rule));
                self.last = Some((probe.state, rule));
            }
            if seen.last() != Some(&probe.state) {
                seen.push(probe.state);
            }
            if done(&seen) {
                return seen;
            }
            if probe.state == SessionState::NeedsYou && answered < 4 {
                // A scrolling CLI keeps answered questions on screen: only its last line is live.
                let live = if self.harness == "aider" {
                    pane.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("")
                } else {
                    pane.as_str()
                };
                if let Some((pat, keys)) = table.iter().find(|(p, _)| Regex::new(p).unwrap().is_match(live)) {
                    self.note(&format!("{what}: answering /{pat}/ with {keys:?}"));
                    for k in keys {
                        self.driver.press(&self.id, k).unwrap();
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    answered += 1;
                    // Let the pane change before the next verdict.
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            assert!(
                Instant::now() < deadline && !probe.state.is_terminal(),
                "[{}] {what}: gave up in {:?} after {seen:?}\n{}\n--- pane ---\n{pane}",
                self.harness,
                probe.state,
                self.log
            );
        }
    }

    fn save_pane(&mut self, label: &str) {
        if let Ok(out) = std::env::var("SMOOTH_SCRAPE_LIVE_OUT") {
            let dir = std::path::Path::new(&out).join(&self.harness);
            std::fs::create_dir_all(&dir).unwrap();
            let pane = self.pane();
            std::fs::write(dir.join(format!("{label}.txt")), pane).unwrap();
        }
    }
}

fn after_working_idle(seen: &[SessionState]) -> bool {
    seen.iter()
        .position(|s| *s == SessionState::Working)
        .is_some_and(|w| seen[w..].contains(&SessionState::Idle))
}

fn kv_list(var: &str, sep: char) -> BTreeMap<String, String> {
    std::env::var(var)
        .unwrap_or_default()
        .split(sep)
        .filter_map(|kv| kv.trim().split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect()
}

fn run_one(harness: &str) -> String {
    let homes = std::env::var("SMOOTH_SCRAPE_LIVE_HOMES").expect("SMOOTH_SCRAPE_LIVE_HOMES");
    let mut env = kv_list("SMOOTH_SCRAPE_LIVE_ENV", ' ');
    env.insert("HOME".into(), format!("{homes}/{harness}"));
    if let Ok(p) = std::env::var("SMOOTH_SCRAPE_LIVE_PATH") {
        env.insert("PATH".into(), p);
    }
    env.entry("LANG".into()).or_insert_with(|| "en_US.UTF-8".into());
    env.entry("TERM".into()).or_insert_with(|| "xterm-256color".into());
    let model = kv_list("SMOOTH_SCRAPE_LIVE_MODELS", ',').get(harness).cloned();

    let registry = Registry::builtin();
    let builtin = registry.get(harness).unwrap_or_else(|| panic!("no built-in `{harness}`"));
    let mut manifest = builtin.clone();
    manifest.launch.env.extend(env);
    let toml_text = toml::to_string(&manifest).unwrap();
    let driver = EngineDriver::private(harness, &toml_text, model).unwrap();
    let mut run = Run {
        harness: harness.to_string(),
        rules: ScrapeRules::compile(&builtin.state.scrape).unwrap(),
        driver,
        id: String::new(),
        started: Instant::now(),
        log: String::new(),
        last: None,
    };

    // 1. launch → (first-run answered) → working → idle.
    run.id = run.driver.launch(harness, "hello live, please answer").unwrap();
    let argv = run.driver.probe(&run.id).unwrap().argv;
    run.note(&format!("launched: {argv:?}"));
    let first = answers(harness, false);
    let seen = run.drive("first turn", Duration::from_secs(180), &first, after_working_idle);
    run.note(&format!("PROVEN first turn: {seen:?}"));
    run.save_pane("1-first-turn-idle");

    // 2. steer → working → idle.
    run.driver.send(&run.id, "tell me more").unwrap();
    let seen = run.drive("steer", Duration::from_secs(120), &first, after_working_idle);
    run.note(&format!("PROVEN steer: {seen:?}"));

    // 3. permission → needs_you → deny → idle.
    run.driver.send(&run.id, permission_prompt(harness)).unwrap();
    let seen = run.drive("permission", Duration::from_secs(120), &[], |s| s.contains(&SessionState::NeedsYou));
    run.note(&format!("PROVEN permission surfaced: {seen:?}"));
    run.save_pane("3-permission");
    let deny = answers(harness, true);
    let seen = run.drive("deny", Duration::from_secs(120), &deny, |s| {
        s.iter().any(|x| *x != SessionState::NeedsYou) && s.last() == Some(&SessionState::Idle)
    });
    run.note(&format!("PROVEN denied → idle: {seen:?}"));
    run.save_pane("3-after-deny-idle");

    // 4. kill + resume → back.
    let probe = run.driver.kill(&run.id, true).unwrap();
    run.note(&format!("resumed with argv {:?}", probe.argv));
    let seen = run.drive("resume", Duration::from_secs(120), &first, |s| {
        matches!(s.last(), Some(SessionState::Idle | SessionState::NeedsYou))
    });
    run.note(&format!("PROVEN resume came back: {seen:?}"));
    run.save_pane("4-resumed");
    run.driver.cleanup(&run.id);

    if let Ok(out) = std::env::var("SMOOTH_SCRAPE_LIVE_OUT") {
        std::fs::create_dir_all(format!("{out}/{harness}")).unwrap();
        std::fs::write(format!("{out}/{harness}/timeline.txt"), &run.log).unwrap();
    }
    run.log
}

#[test]
#[ignore = "needs the real CLIs + a prepared home + an OpenAI-compatible endpoint; see the module docs"]
fn scraped_builtins_drive_real_clis() {
    let which = std::env::var("SMOOTH_SCRAPE_LIVE").expect("SMOOTH_SCRAPE_LIVE=aider,goose,crush,cline");
    for h in which.split(',').map(str::trim).filter(|h| !h.is_empty()) {
        let log = run_one(h);
        eprintln!("==== {h} ====\n{log}");
    }
}
