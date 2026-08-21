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

const HARNESS = 'opencode';
const TOUCH_EVERY_MS = 60_000;

const sanitize = (s) => s.toLowerCase().replace(/[^a-z0-9-]/g, '');

export const SmoothAgent = async ({ $, directory }) => {
    const base = sanitize((directory || process.cwd()).split('/').filter(Boolean).pop() || 'session') || 'session';
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
            // --pid: the long-lived OpenCode process, so `th agent list` reaps
            // the row when it dies (same contract as the Claude Code hook).
            await th('agent', 'register', '--name', handleFor(sid), '--harness', HARNESS, '--pid', String(process.pid));
        },
        'tool.execute.before': async (input) => {
            const sid = sessionId(input);
            if (!sid || !handles.has(sid)) return;
            const now = Date.now();
            if (now - (lastTouch.get(sid) ?? 0) < TOUCH_EVERY_MS) return;
            lastTouch.set(sid, now);
            await th('agent', 'status', '--name', handleFor(sid), '--status', 'working');
        },
        'session.idle': async (input) => {
            const sid = sessionId(input);
            if (!sid || !handles.has(sid)) return;
            lastTouch.delete(sid);
            await th('agent', 'status', '--name', handleFor(sid), '--status', 'idle');
        },
        'session.deleted': async (input) => {
            const sid = sessionId(input);
            if (!sid || !handles.has(sid)) return;
            await th('agent', 'status', '--name', handleFor(sid), '--status', 'offline');
            handles.delete(sid);
            lastTouch.delete(sid);
        },
    };
};
