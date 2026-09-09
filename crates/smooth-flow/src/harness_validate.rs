//! Validate a drafted harness manifest by running it (pearl th-473294): a
//! fixed state machine over a [`FlowDriver`] — launch → working → idle, steer
//! → working → idle, kill+resume → back — that records what it could PROVE
//! and what it could not, never "passed" by assumption.
//!
//! The driver is a trait so the machine is unit-tested against a scripted
//! fake with a fake clock; [`EngineDriver`] is the real one: a **private**
//! [`Engine`] (its own SQLite file, its own tmux socket, a scratch `$HOME`
//! holding only the draft manifest, a scratch git repo as the worktree) so a
//! validation run never touches the user's `flow.db`, tmux server or
//! `~/.smooth/harnesses/`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::engine::{Engine, EngineConfig, NewRequest};
use crate::store::SessionState;
use crate::tmux;

/// The prompt every validation session is launched with: short, no tools,
/// a one-word answer — so "reached idle" means "did a turn", not "did work".
pub const LAUNCH_PROMPT: &str = "Reply with exactly the single word READY and nothing else. Do not use any tools.";
/// The steer message sent once the first turn is idle.
pub const STEER_PROMPT: &str = "Reply with exactly the single word STEERED and nothing else. Do not use any tools.";

/// One observation of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub state: SessionState,
    /// The argv the engine launched (or relaunched) — how a resume is proven.
    pub argv: Vec<String>,
    pub agent_session_id: Option<String>,
    /// `inferred` | `hooks` | `native`.
    pub state_source: String,
    pub exit_code: Option<i32>,
}

/// What the machine drives. `wait` is the ONLY place time passes (the real
/// driver sleeps and ticks the supervisor; the fake advances a script).
pub trait FlowDriver {
    /// Launch a session of `kind` with `prompt`; returns the session id.
    ///
    /// # Errors
    /// When the engine refuses the launch (no manifest, no binary, tmux down).
    fn launch(&mut self, kind: &str, prompt: &str) -> Result<String>;
    /// # Errors
    /// When the session is unknown.
    fn probe(&mut self, id: &str) -> Result<Probe>;
    /// The visible pane, plain text.
    ///
    /// # Errors
    /// When the pane is gone.
    fn snapshot(&mut self, id: &str) -> Result<String>;
    /// Steer: paste `text` + submit.
    ///
    /// # Errors
    /// When the pane is gone.
    fn send(&mut self, id: &str, text: &str) -> Result<()>;
    /// Kill; with `resume`, relaunch per the manifest's resume rule.
    ///
    /// # Errors
    /// When the relaunch fails.
    fn kill(&mut self, id: &str, resume: bool) -> Result<Probe>;
    /// Best-effort teardown of the session.
    fn cleanup(&mut self, id: &str);
    /// Let `d` pass (and let the supervisor observe the pane).
    fn wait(&mut self, d: Duration);
    /// Monotonic time for the budget clock.
    fn now(&self) -> Instant;
}

/// Time budget for one validation run.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// From launch (or relaunch) to the first `working`/`idle`.
    pub boot: Duration,
    /// From `working` to `idle` for a turn.
    pub turn: Duration,
    /// Between probes.
    pub poll: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            boot: Duration::from_secs(60),
            turn: Duration::from_secs(120),
            poll: Duration::from_secs(1),
        }
    }
}

/// The steps a run tries to prove, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// The engine launched a session (binary resolved, tmux pane up).
    Launch,
    /// The session was observed `working` after launch.
    Working,
    /// The first turn reached `idle`.
    Idle,
    /// A steer was acknowledged (state went `working`, or the pane changed).
    Steer,
    /// The steered turn reached `idle`.
    SteerIdle,
    /// Kill + resume relaunched and the session came back (`working`/`idle`).
    Resume,
    /// The relaunch argv carried the harness session id (a true resume, not a
    /// fresh session).
    ResumeUsedSessionId,
}

impl Default for Probe {
    fn default() -> Self {
        Self {
            state: SessionState::Starting,
            argv: Vec::new(),
            agent_session_id: None,
            state_source: "inferred".to_string(),
            exit_code: None,
        }
    }
}

