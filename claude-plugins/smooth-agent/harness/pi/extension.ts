// smooth-agent Pi extension — SmoothFlow state (pearl th-b00115).
//
// Pi has no command hooks; its extensibility is in-process TypeScript
// extensions loaded from ~/.pi/agent/extensions/*.ts. `th pkg install` links
// this file there as <pkg>.ts. Each lifecycle event is posted to the daemon's
// flow engine in the same envelope flow-hook.sh sends for Claude Code:
//
//   POST http://<daemon.addr>/api/flow/hooks
//        {harness:"pi", event, session_id, cwd, payload, flow_id?}
//
// `event` is Pi's own name; the `pi` harness manifest's event_map reads it
// (agent_start / tool_execution_start → working, agent_end → idle,
// session_shutdown → ended). session_id is ctx.sessionManager.getSessionId() —
// the id SmoothFlow pre-assigned with `pi --session-id`. Pi has no permission
// prompts, so there is nothing to report as needs_you.
//
// Only observational events: none of these handlers returns anything, so
// nothing here can block, modify or cancel Pi's work. Pi awaits extension
// handlers, so the post is started and NOT awaited. Silent no-op without a
// daemon. Plain JS syntax (valid TypeScript) so node can test it.

export const HARNESS = 'pi';
export const FLOW_EVENTS = ['session_start', 'agent_start', 'tool_execution_start', 'agent_end', 'session_shutdown'];
const FLOW_TIMEOUT_MS = 2000;

/** `http://<addr>/api/flow/hooks`, or '' when nothing is advertised. */
export const flowHooksUrl = (addr) => {
    const a = String(addr || '').trim();
    if (!a) return '';
    return `${/^https?:\/\//.test(a) ? a : `http://${a}`}/api/flow/hooks`;
};

/** Pi's session id from a handler context, or ''. Never throws. */
export const sessionIdOf = (ctx) => {
    try {
        const id = ctx?.sessionManager?.getSessionId?.();
        return typeof id === 'string' ? id : '';
    } catch {
        return '';
    }
};

/** The hook envelope for one Pi event. Pure. */
export const flowBody = (event, ev, ctx, env = {}, cwd = '') => {
    const e = ev && typeof ev === 'object' ? ev : {};
    const payload = { hook_event_name: event };
    if (typeof e.reason === 'string') payload.reason = e.reason;
    if (typeof e.toolName === 'string') payload.tool_name = e.toolName;
    const body = { harness: HARNESS, event, session_id: sessionIdOf(ctx), cwd: ctx?.cwd || cwd, payload };
    if (env.SMOOTH_FLOW_ID) body.flow_id = env.SMOOTH_FLOW_ID;
    return body;
};

const readAddr = async (env) => {
    const file = env.SMOOTH_DAEMON_ADDR_FILE || `${env.HOME || ''}/.smooth/daemon.addr`;
    try {
        const fs = await import('node:fs/promises');
        return (await fs.readFile(file, 'utf8')).trim();
    } catch {
        return '';
    }
};

/** Fire-and-forget POST; resolves when sent (or given up), never rejects. */
export const postFlow = async (body, env = process.env, fetchImpl = globalThis.fetch) => {
    try {
        const url = flowHooksUrl(await readAddr(env));
        if (!url || typeof fetchImpl !== 'function') return;
        await fetchImpl(url, {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify(body),
            signal: AbortSignal.timeout(FLOW_TIMEOUT_MS),
        }).catch(() => {});
    } catch {
        // SmoothFlow state must never affect the Pi run.
    }
};

export default function (pi) {
    const cwd = process.cwd();
    for (const event of FLOW_EVENTS) {
        pi.on(event, (ev, ctx) => {
            void postFlow(flowBody(event, ev, ctx, process.env, cwd));
        });
    }
}
