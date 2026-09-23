/** Turn control: Stop, and Steer (pearl th-74ba1f).
 *
 * Every Big Smooth client used to stop a turn with `{action:'interrupt'}`. The
 * engine has no such action. It answered `UNSUPPORTED_ACTION`, and the turn ran to
 * completion, which on 2026-09-23 meant a second iMessage went out after the user
 * pressed Stop. Worse, the SPA treated that error frame as the end of the turn, so
 * the UI and the server disagreed about whether anything was running.
 *
 * The engine's contract (smooth-operator-server `server.rs`, `handle_text_frame`):
 *
 * - `{action:'cancel', requestId, sessionId}` aborts the connection's running turn
 *   and emits `{type:'cancelled', requestId:<the TURN's requestId>, status:499}`.
 *   That event REPLACES the turn's `eventual_response`, and the server drops every
 *   later frame carrying that requestId.
 * - A cancel with no running turn is a silent no-op. Nothing comes back. That is
 *   why a client needs a fallback timer rather than waiting forever.
 * - A `send_message` while a turn runs is rejected with `TURN_IN_PROGRESS`. So a
 *   follow-up must wait for the turn to be really over, which is what Steer does.
 *
 * Pure, imports nothing at runtime, so `pnpm test` runs it directly.
 */

/** How long to wait for `cancelled` before ending the turn locally. The engine
 * answers a cancel immediately, so a missing reply means it had no turn running
 * (it already finished, or the socket dropped and the engine aborted it). */
export const CANCEL_FALLBACK_MS = 10_000;

/** The frame that stops the running turn. `requestId` is the turn's own
 * `send_message` requestId. The engine echoes the turn's id regardless, but
 * sending it keeps the frame self-describing in logs. */
export function cancelFrame(turnRequestId: string | null, sessionId: string | null): Record<string, unknown> {
    const frame: Record<string, unknown> = { action: 'cancel' };
    if (turnRequestId) frame.requestId = turnRequestId;
    if (sessionId) frame.sessionId = sessionId;
    return frame;
}

/** A server event, as far as turn control cares. */
export interface TurnEvent {
    type?: string;
    requestId?: string;
}

/** The three events that end a turn. */
const TERMINAL = new Set(['eventual_response', 'cancelled', 'error']);

/** Does `ev` end the turn whose `send_message` requestId is `turnRequestId`?
 *
 * Only when it is terminal AND it is about THIS turn. An event with no requestId
 * is connection-level, so it counts. An error carrying some OTHER request's id,
 * such as the old `UNSUPPORTED_ACTION` for an `interrupt`, or a `TURN_IN_PROGRESS`
 * rejecting a different send, does NOT end the running turn. Treating it as
 * terminal is exactly how the SPA lost track of a turn that was still sending
 * iMessages. */
export function endsTurn(ev: TurnEvent, turnRequestId: string | null): boolean {
    if (!ev.type || !TERMINAL.has(ev.type)) return false;
    if (!turnRequestId || !ev.requestId) return true;
    return ev.requestId === turnRequestId;
}

/** A late frame from a turn we already cancelled. The engine's writer drops these
 * itself. This is the client's belt to that brace, so a straggler can never open a
 * new streaming bubble under the next turn. */
export function isStaleFrame(ev: TurnEvent, cancelledRequestId: string | null): boolean {
    return !!cancelledRequestId && ev.requestId === cancelledRequestId && ev.type !== 'cancelled';
}

/** A message waiting to go out once the running turn stops. `attachments` is kept
 * generic so this module stays free of the operator's types. */
export interface PendingSteer<A> {
    text: string;
    attachments: A[];
}

/** Fold a second Steer into one already waiting, so nothing typed is lost: texts
 * join with a blank line, attachments concatenate. */
export function mergeSteer<A>(prev: PendingSteer<A> | null, next: PendingSteer<A>): PendingSteer<A> {
    if (!prev) return next;
    return {
        text: [prev.text, next.text].filter((t) => t.trim()).join('\n\n'),
        attachments: [...prev.attachments, ...next.attachments],
    };
}

/** The human text of an `error` frame. The engine puts it at `error.message` (and
 * `data.error.message`), not at a top-level `message`. Reading only `message` is
 * why every error rendered as a bare "operator error". */
export function errorText(ev: {
    message?: string;
    error?: { message?: string; code?: string };
    data?: { message?: string; error?: { message?: string } };
}): string {
    return ev.error?.message ?? ev.data?.error?.message ?? ev.message ?? ev.data?.message ?? 'operator error';
}
