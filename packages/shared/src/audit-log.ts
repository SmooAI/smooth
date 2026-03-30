/** Audit logger — human-readable, rotating log files for tool usage
 *
 * Writes append-only log entries in a readable format:
 *
 *   [2026-03-30T17:45:12.345Z] operator-abc3f | tool_call | beads_context
 *     input: {"beadId":"bead-123"}
 *     output: {"title":"Fix auth bug","status":"open"}
 *     duration: 245ms
 *
 * Logs rotate daily and are kept for 30 days by default.
 * Each actor (leader, operator-xyz) gets its own log file.
 *
 * Files live at ~/.smooth/audit/
 *   leader.log, leader.log.2026-03-29, leader.log.2026-03-28, ...
 *   operator-abc3f.log, operator-abc3f.log.2026-03-29, ...
 */

import { existsSync, mkdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { createStream, type RotatingFileStream } from 'rotating-file-stream';

const AUDIT_DIR = process.env.SMOOTH_AUDIT_DIR ?? join(homedir(), '.smooth', 'audit');

const streams = new Map<string, RotatingFileStream>();

function ensureAuditDir(): void {
    if (!existsSync(AUDIT_DIR)) {
        mkdirSync(AUDIT_DIR, { recursive: true });
    }
}

function getStream(actor: string): RotatingFileStream {
    let stream = streams.get(actor);
    if (stream) return stream;

    ensureAuditDir();

    const sanitized = actor.replace(/[^a-zA-Z0-9_-]/g, '_');
    stream = createStream(`${sanitized}.log`, {
        path: AUDIT_DIR,
        size: '10M',
        interval: '1d',
        maxFiles: 30,
        compress: false, // Keep human-readable
    });

    streams.set(actor, stream);
    return stream;
}

export type AuditAction =
    | 'tool_call'
    | 'tool_result'
    | 'prompt_sent'
    | 'prompt_received'
    | 'sandbox_created'
    | 'sandbox_destroyed'
    | 'phase_started'
    | 'phase_completed'
    | 'review_requested'
    | 'review_verdict'
    | 'bead_created'
    | 'bead_updated'
    | 'message_sent'
    | 'error';

export interface AuditEntry {
    actor: string;
    action: AuditAction;
    target?: string;
    beadId?: string;
    input?: unknown;
    output?: unknown;
    durationMs?: number;
    error?: string;
    metadata?: Record<string, unknown>;
}

function formatValue(value: unknown, _indent = 4): string {
    if (value === undefined || value === null) return '';
    const str = typeof value === 'string' ? value : JSON.stringify(value);
    if (str.length <= 200) return str;
    // Truncate long values but show enough to be useful
    return str.slice(0, 200) + `... (${str.length} chars)`;
}

function formatEntry(entry: AuditEntry): string {
    const ts = new Date().toISOString();
    const lines: string[] = [];

    lines.push(`[${ts}] ${entry.actor} | ${entry.action}${entry.target ? ` | ${entry.target}` : ''}`);

    if (entry.beadId) {
        lines.push(`    bead: ${entry.beadId}`);
    }
    if (entry.input !== undefined) {
        lines.push(`    input: ${formatValue(entry.input)}`);
    }
    if (entry.output !== undefined) {
        lines.push(`    output: ${formatValue(entry.output)}`);
    }
    if (entry.durationMs !== undefined) {
        lines.push(`    duration: ${entry.durationMs}ms`);
    }
    if (entry.error) {
        lines.push(`    error: ${entry.error}`);
    }
    if (entry.metadata && Object.keys(entry.metadata).length > 0) {
        for (const [key, val] of Object.entries(entry.metadata)) {
            lines.push(`    ${key}: ${formatValue(val)}`);
        }
    }

    return lines.join('\n') + '\n';
}

/** Write an audit log entry. Non-blocking, fire-and-forget. */
export function audit(entry: AuditEntry): void {
    const stream = getStream(entry.actor);
    stream.write(formatEntry(entry));
}

/** Create a scoped audit logger for a specific actor */
export function createAuditLogger(actor: string, defaultBeadId?: string) {
    return {
        toolCall(tool: string, input: unknown, output?: unknown, durationMs?: number) {
            audit({ actor, action: 'tool_call', target: tool, beadId: defaultBeadId, input, output, durationMs });
        },
        promptSent(sessionId: string, text: string) {
            audit({ actor, action: 'prompt_sent', target: sessionId, beadId: defaultBeadId, input: text });
        },
        promptReceived(sessionId: string, output: unknown, durationMs?: number) {
            audit({ actor, action: 'prompt_received', target: sessionId, beadId: defaultBeadId, output, durationMs });
        },
        phaseStarted(phase: string, beadId?: string) {
            audit({ actor, action: 'phase_started', target: phase, beadId: beadId ?? defaultBeadId });
        },
        phaseCompleted(phase: string, beadId?: string, durationMs?: number) {
            audit({ actor, action: 'phase_completed', target: phase, beadId: beadId ?? defaultBeadId, durationMs });
        },
        sandboxCreated(sandboxId: string, metadata?: Record<string, unknown>) {
            audit({ actor, action: 'sandbox_created', target: sandboxId, beadId: defaultBeadId, metadata });
        },
        sandboxDestroyed(sandboxId: string) {
            audit({ actor, action: 'sandbox_destroyed', target: sandboxId, beadId: defaultBeadId });
        },
        reviewRequested(beadId: string) {
            audit({ actor, action: 'review_requested', beadId });
        },
        reviewVerdict(beadId: string, verdict: string, metadata?: Record<string, unknown>) {
            audit({ actor, action: 'review_verdict', beadId, output: verdict, metadata });
        },
        beadCreated(beadId: string, title: string) {
            audit({ actor, action: 'bead_created', beadId, metadata: { title } });
        },
        beadUpdated(beadId: string, updates: Record<string, unknown>) {
            audit({ actor, action: 'bead_updated', beadId, metadata: updates });
        },
        messageSent(beadId: string, direction: string, content: string) {
            audit({ actor, action: 'message_sent', beadId, metadata: { direction }, output: content });
        },
        error(message: string, metadata?: Record<string, unknown>) {
            audit({ actor, action: 'error', beadId: defaultBeadId, error: message, metadata });
        },
    };
}

/** Flush all open streams. Call on shutdown. */
export async function flushAuditLogs(): Promise<void> {
    const promises: Promise<void>[] = [];
    for (const [, stream] of streams) {
        promises.push(new Promise((resolve) => stream.end(resolve)));
    }
    await Promise.all(promises);
    streams.clear();
}

/** Get the audit log directory path */
export function getAuditDir(): string {
    return AUDIT_DIR;
}