impl Step {
    /// All steps, in the order they run.
    pub const ALL: &'static [Self] = &[
        Self::Launch,
        Self::Working,
        Self::Idle,
        Self::Steer,
        Self::SteerIdle,
        Self::Resume,
        Self::ResumeUsedSessionId,
    ];
}

/// What a step ended as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "detail")]
pub enum Outcome {
    Proven,
    /// Could not be shown either way (e.g. no session id was ever learned).
    Unproven(String),
    /// Shown NOT to work.
    Failed(String),
    /// Not attempted because an earlier step failed.
    Skipped,
}

/// One proof line of the verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    pub step: Step,
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// The result of one validation run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Verdict {
    pub session_id: Option<String>,
    pub proofs: Vec<Proof>,
    /// The last visible pane (the LLM's evidence for the next draft).
    pub pane_tail: String,
    /// `inferred` | `hooks` | `native` as the engine reported it.
    pub state_source: String,
    /// Launch + first idle proven — the manifest can run a session.
    pub usable: bool,
    /// Every step proven.
    pub complete: bool,
    /// Wall-clock seconds the run took.
    pub elapsed_secs: u64,
}

impl Verdict {
    fn set(&mut self, step: Step, outcome: Outcome) {
        if let Some(p) = self.proofs.iter_mut().find(|p| p.step == step) {
            p.outcome = outcome;
        } else {
            self.proofs.push(Proof { step, outcome });
        }
    }

    fn outcome(&self, step: Step) -> Option<&Outcome> {
        self.proofs.iter().find(|p| p.step == step).map(|p| &p.outcome)
    }

    /// Steps that were proven.
    #[must_use]
    pub fn proven(&self) -> Vec<Step> {
        self.proofs.iter().filter(|p| p.outcome == Outcome::Proven).map(|p| p.step).collect()
    }

    /// Steps that were not proven (unproven, failed or skipped), with why.
    #[must_use]
    pub fn not_proven(&self) -> Vec<(Step, String)> {
        self.proofs
            .iter()
            .filter_map(|p| match &p.outcome {
                Outcome::Proven => None,
                Outcome::Unproven(d) | Outcome::Failed(d) => Some((p.step, d.clone())),
                Outcome::Skipped => Some((p.step, "not attempted".to_string())),
            })
            .collect()
    }

    /// Failures only — what the next draft must fix.
    #[must_use]
    pub fn failures(&self) -> Vec<(Step, String)> {
        self.proofs
            .iter()
            .filter_map(|p| match &p.outcome {
                Outcome::Failed(d) => Some((p.step, d.clone())),
                _ => None,
            })
            .collect()
    }

    /// One line per step, for a report.
    #[must_use]
    pub fn summary_lines(&self) -> Vec<String> {
        self.proofs
            .iter()
            .map(|p| {
                let (glyph, detail) = match &p.outcome {
                    Outcome::Proven => ("●", String::new()),
                    Outcome::Unproven(d) => ("◐", format!(" — unproven: {d}")),
                    Outcome::Failed(d) => ("○", format!(" — FAILED: {d}")),
                    Outcome::Skipped => ("○", " — skipped".to_string()),
                };
                format!("{glyph} {}{detail}", step_label(p.step))
            })
            .collect()
    }
}

/// Human label for a step.
#[must_use]
pub const fn step_label(step: Step) -> &'static str {
    match step {
        Step::Launch => "launch (binary resolved, pane up)",
        Step::Working => "working observed after launch",
        Step::Idle => "first turn reached idle",
        Step::Steer => "steer acknowledged",
        Step::SteerIdle => "steered turn reached idle",
        Step::Resume => "kill + resume came back",
        Step::ResumeUsedSessionId => "resume reused the harness session id",
    }
}

/// The last 12 non-blank lines of a pane.
fn tail(pane: &str) -> String {
    let lines: Vec<&str> = pane.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(12);
    lines[start..].join("\n")
}

