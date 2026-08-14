// Decide whether a just-arrived assistant reply deserves a native notification
// (th-6af4b1). The Electron desktop can't receive Web Push, so when a reply
// finishes while the window is unfocused we relay it to the native bridge. Pure
// so it's unit-testable without a DOM — the caller supplies focus + the id it
// last notified for dedupe.

import type { ChatMessage } from './operator';

export interface NotifyDecision {
    id: string;
    title: string;
    body: string;
}

type Reply = Pick<ChatMessage, 'id' | 'role' | 'content' | 'streaming'>;

const BODY_MAX = 160;

/** The newest completed assistant reply worth notifying, or `null`. Returns
 * `null` when focused, when the tail isn't a finished non-empty assistant reply,
 * or when that reply was already notified. */
export function replyToNotify(messages: Reply[], opts: { focused: boolean; lastNotifiedId: string | null }): NotifyDecision | null {
    if (opts.focused) return null;
    const last = messages[messages.length - 1];
    if (!last || last.role !== 'assistant' || last.streaming) return null;
    const body = last.content.trim();
    if (!body || last.id === opts.lastNotifiedId) return null;
    return {
        id: last.id,
        title: 'Big Smooth',
        body: body.length > BODY_MAX ? `${body.slice(0, BODY_MAX - 1)}…` : body,
    };
}
