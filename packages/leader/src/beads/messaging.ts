/** Beads-backed message bus — all durable communication via bead comments */

import type { MessageDirection } from '@smooth/shared/message-types';
import { MESSAGE_PREFIXES } from '@smooth/shared/message-types';
import type { InboxItem, Message } from '@smooth/shared/message-types';

import { addComment, getComments, listBeads } from './client.js';

/** Send a message on a bead (persisted as a prefixed comment) */
export async function sendMessage(beadId: string, direction: MessageDirection, content: string, author: string = 'leader'): Promise<void> {
    const prefix = MESSAGE_PREFIXES[direction];
    await addComment(beadId, `${prefix} ${content}`, author);
}

/** Read messages from a bead, optionally filtered by direction */
export async function readMessages(beadId: string, direction?: MessageDirection): Promise<Message[]> {
    const comments = await getComments(beadId);
    const messages: Message[] = [];

    for (const comment of comments) {
        const parsed = parseMessageDirection(comment.content);
        if (direction && parsed.direction !== direction) continue;

        messages.push({
            id: comment.id,
            beadId: comment.beadId,
            direction: parsed.direction,
            content: parsed.content,
            author: comment.author,
            createdAt: comment.createdAt,
        });
    }

    return messages;
}

/** Get inbox items requiring human attention */
export async function getInbox(): Promise<InboxItem[]> {
    // Find beads with messages directed at humans
    const activeBeads = await listBeads({ status: 'in_progress' });
    const inbox: InboxItem[] = [];

    for (const bead of activeBeads) {
        const messages = await readMessages(bead.id, 'leader→human');
        for (const msg of messages) {
            inbox.push({
                message: msg,
                beadTitle: bead.title,
                requiresAction: true,
                actionType: inferActionType(msg.content),
            });
        }
    }

    return inbox.sort((a, b) => new Date(b.message.createdAt).getTime() - new Date(a.message.createdAt).getTime());
}

/** Append a progress update to a bead */
export async function appendProgress(beadId: string, content: string, author: string = 'worker'): Promise<void> {
    await sendMessage(beadId, 'progress', content, author);
}

/** Record an artifact reference on a bead */
export async function recordArtifact(beadId: string, type: string, path: string, author: string = 'worker'): Promise<void> {
    await sendMessage(beadId, 'artifact', `[${type}] ${path}`, author);
}

// ── Helpers ─────────────────────────────────────────────────

function parseMessageDirection(content: string): { direction: MessageDirection; content: string } {
    for (const [direction, prefix] of Object.entries(MESSAGE_PREFIXES)) {
        if (content.startsWith(prefix)) {
            return {
                direction: direction as MessageDirection,
                content: content.slice(prefix.length).trim(),
            };
        }
    }
    // Default: treat unprefixed comments as progress
    return { direction: 'progress', content };
}

function inferActionType(content: string): InboxItem['actionType'] {
    const lower = content.toLowerCase();
    if (lower.includes('approve') || lower.includes('approval')) return 'approval';
    if (lower.includes('review')) return 'review';
    if (lower.includes('question') || lower.includes('?')) return 'response';
    return 'info';
}