/// Poll until the session is in one of `want`, a terminal/blocked state, or
/// `budget` runs out. Returns the last probe and whether `working` was seen.
fn wait_for(driver: &mut dyn FlowDriver, id: &str, want: &[SessionState], budget: Duration, poll: Duration) -> Result<(Probe, bool)> {
    let start = driver.now();
    let mut saw_working = false;
    loop {
        let p = driver.probe(id)?;
        if p.state == SessionState::Working {
            saw_working = true;
        }
        if want.contains(&p.state)
            || matches!(
                p.state,
                SessionState::NeedsYou | SessionState::Limited | SessionState::Done | SessionState::Dead
            )
        {
            return Ok((p, saw_working));
        }
        if driver.now().duration_since(start) >= budget {
            return Ok((p, saw_working));
        }
        driver.wait(poll);
    }
}

fn blocked_reason(p: &Probe, pane: &str) -> Option<String> {
    match p.state {
        SessionState::NeedsYou => Some(format!(
            "the harness is waiting on an approval/auth/trust prompt the engine cannot answer for you:\n{}",
            tail(pane)
        )),
        SessionState::Limited => Some(format!("the harness reported a usage limit:\n{}", tail(pane))),
        SessionState::Done => Some(format!("the process exited (code {:?}) before the turn finished:\n{}", p.exit_code, tail(pane))),
        SessionState::Dead => Some(format!("the process died (exit {:?}) and could not be resumed:\n{}", p.exit_code, tail(pane))),
        _ => None,
    }
}

