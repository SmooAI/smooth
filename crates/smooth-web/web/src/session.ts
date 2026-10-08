/** Session binding across reconnects, and the wire text of a send (SMOODEV-3708).
 *
 * The desktop chat holds one operator session per WebSocket. When the socket
 * dropped and came back, `onopen` used to open a session with NO
 * `conversationId`; the server answered with a brand-new conversation, the reply
 * overwrote the active id, and the next message silently landed in a fresh chat
 * while the transcript on screen still showed the old one. A reconnect must
 * re-bind the conversation that is on screen and reload its history (the turn
 * may have finished server-side while we were gone).
 *
 * Pure, imports nothing at runtime, so `pnpm test` runs it directly.
 */

/** What goes on the wire when the user sends only an image. The engine rejects
 * an empty `message`, so an image-only send needs SOME text. */
export const IMAGE_ONLY_PLACEHOLDER = '(image attached)';

/** The `create_conversation_session` frame. With a `conversationId` it binds the
 * new session to that existing conversation (a resume); without one the server
 * starts a new conversation. */
export function sessionOpenFrame(requestId: string, agentId: string, conversationId: string | null | undefined): Record<string, unknown> {
    const frame: Record<string, unknown> = { action: 'create_conversation_session', requestId, agentId, userName: 'console' };
    if (conversationId) frame.conversationId = conversationId;
    return frame;
}

/** The `message` text to send: the typed text, or the placeholder when the user
 * attached something and typed nothing. Empty when there is nothing to send. */
export function outgoingText(body: string, attachmentCount: number): string {
    const text = body.trim();
    if (text) return text;
    return attachmentCount > 0 ? IMAGE_ONLY_PLACEHOLDER : '';
}

/** The text to SHOW for a history user turn: the placeholder we sent for an
 * image-only message is wire noise, not something the user typed. */
export function displayText(content: string, hasAttachments: boolean): string {
    return hasAttachments && content.trim() === IMAGE_ONLY_PLACEHOLDER ? '' : content;
}
