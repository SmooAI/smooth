//! The Cmd/Ctrl+N "new chat" chord matcher, split out from `main.ts` so it's
//! unit-tested without importing the Electron app bootstrap (`main.ts` runs the
//! app on import, which throws under a plain `node --test`).

/** True for the "new chat" chord — Cmd+N (macOS) or Ctrl+N (Windows/Linux) on
 * key-down, with no other chord modifier (so Cmd+Shift+N / Cmd+Alt+N don't
 * trigger it). Shaped to accept an Electron `Input`. */
export function isNewChatChord(input: { type: string; key: string; control: boolean; meta: boolean; alt: boolean; shift: boolean }): boolean {
    return input.type === 'keyDown' && input.key.toLowerCase() === 'n' && (input.meta || input.control) && !input.alt && !input.shift;
}