/// Run the state machine for harness `kind` over `driver`.
#[allow(clippy::too_many_lines, reason = "the steps read best as one linear machine")]
pub fn validate(driver: &mut dyn FlowDriver, kind: &str, budget: &Budget) -> Verdict {
    let started = driver.now();
    let mut v = Verdict::default();
    for s in Step::ALL {
        v.set(*s, Outcome::Skipped);
    }
    let finish = |mut v: Verdict, driver: &mut dyn FlowDriver| {
        v.elapsed_secs = driver.now().duration_since(started).as_secs();
        v.usable = v.outcome(Step::Launch) == Some(&Outcome::Proven) && v.outcome(Step::Idle) == Some(&Outcome::Proven);
        v.complete = v.proofs.iter().all(|p| p.outcome == Outcome::Proven);
        v
    };

    // 1. Launch.
    let id = match driver.launch(kind, LAUNCH_PROMPT) {
        Ok(id) => id,
        Err(e) => {
            v.set(Step::Launch, Outcome::Failed(format!("{e:#}")));
            return finish(v, driver);
        }
    };
    v.session_id = Some(id.clone());
    v.set(Step::Launch, Outcome::Proven);

    // 2. Boot → working/idle.
    let (p, saw_working) = match wait_for(driver, &id, &[SessionState::Working, SessionState::Idle], budget.boot, budget.poll) {
        Ok(x) => x,
        Err(e) => {
            v.set(Step::Working, Outcome::Failed(format!("{e:#}")));
            driver.cleanup(&id);
            return finish(v, driver);
        }
    };
    let pane = driver.snapshot(&id).unwrap_or_default();
    v.pane_tail = tail(&pane);
    v.state_source.clone_from(&p.state_source);
    if let Some(why) = blocked_reason(&p, &pane) {
        v.set(Step::Working, if saw_working { Outcome::Proven } else { Outcome::Failed(why.clone()) });
        v.set(Step::Idle, Outcome::Failed(why));
        driver.cleanup(&id);
        return finish(v, driver);
    }
    // 3. First turn → idle.
    let (p, saw_working2) = if p.state == SessionState::Idle {
        (p, saw_working)
    } else {
        match wait_for(driver, &id, &[SessionState::Idle], budget.turn, budget.poll) {
            Ok((p2, w)) => (p2, saw_working || w),
            Err(e) => {
                v.set(Step::Idle, Outcome::Failed(format!("{e:#}")));
                driver.cleanup(&id);
                return finish(v, driver);
            }
        }
    };
    let pane = driver.snapshot(&id).unwrap_or_default();
    v.pane_tail = tail(&pane);
    v.state_source.clone_from(&p.state_source);
    v.set(
        Step::Working,
        if saw_working2 {
            Outcome::Proven
        } else {
            Outcome::Unproven("never observed `working` — the turn finished within one poll, or the working pattern does not match this harness".to_string())
        },
    );
    if p.state != SessionState::Idle {
        let why = blocked_reason(&p, &pane).unwrap_or_else(|| {
            format!(
                "never reached idle within {}s (state stayed `{}`): the idle pattern probably does not match this harness's prompt, or the prompt never reached it\n{}",
                budget.boot.as_secs() + budget.turn.as_secs(),
                p.state,
                tail(&pane)
            )
        });
        v.set(Step::Idle, Outcome::Failed(why));
        driver.cleanup(&id);
        return finish(v, driver);
    }
    v.set(Step::Idle, Outcome::Proven);

    // 4. Steer.
    let before = pane;
    match driver.send(&id, STEER_PROMPT) {
        Ok(()) => {
            let (p, w) = wait_for(driver, &id, &[SessionState::Working], budget.boot, budget.poll).unwrap_or_else(|_| (Probe::default(), false));
            let after = driver.snapshot(&id).unwrap_or_default();
            let changed = after != before;
            if w || p.state == SessionState::Working {
                v.set(Step::Steer, Outcome::Proven);
            } else if changed {
                v.set(
                    Step::Steer,
                    Outcome::Unproven("the pane changed after the steer but `working` was never observed".to_string()),
                );
            } else {
                v.set(
                    Step::Steer,
                    Outcome::Failed(format!(
                        "nothing happened after pasting a message + Enter (steer method / submit key?):\n{}",
                        tail(&after)
                    )),
                );
            }
            if !matches!(v.outcome(Step::Steer), Some(Outcome::Failed(_))) {
                let (p2, _) = wait_for(driver, &id, &[SessionState::Idle], budget.turn, budget.poll).unwrap_or((p, false));
                let pane = driver.snapshot(&id).unwrap_or_default();
                v.pane_tail = tail(&pane);
                if p2.state == SessionState::Idle {
                    v.set(Step::SteerIdle, Outcome::Proven);
                } else {
                    v.set(
                        Step::SteerIdle,
                        blocked_reason(&p2, &pane).map_or_else(
                            || Outcome::Failed(format!("the steered turn never reached idle (state `{}`)\n{}", p2.state, tail(&pane))),
                            Outcome::Failed,
                        ),
                    );
                }
            }
        }
        Err(e) => v.set(Step::Steer, Outcome::Failed(format!("{e:#}"))),
    }

    // 5. Kill + resume.
    let agent_id = driver.probe(&id).ok().and_then(|p| p.agent_session_id);
    match driver.kill(&id, true) {
        Ok(relaunched) => {
            let (p, _) =
                wait_for(driver, &id, &[SessionState::Working, SessionState::Idle], budget.boot, budget.poll).unwrap_or_else(|_| (relaunched.clone(), false));
            let pane = driver.snapshot(&id).unwrap_or_default();
            v.pane_tail = tail(&pane);
            if matches!(p.state, SessionState::Working | SessionState::Idle) {
                v.set(Step::Resume, Outcome::Proven);
            } else {
                v.set(
                    Step::Resume,
                    Outcome::Failed(blocked_reason(&p, &pane).unwrap_or_else(|| format!("the relaunch never came up (state `{}`)\n{}", p.state, tail(&pane)))),
                );
            }
            match &agent_id {
                Some(sid) if relaunched.argv.iter().any(|a| a.contains(sid.as_str())) => v.set(Step::ResumeUsedSessionId, Outcome::Proven),
                Some(sid) => v.set(
                    Step::ResumeUsedSessionId,
                    Outcome::Unproven(format!(
                        "the relaunch argv did not carry the session id {sid} (resume.mode = relaunch_command, or resume.argv lacks {{session_id}})"
                    )),
                ),
                None => v.set(
                    Step::ResumeUsedSessionId,
                    Outcome::Unproven(
                        "no harness session id was learned (state.source is not hooks, or no hook landed) — the resume relaunched the original command"
                            .to_string(),
                    ),
                ),
            }
        }
        Err(e) => v.set(Step::Resume, Outcome::Failed(format!("{e:#}"))),
    }
    driver.cleanup(&id);
    finish(v, driver)
}

// ── the real driver ──────────────────────────────────────────────────────────

/// A private engine: its own db, tmux socket, scratch `$HOME` (holding only
/// the draft manifest) and a scratch git repo as the worktree.
pub struct EngineDriver {
    engine: Engine,
    socket: String,
    workspace: PathBuf,
    model: Option<String>,
    _scratch: tempfile::TempDir,
}

