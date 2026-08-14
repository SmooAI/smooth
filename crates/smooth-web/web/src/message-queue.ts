import type { Attachment } from './operator';

/** Client-side message queue (pearl th-0a079c).
 *
 * Sending while a turn is in flight used to be blocked outright: a second
 * concurrent turn interleaves with the first and the two replies come back
 * swapped (th-426791, the old `turn-guard`). Instead of swallowing the
 * keystroke, we hold the message and send it once the active turn's terminal
 * `eventual_response` lands — one at a time, in order. v1 is purely client-side
 * (React state; a refresh clears it, which is fine).
 *
 * These functions are pure and import only a type, so the whole queue behaviour
 * is testable on its own (`pnpm test`).
 */
export interface QueuedMessage {
    id: string;
    text: string;
    attachments: Attachment[];
}

/** What a composer submit does right now.
 *  - no content (empty draft / disabled) → `noop`
 *  - a turn is in flight → `enqueue` behind it
 *  - idle → `send` immediately (unchanged from today) */
export function submitAction(turnActive: boolean, hasContent: boolean): 'send' | 'enqueue' | 'noop' {
    if (!hasContent) return 'noop';
    return turnActive ? 'enqueue' : 'send';
}

/** Append to the tail — messages send in the order they were queued. */
export function enqueue(queue: QueuedMessage[], item: QueuedMessage): QueuedMessage[] {
    return [...queue, item];
}

/** Pull the head off for sending on turn completion; `null` when empty. */
export function dequeue(queue: QueuedMessage[]): { head: QueuedMessage; rest: QueuedMessage[] } | null {
    if (queue.length === 0) return null;
    const [head, ...rest] = queue;
    return { head, rest };
}

/** Drop one queued message by index (the chip's × button). */
export function removeAt(queue: QueuedMessage[], i: number): QueuedMessage[] {
    return queue.filter((_, j) => j !== i);
}

/** A short label for a queued chip — the text, or an attachment count when text-only. */
export function queuedLabel(m: QueuedMessage): string {
    const body = m.text.trim();
    if (body) return body;
    const n = m.attachments.length;
    return n ? `${n} attachment${n === 1 ? '' : 's'}` : '(empty)';
}
