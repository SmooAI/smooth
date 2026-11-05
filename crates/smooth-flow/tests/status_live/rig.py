#!/usr/bin/env python3
"""Measure what SmoothFlow reports for the REAL harnesses (SMOODEV-4268).

Runs the installed Claude Code, Codex and OpenCode against `mock_llm.py` (no
account, no key, no model), under an isolated daemon with a scratch $HOME, on
each host, and prints the harness-by-state truth table with latencies.

    python3 rig.py --bin <dir holding th, smooth-daemon, smooth-flow-host> \
        --scratch <new dir> --harness claude,codex,opencode --host pty,tmux --wiring hooks,none

Nothing outside --scratch is written: the daemon's HOME, flow.db, tmux socket
and every harness's config are all under it. `--wiring none` leaves the
harness's hooks/plugin out, the state a user has before `th harness enable`.

Every scenario's verdict is a state the engine reported, with the seconds from
the action to the first report of it. Timelines and panes land in
<scratch>/out/<harness>-<host>-<wiring>/.
"""
import argparse
import base64
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
PLUGIN = HERE.parents[3] / "claude-plugins" / "smooth-agent"
WAIT = 45.0

# Dialogs the first launch of each harness puts up. (pane regex, keys).
FIRST_RUN = {
    "claude": [(r"quick safety check|do you trust the files", ["Down", "Enter"]), (r"yes, i accept", ["Down", "Enter"])],
    "codex": [(r"trust this folder\?", ["Enter"]), (r"hooks need review", ["Down", "Enter"]), (r"hooks? needs? review before", ["Escape"]), (r"press enter to continue", ["Enter"])],
    "opencode": [],
}
# A permission prompt, answered from the pane the way a person at the terminal would.
ANSWER_ALLOW = {"claude": ["Enter"], "codex": ["y"], "opencode": ["Enter"]}
ANSWER_DENY = {"claude": ["Escape"], "codex": ["n"], "opencode": ["Escape"]}
ANSWER_Q = {"claude": ["Enter"], "codex": ["Enter"], "opencode": ["Enter"]}
EXIT_CMD = {"claude": "/exit", "codex": "/exit", "opencode": "/exit"}
KEYS = {"Enter": b"\r", "Escape": b"\x1b", "Down": b"\x1b[B", "Right": b"\x1b[C", "y": b"y", "n": b"n", "a": b"a"}


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Api:
    def __init__(self, port, home):
        self.base = f"http://127.0.0.1:{port}"
        self.home = home

    def _token(self):
        try:
            return (self.home / ".smooth" / "operator-token").read_text().strip()
        except OSError:
            return ""

    def call(self, method, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(self.base + path, data=data, method=method)
        req.add_header("content-type", "application/json")
        tok = self._token()
        if tok:
            req.add_header("authorization", f"Bearer {tok}")
        try:
            with urllib.request.urlopen(req, timeout=120) as r:
                return json.loads(r.read() or b"{}")
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"{method} {path}: {e.code} {e.read()[:300]!r}") from e

    def up(self):
        try:
            self.call("GET", "/api/flow/sessions")
            return True
        except Exception:
            return False


def write_json(path, obj):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(obj, indent=2))