impl EngineDriver {
    /// Build one for `manifest_toml` (named `name`). `model` is passed through
    /// to `{model}`; `None` lets the harness pick its default.
    ///
    /// # Errors
    /// When the scratch dirs, the manifest file or the engine cannot be set up,
    /// or tmux is not installed.
    pub fn private(name: &str, manifest_toml: &str, model: Option<String>) -> Result<Self> {
        if !tmux::tmux_available() {
            anyhow::bail!("tmux is not installed — SmoothFlow needs it to run a session");
        }
        let scratch = tempfile::Builder::new().prefix("smooth-harness-validate-").tempdir().context("scratch dir")?;
        let home = scratch.path().join("home");
        let manifests = home.join(".smooth").join("harnesses");
        std::fs::create_dir_all(&manifests)?;
        std::fs::write(manifests.join(format!("{name}.toml")), manifest_toml)?;
        let workspace = scratch.path().join("ws");
        std::fs::create_dir_all(&workspace)?;
        std::fs::write(workspace.join("README.md"), "# harness validation scratch\n")?;
        // Some harnesses insist on a git repo; a throwaway one costs nothing.
        let _ = std::process::Command::new("git").args(["init", "-q"]).current_dir(&workspace).output();
        let _ = std::process::Command::new("git")
            .args(["-c", "user.email=validate@smoo.ai", "-c", "user.name=validate", "add", "-A"])
            .current_dir(&workspace)
            .output();
        let _ = std::process::Command::new("git")
            .args(["-c", "user.email=validate@smoo.ai", "-c", "user.name=validate", "commit", "-q", "-m", "scratch"])
            .current_dir(&workspace)
            .output();
        let socket = format!(
            "smooth-flow-validate-{}-{}",
            std::process::id(),
            &uuid::Uuid::new_v4().simple().to_string()[..6]
        );
        let engine = Engine::open(EngineConfig {
            db_path: scratch.path().join("flow.db"),
            version: "validate".into(),
            machine_label: "harness-validate".into(),
            home,
            daemon_url: None,
            ..EngineConfig::new(workspace.clone())
        })?;
        Ok(Self {
            engine,
            socket,
            workspace,
            model,
            _scratch: scratch,
        })
    }

    /// The scratch worktree the harness runs in.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
}

impl Drop for EngineDriver {
    fn drop(&mut self) {
        tmux::kill_server(&self.socket);
    }
}

fn probe_of(s: &crate::store::Session) -> Probe {
    Probe {
        state: s.state,
        argv: s.argv.clone(),
        agent_session_id: s.agent_session_id.clone(),
        state_source: s.state_source.clone(),
        exit_code: s.exit_code,
    }
}

impl FlowDriver for EngineDriver {
    fn launch(&mut self, kind: &str, prompt: &str) -> Result<String> {
        let kind: crate::store::SessionKind = kind.parse()?;
        let s = self.engine.new_session(NewRequest {
            kind,
            worktree: Some(self.workspace.to_string_lossy().into_owned()),
            prompt: Some(prompt.to_string()),
            model: self.model.clone(),
            tmux_socket: Some(self.socket.clone()),
            title: Some("harness validation".into()),
            ..NewRequest::default()
        })?;
        Ok(s.id)
    }

    fn probe(&mut self, id: &str) -> Result<Probe> {
        self.engine
            .get(id)?
            .map(|s| probe_of(&s))
            .ok_or_else(|| anyhow::anyhow!("no such session {id}"))
    }

    fn snapshot(&mut self, id: &str) -> Result<String> {
        match self.engine.snapshot(id)? {
            crate::protocol::ServerFrame::Screen { text, .. } => Ok(text),
            _ => Ok(String::new()),
        }
    }

    fn send(&mut self, id: &str, text: &str) -> Result<()> {
        self.engine.send(id, text)
    }

    fn kill(&mut self, id: &str, resume: bool) -> Result<Probe> {
        self.engine.kill(id, resume).map(|s| probe_of(&s))
    }

