//! Agent sessions end to end: the state machine (working / idle /
//! needs_you / limited / done / dead), steering, the permission long-poll,
//! resume-on-death with `--resume`, the give-up after three resumes, the
//! learned-id binding, the duplicate-resume guard, and the native + scrape
//! state sources.

use std::time::Duration;

use serde_json::json;

use crate::support::{local_clock_in, prereqs, state, Daemon, TICK, WAIT};

/// Rule 2's first backoff (`RESUME_BACKOFF_BASE · 2^0`) plus a tick.
const FIRST_RESUME: Duration = Duration::from_secs(5 + 2 + 2);

#[tokio::test]
async fn agent_transitions_working_idle_needs_you_done() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    let mut ws = d.ws().await;

    let s = d.new_session("fake-agent", Some("/work first")).await;
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["kind"], "fake-agent");
    assert_eq!(s["state"], "starting");
    assert_eq!(s["state_source"], "inferred", "nothing has reported yet");
    let agent_id = s["agent_session_id"].as_str().unwrap().to_string();
    assert_eq!(agent_id.len(), 36, "a pre-assigned uuid: {s}");
    let argv = s["argv"].as_array().unwrap();
    assert!(
        argv[0].as_str().unwrap().ends_with("/.local/bin/fake-agent"),
        "resolved through prefer_paths: {argv:?}"
    );
    assert_eq!(
        argv[1..],
        [json!("--session-id"), json!(agent_id), json!("/work first")],
        "no --model when none was given"
    );
    assert_eq!(s["title"], "/work first");

    // The prompt runs a turn: hooks drive working → idle, unread, `hooks`.
    // (SessionStart already made it idle-and-read; the turn's Stop is the
    // unread idle.)
    let idle = d
        .wait_until(&id, "unread idle via hooks", WAIT, |s| {
            state(s) == "idle" && s["state_source"] == "hooks" && s["unread"] == true
        })
        .await;
    assert!(idle["attention"].is_null());
    let screen = d.wait_screen(&id, "worked: first", WAIT).await;
    assert!(screen.contains(&format!("fake-agent ready sid={agent_id} resume=0 mode=hooks")), "{screen}");
    let log = d.agent_log();
    assert!(
        log.contains("hook UserPromptSubmit → {}") && log.contains("hook Stop → {}"),
        "every hook got a 200 body:\n{log}"
    );

    // The WS saw the whole story as flow.event lines.
    let mut kinds = Vec::new();
    let deadline = std::time::Instant::now() + WAIT;
    while std::time::Instant::now() < deadline && !kinds.iter().any(|(k, t): &(String, String)| k == "agent" && t == "done: first") {
        let Some(f) = ws.next(Duration::from_secs(5)).await else { break };
        if f["type"] == "flow.event" && f["id"] == id {
            kinds.push((f["kind"].as_str().unwrap().to_string(), f["text"].as_str().unwrap().to_string()));
        }
    }
    assert!(kinds.contains(&("user".into(), "first".into())), "{kinds:?}");
    assert!(kinds.iter().any(|(k, t)| k == "tool" && t.contains("Bash(echo hi)")), "{kinds:?}");
    assert!(kinds.contains(&("system".into(), "working".into())), "{kinds:?}");
    assert!(kinds.contains(&("agent".into(), "done: first".into())), "{kinds:?}");

    // mark_read clears the flag.
    ws.send(json!({"type":"flow.mark_read","id":id})).await;
    d.wait_until(&id, "read", WAIT, |s| s["unread"] == false).await;

    // Steer: /perm → a hook-reported permission with a request_id; the hook
    // POST is held open until flow.approve answers it.
    d.send(&id, "/perm").await;
    let ask = d.wait_state(&id, "needs_you", WAIT).await;
    assert_eq!(ask["attention"]["reason"], "permission");
    assert_eq!(ask["attention"]["detail"], "Bash: git push");
    let request_id = ask["attention"]["request_id"].as_str().unwrap().to_string();
    assert!(!request_id.starts_with("scrape-"), "reported by the hook, not scraped: {ask}");
    let att = ws
        .wait_for("flow.attention", WAIT, |v| {
            v["type"] == "flow.attention" && v["id"] == id && v["attention"]["reason"] == "permission"
        })
        .await;
    assert_eq!(att["attention"]["request_id"], request_id);
    // The daemon's inbox view (needs_you) is what `th flow inbox` filters on.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !d.agent_log().contains("hook PermissionRequest →"),
        "the long-poll is still open:\n{}",
        d.agent_log()
    );

    ws.send(json!({"type":"flow.approve","id":id,"request_id":request_id,"decision":"allow_session"}))
        .await;
    d.wait_state(&id, "working", WAIT).await;
    let screen = d.wait_screen(&id, "decision:", WAIT).await;
    assert!(screen.contains(r#""behavior":"allow""#), "the agent printed the long-polled reply: {screen}");
    assert!(screen.contains(r#""destination":"session""#), "allow_session adds a session rule: {screen}");
    // Approving is a user event; the state line follows.
    ws.wait_for("approve event", WAIT, |v| {
        v["type"] == "flow.event" && v["id"] == id && v["text"] == "approve: allow_session"
    })
    .await;

    // Another turn, then a clean exit → done with the code.
    d.send(&id, "/work second").await;
    d.wait_until(&id, "idle again", WAIT, |s| state(s) == "idle" && s["unread"] == true).await;
    d.wait_screen(&id, "worked: second", WAIT).await;
    d.send(&id, "/exit 0").await;
    let done = d.wait_state(&id, "done", WAIT + TICK).await;
    assert_eq!(done["exit_code"], 0, "{done}");
    assert!(done["attention"].is_null());
    let log = d.agent_log();
    assert!(log.contains("hook SessionEnd → {}"), "{log}");
    // A done agent is not resumed by the supervisor.
    tokio::time::sleep(TICK * 2).await;
    assert_eq!(state(&d.session(&id).await), "done");
}

#[tokio::test]
async fn agent_question_notification_needs_you_and_steer_answers_it() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    let s = d.new_session("fake-agent", Some("/ask which branch?")).await;
    let id = s["id"].as_str().unwrap().to_string();
    let q = d.wait_state(&id, "needs_you", WAIT).await;
    assert_eq!(q["attention"]["reason"], "question");
    assert_eq!(q["attention"]["detail"], "which branch?");
    assert!(q["attention"]["request_id"].is_null(), "a question has nothing to long-poll: {q}");
    // Steering text is the answer; the next turn is working → idle.
    d.send(&id, "/work main").await;
    d.wait_until(&id, "idle", WAIT, |s| state(s) == "idle" && s["state_source"] == "hooks").await;
    d.wait_screen(&id, "worked: main", WAIT).await;
    d.kill(&id, false).await;
    d.wait_state(&id, "done", WAIT).await;
}

#[tokio::test]
async fn agent_usage_limit_is_scheduled_from_the_banner() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    let s = d.new_session("fake-agent", Some("/work warm; /limit 11:59pm")).await;
    let id = s["id"].as_str().unwrap().to_string();
    let limited = d.wait_state(&id, "limited", WAIT).await;
    assert_eq!(limited["attention"]["reason"], "usage_limit");
    assert_eq!(
        limited["state_source"], "hooks",
        "limits are scraped even when hooks own working/idle: {limited}"
    );
    let at = chrono::DateTime::parse_from_rfc3339(limited["attention"]["resume_at"].as_str().unwrap()).unwrap();
    let wait = at.signed_duration_since(chrono::Utc::now());
    assert!(
        wait > chrono::Duration::seconds(30) && wait <= chrono::Duration::hours(24),
        "resumes at the next 11:59pm: {limited}"
    );
    assert!(limited["attention"]["detail"].as_str().unwrap().starts_with("resumes at "), "{limited}");
    // The window is in the future — nothing fires, the state holds.
    tokio::time::sleep(TICK * 2).await;
    assert_eq!(state(&d.session(&id).await), "limited");
    let killed = d.kill(&id, false).await;
    assert_eq!(state(&killed), "done");
}

/// Slow (~90 s): the banner names a time ~1 min out; the supervisor presses
/// Enter when it passes and the session is working again.
#[tokio::test]
async fn agent_usage_limit_resume_fires_when_the_window_passes() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    // Minute resolution + "at least one minute out": ~70–130 s from now.
    let at = local_clock_in(75);
    let s = d.new_session("fake-agent", Some(&format!("/work warm; /limit {at}"))).await;
    let id = s["id"].as_str().unwrap().to_string();
    let limited = d.wait_state(&id, "limited", WAIT).await;
    let resume_at = chrono::DateTime::parse_from_rfc3339(limited["attention"]["resume_at"].as_str().unwrap()).unwrap();
    let wait = resume_at.signed_duration_since(chrono::Utc::now());
    assert!(
        wait > chrono::Duration::seconds(30) && wait < chrono::Duration::seconds(150),
        "parsed `{at}` → {limited}"
    );
    let budget = Duration::from_secs(wait.num_seconds().max(0) as u64) + TICK * 3;
    let working = d.wait_state(&id, "working", budget).await;
    assert!(working["attention"].is_null(), "{working}");
    // The Enter reached the agent: an empty line is echoed (`echo:` — tmux
    // strips the trailing space from a captured pane).
    d.wait_screen(&id, "echo:", WAIT).await;
    d.kill(&id, false).await;
}

