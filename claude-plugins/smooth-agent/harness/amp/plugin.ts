// smooth-agent Amp plugin — SmoothFlow state (pearl th-b00115).
//
// Amp has no command hooks; it runs TypeScript plugins (under Bun) from
// ~/.config/amp/plugins/*.ts. `th pkg install` links this file there as
// <pkg>.ts. Each lifecycle event is posted to the daemon's flow engine in the
// same envelope flow-hook.sh sends for Claude Code:
//
//   POST http://<daemon.addr>/api/flow/hooks
//        {harness:"amp", event, session_id:<thread id>, cwd, payload, flow_id?}
//
// `event` is Amp's own name; the `amp` harness manifest's event_map reads it
// (agent.start / tool.result → working, agent.end → idle). flow_id is
// SMOOTH_FLOW_ID, which the engine exports into every pane it launches, so the
// thread binds to its flow row without guessing by cwd.
//
// Deliberately NOT subscribed: tool.call. Its handler must answer
// allow / reject / modify, and answering `allow` would step around Amp's own
// permission rules. Every handler here returns a neutral value.
//
// Contract, same as every flow hook: never block, never throw into Amp.
// No daemon.addr / daemon down / fetch missing → silent no-op. 2 s cap,
// fire-and-forget. Plain JS syntax (valid TypeScript) so node can test it.

export const HARNESS = 'amp';
export const FLOW_EVENTS = ['session.start', 'agent.start', 'tool.result', 'agent.end'];
const FLOW_TIMEOUT_MS = 2000;

/** `http://<addr>/api/flow/hooks`, or '' when nothing is advertised. */
export const flowHooksUrl = (addr) => {
    const a = String(addr || '').trim();
    if (!a) return '';
    return `${/^https?:\/\//.test(a) ? a : `http://${a}`}/api/flow/hooks`;
};

/** The hook envelope for one Amp event. Pure. */
export const flowBody = (event, ev, env = {}, cwd = '') => {
    const e = ev && typeof ev === 'object' ? ev : {};
    const payload = { hook_event_name: event, thread_id: e.thread?.id ?? '' };
    if (typeof e.message === 'string') payload.message = e.message.slice(0, 4000);
    if (typeof e.status === 'string') payload.status = e.status;
    if (typeof e.tool === 'string') payload.tool_name = e.tool;
    const body = { harness: HARNESS, event, session_id: e.thread?.id ?? '', cwd, payload };
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

/**
 * This launch's SmoothFlow hook token (th-91d032): the 0600 file the engine
 * names in SMOOTH_FLOW_HOOK_TOKEN_FILE, hex only. '' when SmoothFlow did not
 * launch this process; the engine then treats the hook as unauthenticated.
 */
export const flowHookToken = async (env) => {
    const file = env.SMOOTH_FLOW_HOOK_TOKEN_FILE || '';
    if (!file) return '';
    try {
        const fs = await import('node:fs/promises');
        return (await fs.readFile(file, 'utf8')).replace(/[^0-9a-fA-F]/g, '').slice(0, 128);
    } catch {
        return '';
    }
};

/** Fire-and-forget POST; resolves when sent (or given up), never rejects. */
export const postFlow = async (body, env = process.env, fetchImpl = globalThis.fetch) => {
    try {
        const url = flowHooksUrl(await readAddr(env));
        if (!url || typeof fetchImpl !== 'function') return;
        const headers = { 'content-type': 'application/json' };
        const token = await flowHookToken(env);
        if (token) headers['x-smooth-flow-hook-token'] = token;
        await fetchImpl(url, {
            method: 'POST',
            headers,
            body: JSON.stringify(body),
            signal: AbortSignal.timeout(FLOW_TIMEOUT_MS),
        }).catch(() => {});
    } catch {
        // SmoothFlow state must never affect the Amp run.
    }
};

export const description = 'Reports Amp thread state (working / idle) to SmoothFlow';

export default function (amp) {
    const cwd = process.cwd();
    for (const event of FLOW_EVENTS) {
        amp.on(event, (ev) => {
            // Not awaited: the post rides alongside the turn, never in front of it.
            void postFlow(flowBody(event, ev, process.env, cwd));
            // agent.start's result is an object (no message); the rest are void.
            return event === 'agent.start' ? {} : undefined;
        });
    }
}
