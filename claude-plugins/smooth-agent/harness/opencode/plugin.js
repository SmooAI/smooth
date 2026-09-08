// smooth-agent OpenCode lifecycle plugin (pearl th-cc50cd / EPIC th-1945b9).
//
// Puts every OpenCode session on the th-mail bus, the way the smooth-agent
// Claude Code plugin's SessionStart hook does: register a placeholder handle
// per session, publish working/idle presence, and mark the agent offline when
// the session is deleted. The mailbox is the machine-level SQLite store
// (~/.smooth/mail.db) shared by every harness that runs `th`.
//
// Installed into ~/.config/opencode/plugins/ by `th harness enable opencode`
// (as a symlink into the smooth-agent plugin checkout, so `disable` can
// recognize and remove it). Degrades to a silent no-op when `th` is not on
// PATH — an outside contributor without the CLI loses nothing.
//
// Message delivery INTO a running session (inbox at prompt boundaries) is
// deliberately not here yet: it needs the OpenCode SDK client, not a shell-out.
// The pearl tracks it.
//
// SmoothFlow (pearl th-5c5457): every lifecycle event is ALSO posted to the
// daemon's flow engine, the same body flow-hook.sh sends for Claude Code —
// {harness:"opencode", event:<Claude event name>, session_id, cwd, payload} —
// so an opencode session's state comes from hooks, not pane scraping. The
// engine binds the session id from the first event by cwd (opencode can't
// pre-assign one). Fire-and-forget, 2 s cap, silent when the daemon is down.

const HARNESS = 'opencode';
const TOUCH_EVERY_MS = 60_000;
const FLOW_TIMEOUT_MS = 2_000;
// OpenCode lifecycle → the Claude Code hook event the engine already maps.
const FLOW_EVENTS = {
    'session.created': 'SessionStart',
    'tool.execute.before': 'PreToolUse',
    'session.idle': 'Stop',
    'session.deleted': 'SessionEnd',
};

const readFile = async (p) => {
    try {
        const fs = await import('node:fs/promises');
        return (await fs.readFile(p, 'utf8')).trim();
    } catch {
        return '';
    }
};

/// `http://<daemon.addr>/api/flow/hooks`, or '' when no daemon is advertised.
export const flowHooksUrl = async (addrFile = process.env.SMOOTH_DAEMON_ADDR_FILE || `${process.env.HOME || ''}/.smooth/daemon.addr`) => {
    const addr = await readFile(addrFile);
    if (!addr) return '';
    return `${/^https?:\/\//.test(addr) ? addr : `http://${addr}`}/api/flow/hooks`;
};

const sanitize = (s) => s.toLowerCase().replace(/[^a-z0-9-]/g, '');

export const SmoothAgent = async ({ $, directory }) => {
    const cwd = directory || process.cwd();
    const base = sanitize(cwd.split('/').filter(Boolean).pop() || 'session') || 'session';
    const flowUrl = await flowHooksUrl();
    const flow = (hook, sid, payload) => {
        const event = FLOW_EVENTS[hook];
        if (!flowUrl || !event || typeof fetch !== 'function') return;
        const body = JSON.stringify({ harness: HARNESS, event, session_id: sid, cwd, payload });
        fetch(flowUrl, {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body,
            signal: AbortSignal.timeout(FLOW_TIMEOUT_MS),
        }).catch(() => {});
    };
    const handles = new Map(); // session id -> registered handle
    const lastTouch = new Map(); // session id -> ms timestamp
    let thOk = true; // ponytail: first failure disables all calls (th missing / broken)

    const th = async (...args) => {
        if (!thOk) return;
        try {
            await $`th ${args}`.quiet();
        } catch {
            thOk = false;
        }
    };

    // Event payload shapes vary a little across OpenCode versions; every
    // accessor is hedged and an unidentifiable session is simply skipped.
    const sessionId = (input) => {
        const sid = input?.session?.id ?? input?.sessionID ?? input?.info?.id ?? '';
        return typeof sid === 'string' && sid ? sid : '';
    };

    const handleFor = (sid) => {
        if (!handles.has(sid)) {
            handles.set(sid, sanitize(`oc-${base}-${sid.slice(-4)}`) || `oc-${base}-${process.pid}`);
        }
        return handles.get(sid);
    };

    return {
        'session.created': async (input) => {
            const sid = sessionId(input);
            if (!sid) return;
            flow('session.created', sid, {});
            // --pid: the long-lived OpenCode process, so `th agent list` reaps
            // the row when it dies (same contract as the Claude Code hook).
            await th('agent', 'register', '--name', handleFor(sid), '--harness', HARNESS, '--pid', String(process.pid));
        },
        'tool.execute.before': async (input) => {
            const sid = sessionId(input);
            if (!sid || !handles.has(sid)) return;
            flow('tool.execute.before', sid, { tool_name: input?.tool ?? 'tool', tool_input: input?.args ?? {} });
            const now = Date.now();
            if (now - (lastTouch.get(sid) ?? 0) < TOUCH_EVERY_MS) return;
            lastTouch.set(sid, now);
            await th('agent', 'status', '--name', handleFor(sid), '--status', 'working');
        },
        'session.idle': async (input) => {
            const sid = sessionId(input);
            if (!sid || !handles.has(sid)) return;
            flow('session.idle', sid, {});
            lastTouch.delete(sid);
            await th('agent', 'status', '--name', handleFor(sid), '--status', 'idle');
        },
        'session.deleted': async (input) => {
            const sid = sessionId(input);
            if (!sid || !handles.has(sid)) return;
            flow('session.deleted', sid, {});
            await th('agent', 'status', '--name', handleFor(sid), '--status', 'offline');
            handles.delete(sid);
            lastTouch.delete(sid);
        },
    };
};