#[tokio::test]
async fn agent_that_dies_is_resumed_with_its_session_id() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    let mut ws = d.ws().await;
    let s = d.new_session("fake-agent", Some("/work once; /crash 2")).await;
    let id = s["id"].as_str().unwrap().to_string();
    let agent_id = s["agent_session_id"].as_str().unwrap().to_string();

    // Death → starting + `crashed` attention with the schedule (rule 2).
    let crashed = d.wait_until(&id, "crashed attention", WAIT, |s| s["attention"]["reason"] == "crashed").await;
    assert_eq!(state(&crashed), "starting", "{crashed}");
    assert_eq!(crashed["exit_code"], 2);
    let detail = crashed["attention"]["detail"].as_str().unwrap();
    assert!(detail.contains("exit 2") && detail.contains("resuming in 5s (attempt 1/3)"), "{detail}");
    assert!(crashed["attention"]["resume_at"].is_string());

    // Relaunched with `--resume <id>`; the pane proves it; the row's argv
    // is the resume argv; state comes from the scraper again until hooks
    // speak (a relaunch resets state_source to inferred).
    let resumed = d
        .wait_until(&id, "relaunched", FIRST_RESUME + WAIT, |s| {
            s["attention"].is_null() && s["argv"][1] == "--resume"
        })
        .await;
    assert_eq!(resumed["argv"][2], agent_id, "{resumed}");
    assert_eq!(resumed["agent_session_id"], agent_id, "the harness session survives the relaunch");
    let screen = d.wait_screen(&id, "resume=1", WAIT).await;
    assert!(screen.contains(&format!("sid={agent_id} resume=1")), "{screen}");
    let idle = d.wait_state(&id, "idle", WAIT).await;
    assert!(idle["pid"].as_u64().unwrap() != s["pid"].as_u64().unwrap(), "a new process: {idle}");
    // The story is in the event stream: crashed, then starting, then idle.
    let mut texts = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let Some(f) = ws.next(Duration::from_secs(1)).await else { break };
        if f["type"] == "flow.event" && f["id"] == id && f["kind"] == "system" {
            texts.push(f["text"].as_str().unwrap().to_string());
        }
    }
    assert!(texts.iter().any(|t| t.starts_with("starting · crashed: exit 2")), "{texts:?}");
    // A relaunch is starting → starting (no line); the resumed harness's
    // SessionStart is what makes it idle again.
    assert!(
        texts.iter().filter(|t| *t == "idle").count() >= 2,
        "idle before the crash and after the resume: {texts:?}"
    );
    // It works again after the resume (hooks re-attach to the same id).
    d.send(&id, "/work after").await;
    d.wait_until(&id, "idle via hooks after resume", WAIT, |s| state(s) == "idle" && s["state_source"] == "hooks")
        .await;
    let log = d.agent_log();
    assert!(log.contains("argv sid=") && log.matches("argv sid=").count() == 2, "two launches:\n{log}");
    d.kill(&id, false).await;
}