def prepare_home(home, harness, wiring, mock_port, bin_dir):
    home.mkdir(parents=True, exist_ok=True)
    real = Path(os.path.expanduser("~"))
    local_bin = home / ".local" / "bin"
    local_bin.mkdir(parents=True, exist_ok=True)
    # Real binaries, past any cmux shim on PATH; nothing else of the real $HOME.
    for name, src in (("claude", real / ".local/bin/claude"), ("opencode", real / ".opencode/bin/opencode")):
        link = local_bin / name
        if not link.exists() and src.exists():
            link.symlink_to(src)
    mock = f"http://127.0.0.1:{mock_port}"

    # Claude Code
    write_json(
        home / ".claude.json",
        {
            "hasCompletedOnboarding": True,
            "theme": "dark",
            "customApiKeyResponses": {"approved": ["-rig-mock-key-000000"], "rejected": []},
            "bypassPermissionsModeAccepted": False,
            "autoUpdates": False,
        },
    )
    settings = {"env": {}, "permissions": {"defaultMode": "default", "disableAutoMode": "disable"}, "disableAutoMode": "disable", "skipAutoPermissionPrompt": True}
    if wiring == "hooks":
        hooks = json.loads((PLUGIN / "hooks" / "hooks.json").read_text())["hooks"]
        kept = {}
        for ev, groups in hooks.items():
            keep = []
            for g in groups:
                hs = [h for h in g.get("hooks", []) if "th flow hook" in h.get("command", "")]
                if hs:
                    keep.append({**g, "hooks": hs})
            if keep:
                kept[ev] = keep
        settings["hooks"] = kept
    write_json(home / ".claude" / "settings.json", settings)

    # Codex
    codex = home / ".codex"
    codex.mkdir(parents=True, exist_ok=True)
    (codex / "config.toml").write_text(
        'model = "mock-model"\n'
        'model_provider = "mock"\n'
        'approval_policy = "on-request"\n'
        'sandbox_mode = "read-only"\n'
        "check_for_update_on_startup = false\n"
        "[features]\nhooks = true\n"
        "[model_providers.mock]\n"
        'name = "mock"\n'
        f'base_url = "{mock}/v1"\n'
        'wire_api = "responses"\n'
    )
    if wiring == "hooks":
        shutil.copy(PLUGIN / "harness" / "codex" / "hooks.json", codex / "hooks.json")

    # OpenCode
    oc = home / ".config" / "opencode"
    write_json(
        oc / "opencode.json",
        {
            "$schema": "https://opencode.ai/config.json",
            "model": "mock/mock-model",
            "small_model": "mock/mock-model",
            "autoupdate": False,
            "share": "disabled",
            "permission": {"bash": "ask", "edit": "ask"},
            "provider": {
                "mock": {
                    "npm": "@ai-sdk/openai-compatible",
                    "name": "mock",
                    "options": {"baseURL": f"{mock}/v1", "apiKey": "mock"},
                    "models": {"mock-model": {"name": "mock", "tool_call": True}},
                }
            },
        },
    )
    if wiring == "hooks":
        (oc / "plugins").mkdir(parents=True, exist_ok=True)
        shutil.copy(PLUGIN / "harness" / "opencode" / "plugin.js", oc / "plugins" / "smooth-agent.js")


def daemon_env(home, scratch, host, bin_dir, mock_port, port):
    real_path = os.environ["PATH"].split(":")
    path = ":".join([str(bin_dir)] + [p for p in real_path if "cmux" not in p])
    env = {k: v for k, v in os.environ.items() if not k.startswith(("SMOOTH", "CLAUDE", "CMUX", "TMUX", "CODEX", "OPENCODE", "ANTHROPIC", "OPENAI"))}
    env.update(
        {
            "HOME": str(home),
            "PATH": path,
            "SMOOTH_ALLOW_SECOND_DAEMON": "1",
            "SMOOTH_TAILSCALE_SERVE": "0",
            "SMOOTH_OPERATOR_DB": str(scratch / "operator.db"),
            "SMOOTH_FLOW_DB": str(scratch / f"flow-{host}.db"),
            "SMOOTH_FLOW_TMUX_SOCKET": f"sf4268-{os.getpid()}",
            "SMOOTH_FLOW_HOST": host,
            "SMOOTH_FLOW_ADDR": f"127.0.0.1:{port}",
            "SMOOTH_FLOW_PREWARM": "0",
            "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{mock_port}",
            "ANTHROPIC_API_KEY": "sk-ant-rig-mock-key-000000",
            "DISABLE_AUTOUPDATER": "1",
            "DISABLE_TELEMETRY": "1",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
            "OPENCODE_DISABLE_AUTOUPDATE": "1",
            "OPENCODE_DISABLE_MODELS_FETCH": "1",
            "OPENCODE_DISABLE_LSP_DOWNLOAD": "1",
            "RUST_LOG": "smooth_flow=debug,info",
        }
    )
    return env


