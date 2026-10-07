// Pure parsing of server conversation history into the shapes the UI renders.
// Kept in its own module (no runtime imports) so it's unit-testable under node
// — operator.ts uses Vite-only extensionless imports and can't be loaded by the
// test runner. th-1fca98.

import type { Attachment } from './operator';

/** A raw history message from `get_conversation_messages`. The server returns
 * the stored domain `Message`: `direction` ('inbound' = user, 'outbound' = agent)
 * + `content: { items: [{ type:'text', text } | { type:'image', url }] }`.
 * Fallbacks (`role`, string content, `text`) tolerate other shapes. Past tool
 * calls are NOT reconstructed; only the text + images of each turn are rendered. */
export interface HistoryContentItem {
    type?: string;
    text?: string;
    /** Set on `type:'image'` items — a `data:`/`https` image URL the engine now
     * persists on the user turn (th-1fca98), so history re-renders images another
     * client attached. */
    url?: string;
}

export interface HistoryMessage {
    direction?: string;
    content?: { items?: HistoryContentItem[] } | string;
    role?: string;
    text?: string;
    /** ISO-8601 send time — used to render history oldest-first (the server
     * returns newest-first). */
    createdAt?: string;
    /** Free-form metadata. A failed turn's record carries `turnError`
     * (`{code, requestId}`), see {@link historyTurnError}. */
    metadataJson?: { turnError?: { code?: string; requestId?: string } } | null;
}

/** Flatten a history message's content to text (real shape = content.items[] of
 * text parts; tolerate a bare string or a `text` alias). */
export function historyText(m: HistoryMessage): string {
    const c = m.content;
    if (c && typeof c === 'object' && Array.isArray(c.items)) {
        return c.items
            .filter((i) => (i.type ?? 'text') === 'text')
            .map((i) => i.text ?? '')
            .join('');
    }
    if (typeof c === 'string') return c;
    return m.text ?? '';
}

/** The images persisted on a history message (`content.items[]` of `type:'image'`),
 * as renderable attachments. Empty for text-only turns and old daemons that never
 * stored images. mime comes from a `data:` URL prefix; an `https` URL defaults to
 * `image/*` (these items are always images). th-1fca98. */
export function historyImages(m: HistoryMessage): Attachment[] {
    const c = m.content;
    if (!c || typeof c !== 'object' || !Array.isArray(c.items)) return [];
    const out: Attachment[] = [];
    for (const i of c.items) {
        if (i.type === 'image' && i.url) {
            const mime = i.url.match(/^data:([^;,]+)/)?.[1] ?? 'image/*';
            out.push({ name: 'image', mime, dataUrl: i.url });
        }
    }
    return out;
}

/** A failed turn the server recorded in the conversation (SMOODEV-3705): an
 * outbound message whose `metadataJson.turnError` is `{code, requestId}`. A
 * client that dropped mid-turn missed the live `error` frame; this record is how
 * its reload learns the turn failed, so it renders as an error card rather than
 * as a normal reply. Null for every ordinary message. */
export function historyTurnError(m: HistoryMessage): { code: string; requestId?: string } | null {
    const te = m.metadataJson?.turnError;
    if (!te || typeof te !== 'object') return null;
    return { code: typeof te.code === 'string' && te.code ? te.code : 'TURN_ERROR', requestId: te.requestId };
}