/// Slow (~45 s): three failed resumes (5 s + 10 s + 20 s backoff) → dead.
#[tokio::test]
async fn agent_that_keeps_crashing_is_dead_after_three_resumes() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    // The start-up script runs on every launch — fresh and resumed — so each
    // relaunch dies the same way.
    d.write_script(&d.ws.clone(), &["/crash 3"]);
    let s = d.new_session("fake-agent", None).await;
    let id = s["id"].as_str().unwrap().to_string();
    let budget = Duration::from_secs(5 + 10 + 20) + TICK * 6 + WAIT;
    let dead = d.wait_state(&id, "dead", budget).await;
    assert_eq!(dead["attention"]["reason"], "crashed");
    let detail = dead["attention"]["detail"].as_str().unwrap();
    assert!(detail.contains("exit 3") && detail.contains("gave up after 3 resumes"), "{detail}");
    assert_eq!(dead["exit_code"], 3);
    let log = d.agent_log();
    assert_eq!(log.matches("argv sid=").count(), 4, "one launch + three resumes:\n{log}");
    assert_eq!(log.matches("resume=1").count(), 3, "{log}");
    // Dead stays dead: no fourth relaunch.
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert_eq!(d.agent_log().matches("argv sid=").count(), 4);
    assert_eq!(state(&d.session(&id).await), "dead");
}

