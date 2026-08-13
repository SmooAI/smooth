// The `todos` directive — the agent's live checklist. Kept in its own
// import-free module so the wire-shape tolerance is testable on its own
// (`pnpm test`), the way `turn-guard` is.

/** One row of the agent's live checklist (the `todos` directive). */
export interface TodoItem {
    text: string;
    status: 'pending' | 'in_progress' | 'completed';
}

/** Normalize the raw `todos` directive items: drop anything without a string
 * `text`, and clamp `status` to the three known values (default 'pending'). */
export function normalizeTodos(items: unknown): TodoItem[] {
    if (!Array.isArray(items)) return [];
    const out: TodoItem[] = [];
    for (const raw of items) {
        const t = raw as { text?: unknown; status?: unknown };
        if (typeof t?.text !== 'string') continue;
        const status = t.status === 'in_progress' || t.status === 'completed' ? t.status : 'pending';
        out.push({ text: t.text, status });
    }
    return out;
}