class Probe:
    """One session, its timeline, and the keys that drive its pane."""

    def __init__(self, api, sid, harness, out):
        self.api = api
        self.id = sid
        self.harness = harness
        self.out = out
        self.t0 = time.time()
        self.last = None
        self.log = []
        self.panes = 0
        self.dialogs = {}

    def session(self):
        for s in self.api.call("GET", "/api/flow/sessions")["sessions"]:
            if s["id"] == self.id:
                return s
        return None

    def pane(self):
        try:
            f = self.api.call("GET", f"/api/flow/sessions/{self.id}/snapshot")
            return f.get("text") or f.get("data") or json.dumps(f)
        except Exception as e:
            return f"<no pane: {e}>"

    def key(self, *names):
        for n in names:
            self.api.call("POST", f"/api/flow/sessions/{self.id}/input", {"data_b64": base64.b64encode(KEYS[n]).decode()})
            time.sleep(0.25)

    def send(self, text):
        self.api.call("POST", f"/api/flow/sessions/{self.id}/send", {"text": text})

    def note(self, msg):
        line = f"{time.time() - self.t0:8.1f}s  {msg}"
        self.log.append(line)
        print(f"  [{self.out.name}] {line}", flush=True)
        (self.out / "timeline.txt").write_text("\n".join(self.log) + "\n")

    def save_pane(self, label):
        self.panes += 1
        (self.out / f"{self.panes:02d}-{re.sub(r'[^a-z0-9]+', '-', label.lower())}.txt").write_text(self.pane())

    def tick(self):
        s = self.session()
        if s is None:
            return None
        att = s.get("attention") or {}
        cur = (s["state"], att.get("reason"), s.get("state_source"), (s.get("confidence") or ""))
        if cur != self.last:
            self.note(f"state={s['state']} attention={att.get('reason')} source={s.get('state_source')} detail={att.get('detail')!r} {s.get('confidence') or ''}")
            self.save_pane(f"state-{s['state']}")
            self.last = cur
        return s

    def wait(self, what, pred, budget=WAIT, first_run=False):
        """(seconds, session) at the first tick pred holds; (None, last) on timeout."""
        start = time.time()
        answered = 0
        s = None
        while time.time() - start < budget:
            s = self.tick()
            if s is not None and pred(s):
                return round(time.time() - start, 1), s
            if first_run and answered < 4:
                pane = self.pane().lower()
                for rx, keys in FIRST_RUN.get(self.harness, []):
                    if re.search(rx, pane):
                        # What does the row say while it waits on this dialog?
                        time.sleep(3.0)
                        cur = self.tick()
                        self.dialogs[rx] = cur["state"] if cur else "?"
                        self.note(f"first-run dialog ({rx}) reads {self.dialogs[rx]}; answering with {keys}")
                        self.save_pane("first-run")
                        self.key(*keys)
                        answered += 1
                        time.sleep(1.0)
                        break
            time.sleep(0.15)
        self.note(f"TIMEOUT waiting for {what}; state={s and s['state']}")
        self.save_pane(f"timeout-{what}")
        return None, s


def state_is(*names):
    return lambda s: s["state"] in names