#[tokio::test]
async fn learned_session_id_binds_from_the_first_hook_and_kill_resume_relaunches() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    std::fs::write(d.ws.join(".fake-agent-session-id"), "learned-abc-123\n").unwrap();
    let s = d.new_session("fake-agent-learned", Some("/work bind")).await;
    let id = s["id"].as_str().unwrap().to_string();
    assert!(s["agent_session_id"].is_null(), "learned: no id until the harness reports one: {s}");
    assert_eq!(s["argv"][1], "/work bind", "no --session-id in a learned launch: {s}");

    // The first hook from the worktree binds the id to this row.
    let bound = d
        .wait_until(&id, "bound", WAIT, |s| s["agent_session_id"] == "learned-abc-123" && state(s) == "idle")
        .await;
    assert_eq!(bound["state_source"], "hooks");

    // kill --resume relaunches with `--resume <learned id>`.
    let relaunched = d.kill(&id, true).await;
    assert_eq!(state(&relaunched), "starting", "{relaunched}");
    assert_eq!(relaunched["argv"][1], "--resume");
    assert_eq!(relaunched["argv"][2], "learned-abc-123");
    d.wait_screen(&id, "sid=learned-abc-123 resume=1", WAIT).await;
    d.wait_state(&id, "idle", WAIT).await;
    d.kill(&id, false).await;
}

/// Rule 4: a resume is refused while another live process owns the same
/// harness session. Two rows can only share an id through store state (the
/// engine never binds an id twice), so the second row is real — a live
/// `fake-agent-learned` process — and the shared id is written to its row
/// the way a stale or foreign db row would carry it.
#[tokio::test]
async fn duplicate_resume_guard_holds_when_a_live_pid_owns_the_session() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    std::fs::write(d.ws.join(".fake-agent-session-id"), "shared-s1\n").unwrap();
    let a = d.new_session("fake-agent-learned", Some("/work a")).await;
    let a_id = a["id"].as_str().unwrap().to_string();
    d.wait_until(&a_id, "A bound", WAIT, |s| s["agent_session_id"] == "shared-s1" && state(s) == "idle")
        .await;

    let other = d.extra_repo("ws2");
    let b = d.new_session_in("fake-agent-learned", Some("/work b"), &other).await;
    let b_id = b["id"].as_str().unwrap().to_string();
    let b = d.wait_state(&b_id, "idle", WAIT).await;
    let b_pid = b["pid"].as_u64().unwrap() as u32;
    assert!(Daemon::pid_alive(b_pid));
    d.store().set_agent_session(&b_id, "shared-s1").unwrap();

    // A's resume is held, naming B's pid; A is not relaunched.
    let held = d.kill(&a_id, true).await;
    assert_eq!(state(&held), "needs_you", "{held}");
    assert_eq!(held["attention"]["reason"], "held");
    let detail = held["attention"]["detail"].as_str().unwrap();
    assert!(detail.contains("shared-s1") && detail.contains(&format!("live pid {b_pid}")), "{detail}");
    tokio::time::sleep(TICK * 2).await;
    let a_now = d.session(&a_id).await;
    assert_eq!(state(&a_now), "needs_you", "the supervisor leaves a held row alone: {a_now}");
    assert_eq!(d.agent_log().matches("argv sid=").count(), 1, "A was not relaunched:\n{}", d.agent_log());

    // Once B is gone the claim lapses and the resume goes through.
    d.kill(&b_id, false).await;
    d.wait_state(&b_id, "done", WAIT).await;
    let relaunched = d.kill(&a_id, true).await;
    assert_eq!(state(&relaunched), "starting", "{relaunched}");
    d.wait_screen(&a_id, "sid=shared-s1 resume=1", WAIT).await;
    d.kill(&a_id, false).await;
}