    fn cleanup(&mut self, id: &str) {
        if let Ok(Some(s)) = self.engine.get(id) {
            if !s.state.is_terminal() {
                let _ = self.engine.kill(id, false);
            }
        }
        let _ = self.engine.remove(id);
    }

    fn wait(&mut self, d: Duration) {
        std::thread::sleep(d);
        if let Err(e) = self.engine.supervise_tick() {
            tracing::warn!(error = %e, "harness validation: supervise tick");
        }
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A scripted session: each `wait` pops the next state; `send` and
    /// `kill(resume)` push their own scripted follow-ups.
    struct Fake {
        script: VecDeque<SessionState>,
        after_send: VecDeque<SessionState>,
        after_resume: VecDeque<SessionState>,
        state: SessionState,
        pane: String,
        clock: Instant,
        agent_id: Option<String>,
        relaunch_argv: Vec<String>,
        launch_err: Option<String>,
        send_err: Option<String>,
        cleaned: bool,
        sends: Vec<String>,
        pane_changes_on_send: bool,
    }

    impl Fake {
        fn new(script: &[SessionState]) -> Self {
            Self {
                script: script.iter().copied().collect(),
                after_send: VecDeque::new(),
                after_resume: VecDeque::new(),
                state: SessionState::Starting,
                pane: "banner\n> ".into(),
                clock: Instant::now(),
                agent_id: None,
                relaunch_argv: vec!["tool".into()],
                launch_err: None,
                send_err: None,
                cleaned: false,
                sends: Vec::new(),
                pane_changes_on_send: true,
            }
        }
    }

    impl FlowDriver for Fake {
        fn launch(&mut self, _kind: &str, _prompt: &str) -> Result<String> {
            if let Some(e) = &self.launch_err {
                anyhow::bail!("{e}");
            }
            Ok("fs-1".into())
        }
        fn probe(&mut self, _id: &str) -> Result<Probe> {
            Ok(Probe {
                state: self.state,
                argv: self.relaunch_argv.clone(),
                agent_session_id: self.agent_id.clone(),
                state_source: "inferred".into(),
                exit_code: None,
            })
        }
        fn snapshot(&mut self, _id: &str) -> Result<String> {
            Ok(self.pane.clone())
        }
        fn send(&mut self, _id: &str, text: &str) -> Result<()> {
            if let Some(e) = &self.send_err {
                anyhow::bail!("{e}");
            }
            self.sends.push(text.to_string());
            if self.pane_changes_on_send {
                self.pane.push_str("\nsteered");
            }
            self.script = std::mem::take(&mut self.after_send);
            Ok(())
        }
        fn kill(&mut self, _id: &str, resume: bool) -> Result<Probe> {
            self.state = if resume { SessionState::Starting } else { SessionState::Done };
            self.script = std::mem::take(&mut self.after_resume);
            self.probe("fs-1")
        }
        fn cleanup(&mut self, _id: &str) {
            self.cleaned = true;
        }
        fn wait(&mut self, d: Duration) {
            self.clock += d;
            if let Some(s) = self.script.pop_front() {
                self.state = s;
            }
        }
        fn now(&self) -> Instant {
            self.clock
        }
    }

    fn budget() -> Budget {
        Budget {
            boot: Duration::from_secs(5),
            turn: Duration::from_secs(5),
            poll: Duration::from_secs(1),
        }
    }

    use SessionState::{Idle, NeedsYou, Starting, Working};

    #[test]
    fn happy_path_proves_everything_with_hooks_session_id() {
        let mut f = Fake::new(&[Starting, Working, Idle]);
        f.after_send = [Working, Idle].into();
        f.after_resume = [Starting, Idle].into();
        f.agent_id = Some("sid-123".into());
        f.relaunch_argv = vec!["tool".into(), "--resume".into(), "sid-123".into()];
        let v = validate(&mut f, "tool", &budget());
        assert!(v.complete, "{v:#?}");
        assert!(v.usable);
        assert_eq!(v.proven().len(), Step::ALL.len());
        assert_eq!(v.session_id.as_deref(), Some("fs-1"));
        assert_eq!(f.sends, vec![STEER_PROMPT]);
        assert!(f.cleaned, "the session is torn down");
        assert!(v.summary_lines().iter().all(|l| l.starts_with('●')), "{:?}", v.summary_lines());
    }

    #[test]
    fn launch_failure_skips_the_rest() {
        let mut f = Fake::new(&[]);
        f.launch_err = Some("no manifest".into());
        let v = validate(&mut f, "tool", &budget());
        assert!(!v.usable && !v.complete);
        assert!(matches!(v.outcome(Step::Launch), Some(Outcome::Failed(d)) if d.contains("no manifest")));
        assert_eq!(v.failures().len(), 1);
        assert_eq!(v.not_proven().len(), Step::ALL.len());
        assert!(v.session_id.is_none());
    }

    #[test]
    fn never_idle_fails_idle_with_the_pane_tail_and_is_unusable() {
        // Working forever: boot sees working, the turn budget runs out.
        let mut f = Fake::new(&[
            Working, Working, Working, Working, Working, Working, Working, Working, Working, Working, Working, Working,
        ]);
        f.pane = "spinner\nesc to interrupt".into();
        let v = validate(&mut f, "tool", &budget());
        assert!(!v.usable);
        assert_eq!(v.outcome(Step::Working), Some(&Outcome::Proven));
        assert!(
            matches!(v.outcome(Step::Idle), Some(Outcome::Failed(d)) if d.contains("idle pattern") && d.contains("esc to interrupt")),
            "{v:#?}"
        );
        assert_eq!(v.outcome(Step::Steer), Some(&Outcome::Skipped));
        assert!(f.cleaned);
        assert_eq!(v.pane_tail, "spinner\nesc to interrupt");
    }

    #[test]
    fn approval_prompt_blocks_with_a_clear_reason() {
        let mut f = Fake::new(&[Starting, NeedsYou]);
        f.pane = "Trust this folder? (y/n)".into();
        let v = validate(&mut f, "tool", &budget());
        assert!(matches!(v.outcome(Step::Idle), Some(Outcome::Failed(d)) if d.contains("approval/auth/trust") && d.contains("(y/n)")));
        assert!(matches!(v.outcome(Step::Working), Some(Outcome::Failed(_))));
        assert!(!v.usable);
    }

    #[test]
    fn idle_without_working_is_usable_but_working_is_unproven() {
        let mut f = Fake::new(&[Idle]);
        f.after_send = [Idle].into();
        f.after_resume = [Idle].into();
        f.pane_changes_on_send = true;
        let v = validate(&mut f, "tool", &budget());
        assert!(v.usable, "{v:#?}");
        assert!(!v.complete);
        assert!(matches!(v.outcome(Step::Working), Some(Outcome::Unproven(_))));
        // Steer: pane changed but no working → unproven, and the steered idle still proves.
        assert!(matches!(v.outcome(Step::Steer), Some(Outcome::Unproven(d)) if d.contains("pane changed")));
        assert_eq!(v.outcome(Step::SteerIdle), Some(&Outcome::Proven));
        assert_eq!(v.outcome(Step::Resume), Some(&Outcome::Proven));
        assert!(matches!(v.outcome(Step::ResumeUsedSessionId), Some(Outcome::Unproven(d)) if d.contains("no harness session id")));
    }

    #[test]
    fn steer_that_changes_nothing_fails_and_skips_steer_idle() {
        let mut f = Fake::new(&[Working, Idle]);
        f.after_send = [Idle, Idle, Idle, Idle, Idle, Idle].into();
        f.after_resume = [Idle].into();
        f.pane_changes_on_send = false;
        let v = validate(&mut f, "tool", &budget());
        assert!(
            matches!(v.outcome(Step::Steer), Some(Outcome::Failed(d)) if d.contains("nothing happened")),
            "{v:#?}"
        );
        assert_eq!(v.outcome(Step::SteerIdle), Some(&Outcome::Skipped));
        // Resume is still attempted — it is independent of steering.
        assert_eq!(v.outcome(Step::Resume), Some(&Outcome::Proven));
        assert!(v.usable);
    }

    #[test]
    fn resume_that_never_comes_back_fails_resume() {
        let mut f = Fake::new(&[Working, Idle]);
        f.after_send = [Working, Idle].into();
        f.after_resume = [Starting, Starting, Starting, Starting, Starting, Starting, Starting].into();
        f.agent_id = Some("sid-9".into());
        let v = validate(&mut f, "tool", &budget());
        assert!(
            matches!(v.outcome(Step::Resume), Some(Outcome::Failed(d)) if d.contains("never came up")),
            "{v:#?}"
        );
        assert!(matches!(v.outcome(Step::ResumeUsedSessionId), Some(Outcome::Unproven(d)) if d.contains("did not carry")));
        assert!(v.usable);
        assert!(!v.complete);
    }

    #[test]
    fn send_error_is_a_steer_failure_not_a_panic() {
        let mut f = Fake::new(&[Working, Idle]);
        f.send_err = Some("pane gone".into());
        f.after_resume = [Idle].into();
        let v = validate(&mut f, "tool", &budget());
        assert!(matches!(v.outcome(Step::Steer), Some(Outcome::Failed(d)) if d.contains("pane gone")));
        assert_eq!(v.outcome(Step::SteerIdle), Some(&Outcome::Skipped));
    }

    #[test]
    fn budget_clock_bounds_the_boot_wait() {
        // Starting forever: the boot budget (5s at 1s polls) ends the wait.
        let mut f = Fake::new(&[Starting; 50]);
        let start = f.clock;
        let v = validate(&mut f, "tool", &budget());
        assert!(
            f.clock.duration_since(start) <= Duration::from_secs(12),
            "the fake clock advanced only within the budgets"
        );
        assert!(matches!(v.outcome(Step::Idle), Some(Outcome::Failed(_))));
        assert!(v.elapsed_secs >= 5);
    }

    #[test]
    fn verdict_serializes_with_flat_outcomes() {
        let mut v = Verdict::default();
        v.set(Step::Launch, Outcome::Proven);
        v.set(Step::Idle, Outcome::Failed("x".into()));
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["proofs"][0]["step"], "launch");
        assert_eq!(j["proofs"][0]["outcome"], "proven");
        assert_eq!(j["proofs"][1]["outcome"], "failed");
        assert_eq!(j["proofs"][1]["detail"], "x");
        let back: Verdict = serde_json::from_value(j).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn step_labels_are_distinct() {
        let labels: std::collections::HashSet<&str> = Step::ALL.iter().map(|s| step_label(*s)).collect();
        assert_eq!(labels.len(), Step::ALL.len());
    }

    /// The real driver against the daemon's `fake-claude` fixture — needs
    /// tmux + curl; the fixture only posts hooks when a daemon address exists,
    /// so here it runs scrape-only (no `.flow-e2e-addr`), which is exactly the
    /// path a freshly drafted manifest takes. Its turn is instantaneous, so
    /// there is no working-only phrase to scrape: idle is the bare `>` prompt
    /// (tmux trims the trailing space; `(?m)` anchors per line).
    #[test]
    #[ignore = "live: launches tmux + the fake-claude fixture; run with --ignored"]
    fn engine_driver_runs_fake_claude_scrape_only() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../smooth-daemon/tests/fixtures/fake-claude");
        let fixture = fixture.canonicalize().unwrap();
        let toml = format!(
            "name = \"fakeclaude\"\n[binary]\nnames = [\"{}\"]\n[launch]\nargv = [\"--session-id\", \"{{session_id}}\", \"{{prompt}}\"]\nsession_id = \"preassigned\"\n[resume]\nargv = [\"--resume\", \"{{session_id}}\"]\nmode = \"resume_session\"\n[state]\nsource = \"scrape\"\n[state.scrape]\nidle = [\"(?m)^>\\\\s*$\"]\n",
            fixture.display()
        );
        let mut d = EngineDriver::private("fakeclaude", &toml, None).unwrap();
        let v = validate(
            &mut d,
            "fakeclaude",
            &Budget {
                boot: Duration::from_secs(15),
                turn: Duration::from_secs(15),
                poll: Duration::from_millis(500),
            },
        );
        eprintln!("{}", v.summary_lines().join("\n"));
        assert!(v.usable, "{v:#?}");
        assert_eq!(v.outcome(Step::Resume), Some(&Outcome::Proven));
        assert_eq!(v.outcome(Step::ResumeUsedSessionId), Some(&Outcome::Proven));
    }
}