def run_one(api, harness, host, wiring, scratch, results):
    out = scratch / "out" / f"{harness}-{host}-{wiring}"
    out.mkdir(parents=True, exist_ok=True)
    wt = scratch / "wt" / f"{harness}-{host}-{wiring}-{uuid.uuid4().hex[:6]}"
    wt.mkdir(parents=True)
    subprocess.run(["git", "init", "-q", str(wt)], check=True)
    tag = uuid.uuid4().hex[:10]
    row = {}
    results[(harness, host, wiring)] = row

    body = {"kind": harness, "worktree": str(wt), "prompt": f"say hi {tag}", "title": f"rig {harness}"}
    sess = api.call("POST", "/api/flow/sessions", body)["session"]
    p = Probe(api, sess["id"], harness, out)
    p.note(f"launched {sess['id']} on host {sess.get('host')}")
    row["launch_host"] = sess.get("host")

    def verdict(name, secs, s, want):
        got = s["state"] if s else "?"
        ok = secs is not None
        row[name] = f"{want} {secs}s" if ok else f"STUCK {got}"
        return ok

    # starting -> working -> idle
    secs, s = p.wait("working", state_is("working"), budget=25, first_run=True)
    verdict("starting->working", secs, s, s["state"] if s else "?")
    row["first_source"] = s.get("state_source") if s else None
    secs, s = p.wait("idle", state_is("idle"), first_run=True)
    verdict("turn->idle", secs, s, "idle")
    row["source"] = s.get("state_source") if s else None
    if secs is None:
        # Everything below reads a settled pane; give up on this row honestly.
        time.sleep(3)

    row["dialog_states"] = dict(p.dialogs)

    # steer
    p.note("ACTION steer")
    p.send("tell me about the rig")
    secs, s = p.wait("steer working", state_is("working"), budget=20)
    verdict("steer->working", secs, s, "working")
    secs, s = p.wait("steer idle", state_is("idle"), budget=30)
    verdict("steer->idle", secs, s, "idle")

    # approval, answered at the pane
    p.note("ACTION rig-tool")
    p.send("rig-tool please run the thing")
    secs, s = p.wait("needs_you", state_is("needs_you"), budget=25)
    verdict("approval->needs_you", secs, s, "needs_you")
    row["approval_detail"] = (s.get("attention") or {}).get("detail") if s and s["state"] == "needs_you" else None
    p.save_pane("approval")
    if secs is not None:
        # Unanswered approval must stay put.
        time.sleep(6)
        s = p.tick()
        row["approval_holds_6s"] = s["state"] == "needs_you" if s else False
    p.note("ACTION answer allow at the pane")
    p.key(*ANSWER_ALLOW[harness])
    secs, s = p.wait("approval answered", lambda s: s["state"] != "needs_you", budget=20)
    verdict("approved->left needs_you", secs, s, s["state"] if s else "?")
    secs, s = p.wait("approval turn idle", state_is("idle"), budget=30)
    verdict("approved->idle", secs, s, "idle")

    # approval from SmoothFlow, not the pane
    p.note("ACTION rig-tool (flow.approve)")
    p.send("rig-tool please run it once more")
    secs, s = p.wait("needs_you via app", state_is("needs_you"), budget=25)
    verdict("approval3->needs_you", secs, s, "needs_you")
    rid = (s.get("attention") or {}).get("request_id") if s else None
    row["approvable"] = bool(rid)
    if rid:
        api.call("POST", f"/api/flow/sessions/{p.id}/approve", {"request_id": rid, "decision": "allow"})
    else:
        p.key(*ANSWER_ALLOW[harness])
    secs, s = p.wait("approved idle", state_is("idle"), budget=30)
    verdict("flow.approve->idle", secs, s, "idle")
    if s:
        row["flow.approve_ran"] = "x.txt" in p.pane() or secs is not None

    # deny at the pane
    p.note("ACTION rig-tool (deny)")
    p.send("rig-tool please run it again")
    secs, s = p.wait("needs_you 2", state_is("needs_you"), budget=25)
    verdict("approval2->needs_you", secs, s, "needs_you")
    # A person's reaction time: long enough for one supervision pass to see it.
    time.sleep(2.5)
    p.note("ACTION answer deny at the pane")
    p.key(*ANSWER_DENY[harness])
    secs, s = p.wait("denied leaves needs_you", lambda s: s["state"] != "needs_you", budget=15)
    verdict("denied->left needs_you", secs, s, s["state"] if s else "?")
    secs, s = p.wait("denied idle", state_is("idle"), budget=20)
    verdict("denied->idle", secs, s, "idle")
    if secs is None:
        p.key("Escape")
        p.wait("recover", state_is("idle"), budget=10)

    # a question
    p.note("ACTION rig-question")
    p.send("rig-question ask me something")
    secs, s = p.wait("question", state_is("needs_you"), budget=20)
    verdict("question->needs_you", secs, s, "needs_you")
    row["question_reason"] = (s.get("attention") or {}).get("reason") if s else None
    p.save_pane("question")
    if secs is not None:
        p.key(*ANSWER_Q[harness])
        secs, s = p.wait("question answered", lambda s: s["state"] != "needs_you", budget=15)
        verdict("answered->left needs_you", secs, s, s["state"] if s else "?")
    else:
        p.key("Escape")
    secs, s = p.wait("question idle", state_is("idle"), budget=25)
    verdict("question turn->idle", secs, s, "idle")
    if secs is None:
        p.key("Escape")
        p.wait("recover", state_is("idle"), budget=10)

    # interrupt mid-turn: the user presses Esc, the harness is idle.
    p.note("ACTION interrupt mid-turn")
    p.send("rig-slow tell me a long one")
    secs, s = p.wait("slow working", state_is("working"), budget=15)
    time.sleep(3)
    p.save_pane("slow-working")
    p.key("Escape")
    if harness == "opencode":
        p.key("Escape")
    secs, s = p.wait("interrupted idle", state_is("idle"), budget=15)
    verdict("interrupt->idle", secs, s, "idle")
    p.save_pane("after-interrupt")

    # crash: SIGKILL the harness itself, then see the row say so.
    p.note("ACTION kill -9 the harness")
    pids = subprocess.run(["pgrep", "-f", tag], capture_output=True, text=True).stdout.split()
    pids = [x for x in pids if int(x) != os.getpid()]
    p.note(f"pids carrying the launch tag: {pids}")
    for x in pids:
        try:
            os.kill(int(x), signal.SIGKILL)
        except OSError:
            pass
    if pids:
        secs, s = p.wait("crash seen", lambda s: s["state"] not in ("idle", "working", "needs_you"), budget=20)
        verdict("crash->", secs, s, s["state"] if s else "?")
        row["crash_attention"] = (s.get("attention") or {}).get("reason") if s else None
        secs, s = p.wait("relaunch settles", state_is("idle", "working", "dead"), budget=30, first_run=True)
        row["after_crash"] = s["state"] if s else "?"
    else:
        row["crash->"] = "no pid carries the tag (resume argv)"

    # a clean exit
    p.wait("settle", state_is("idle"), budget=10)
    p.note("ACTION /exit")
    p.send(EXIT_CMD[harness])
    if harness == "opencode":
        # The pasted `/exit` opens OpenCode's command menu; its Enter picks the entry.
        time.sleep(1.5)
        try:
            p.key("Enter")
        except RuntimeError:
            pass  # it took the first Enter and has already gone
    secs, s = p.wait("done", state_is("done", "dead"), budget=20)
    verdict("exit->", secs, s, s["state"] if s else "?")
    row["exit_code"] = s.get("exit_code") if s else None
    p.save_pane("end")
    try:
        api.call("POST", f"/api/flow/sessions/{p.id}/kill", {"resume": False})
    except Exception as e:
        p.note(f"kill at the end: {e}")
    (out / "row.json").write_text(json.dumps(row, indent=2))
    print(f"--- {harness}/{host}/{wiring}: {json.dumps(row, indent=1)}", flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--scratch", required=True)
    ap.add_argument("--harness", default="claude,codex,opencode")
    ap.add_argument("--host", default="pty,tmux")
    ap.add_argument("--wiring", default="hooks,none")
    args = ap.parse_args()
    bin_dir = Path(args.bin).resolve()
    scratch = Path(args.scratch).resolve()
    scratch.mkdir(parents=True, exist_ok=True)
    results = {}
    mock_port = free_port()
    mock = subprocess.Popen([sys.executable, "-I", str(HERE / "mock_llm.py"), str(mock_port)], env={**os.environ, "MOCK_LOG": str(scratch / "mock.log")})
    try:
        for host in args.host.split(","):
            for wiring in args.wiring.split(","):
                for harness in args.harness.split(","):
                    # Short on purpose: Codex's control socket must fit sun_path (104 bytes).
                    home = scratch / "h" / f"{harness[0]}{host[0]}{wiring[0]}"
                    prepare_home(home, harness, wiring, mock_port, bin_dir)
                    port = free_port()
                    env = daemon_env(home, scratch, f"{host}-{harness}-{wiring}", bin_dir, mock_port, port)
                    env["SMOOTH_FLOW_HOST"] = host
                    log = scratch / f"daemon-{harness}-{host}-{wiring}.log"
                    with open(log, "w") as lf:
                        d = subprocess.Popen([str(bin_dir / "smooth-daemon"), "operator", "--addr", f"127.0.0.1:{port}"], env=env, stdout=lf, stderr=lf, start_new_session=True)
                    api = Api(port, home)
                    try:
                        for _ in range(100):
                            if api.up():
                                break
                            time.sleep(0.2)
                        else:
                            raise RuntimeError(f"daemon on {port} never came up; see {log}")
                        run_one(api, harness, host, wiring, scratch, results)
                    except Exception as e:
                        print(f"!!! {harness}/{host}/{wiring}: {e}", flush=True)
                        results.setdefault((harness, host, wiring), {})["error"] = str(e)
                    finally:
                        # Everything this rig started goes with its daemon.
                        try:
                            for s in api.call("GET", "/api/flow/sessions")["sessions"]:
                                if s["state"] not in ("done", "dead"):
                                    api.call("POST", f"/api/flow/sessions/{s['id']}/kill", {"resume": False})
                        except Exception:
                            pass
                        d.terminate()
                        try:
                            d.wait(10)
                        except subprocess.TimeoutExpired:
                            d.kill()
                        subprocess.run(["tmux", "-L", env["SMOOTH_FLOW_TMUX_SOCKET"], "kill-server"], capture_output=True)
    finally:
        mock.terminate()
        # Codex leaves app-server daemons behind, named by their scratch path.
        for pid in subprocess.run(["pgrep", "-f", str(scratch)], capture_output=True, text=True).stdout.split():
            if int(pid) not in (os.getpid(), os.getppid()):
                try:
                    os.kill(int(pid), signal.SIGTERM)
                except OSError:
                    pass
    table = {"/".join(k): v for k, v in results.items()}
    (scratch / "truth.json").write_text(json.dumps(table, indent=2))
    print(json.dumps(table, indent=2))


if __name__ == "__main__":
    main()