#[tokio::test]
async fn native_harness_reports_its_own_turns() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    let s = d.new_session("fake-agent-native", Some("/work n1")).await;
    let id = s["id"].as_str().unwrap().to_string();
    let idle = d
        .wait_until(&id, "idle via native", WAIT, |s| state(s) == "idle" && s["state_source"] == "native")
        .await;
    assert_eq!(idle["unread"], true);
    d.wait_screen(&id, "worked: n1", WAIT).await;
    let log = d.agent_log();
    assert!(log.contains("hook turn_start → {}") && log.contains("hook turn_end → {}"), "{log}");
    assert!(!log.contains("SessionStart"), "a native harness has no SessionStart: {log}");

    // `ask` maps to needs_you through event_map, with the payload's reason;
    // there is no long-poll, so approve presses the key on the pane.
    d.send(&id, "/perm").await;
    let ask = d.wait_state(&id, "needs_you", WAIT).await;
    assert_eq!(ask["attention"]["reason"], "permission");
    assert_eq!(ask["attention"]["detail"], "run git push?");
    assert!(ask["attention"]["request_id"].is_null(), "{ask}");
    d.approve(&id, "none", "allow").await;
    d.wait_state(&id, "working", WAIT).await;
    // The keystroke path typed `1`; the next steer line shows it (tmux can't
    // paste an empty buffer, so the line is `1x`).
    d.send(&id, "x").await;
    d.wait_screen(&id, "echo: 1x", WAIT).await;
    d.send(&id, "/ask ready?").await;
    let q = d.wait_until(&id, "question", WAIT, |s| s["attention"]["reason"] == "question").await;
    assert_eq!(q["attention"]["detail"], "ready?");
    d.send(&id, "/exit 0").await;
    let done = d.wait_state(&id, "done", WAIT + TICK).await;
    assert_eq!(done["exit_code"], 0);
    assert!(d.agent_log().contains("hook bye → {}"));
}

#[tokio::test]
async fn scrape_harness_state_is_inferred_from_the_pane() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    let s = d.new_session("fake-agent-scrape", Some("/work s1")).await;
    let id = s["id"].as_str().unwrap().to_string();
    // The "working" marker is on screen for FAKE_AGENT_WORK_SECS (5 s).
    let working = d.wait_state(&id, "working", WAIT).await;
    assert_eq!(working["state_source"], "inferred");
    let idle = d.wait_until(&id, "idle scraped", WAIT, |s| state(s) == "idle").await;
    assert_eq!(idle["state_source"], "inferred", "scraping never claims `hooks`: {idle}");
    assert_eq!(idle["unread"], true, "working → idle by scrape is unread too: {idle}");
    assert!(
        d.agent_log().contains("cmd /work s1") && !d.agent_log().contains("hook "),
        "scrape mode posts nothing:\n{}",
        d.agent_log()
    );

    // An approval menu on the pane → needs_you with a scrape- request id;
    // approve presses the menu key (2 = allow_session).
    d.send(&id, "/perm").await;
    let ask = d.wait_state(&id, "needs_you", WAIT).await;
    assert!(ask["attention"]["request_id"].as_str().unwrap().starts_with("scrape-"), "{ask}");
    assert_eq!(ask["attention"]["detail"], "approval prompt on screen");
    let request_id = ask["attention"]["request_id"].as_str().unwrap().to_string();
    d.approve(&id, &request_id, "allow_session").await;
    d.wait_screen(&id, "decision: allow_session", WAIT).await;
    d.wait_state(&id, "working", WAIT).await;

    // The banner → limited, scraped like any harness.
    d.send(&id, "/limit 11:59pm").await;
    let limited = d.wait_state(&id, "limited", WAIT).await;
    assert_eq!(limited["attention"]["reason"], "usage_limit");

    // relaunch_command: a crash relaunches the ORIGINAL argv (resume=0, the
    // prompt runs again).
    let (status, _) = d.post(&format!("/api/flow/sessions/{id}/send"), json!({"text":"/crash 4"})).await;
    assert_eq!(status, 200);
    d.wait_until(&id, "crashed", WAIT, |s| s["attention"]["reason"] == "crashed").await;
    let relaunched = d
        .wait_until(&id, "relaunched", FIRST_RESUME + WAIT, |s| s["attention"].is_null() && state(s) != "limited")
        .await;
    assert_eq!(relaunched["argv"], s["argv"], "relaunch_command reuses the launch argv: {relaunched}");
    let screen = d.wait_screen(&id, "resume=0 mode=scrape", WAIT).await;
    assert!(
        d.agent_log().matches("cmd /work s1").count() >= 2,
        "the prompt ran again: {screen}\n{}",
        d.agent_log()
    );
    d.kill(&id, false).await;
}
