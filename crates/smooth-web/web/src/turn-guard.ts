/** Whether the composer may dispatch a message right now.
 *
 * A message sent while a turn is already in flight does NOT queue behind it —
 * the daemon spawns a **second concurrent turn**, and the two come back
 * interleaved, so turn 1's answer lands under turn 2's prompt. That is the
 * root of the contradictory replies in pearl th-426791; the agentic bench
 * reproduces it with swapped responses.
 *
 * So `turnActive` blocks send outright. The escape hatch is Stop (th-3a912a),
 * which is exactly the affordance the composer already shows in Send's place.
 *
 * Pure and free of imports so the guard is testable on its own (`pnpm test`).
 */
export function canSend(opts: { text: string; attachments: number; disabled: boolean; turnActive: boolean }): boolean {
    if (opts.disabled || opts.turnActive) return false;
    return opts.text.trim().length > 0 || opts.attachments > 0;
}
