/** The modifier state of a composer keydown, as the textarea sees it. */
export interface EnterKey {
    key: string;
    shiftKey: boolean;
    altKey: boolean;
    metaKey: boolean;
    ctrlKey: boolean;
    /** True while an IME is composing; its Enter confirms the candidate. */
    isComposing?: boolean;
}

/**
 * What Enter means in the Big Smooth composer (SMOODEV-4147): Shift+Enter and
 * Alt/Option+Enter break the line, ⌘/Ctrl+Enter steers a running turn, plain
 * Enter sends. `null` means the key isn't ours (not Enter, or an IME Enter).
 */
export function enterAction(e: EnterKey): 'newline' | 'send' | 'steer' | null {
    if (e.key !== 'Enter' || e.isComposing) return null;
    if (e.shiftKey || e.altKey) return 'newline';
    return e.metaKey || e.ctrlKey ? 'steer' : 'send';
}

/** Insert a line break over the selection `[start, end)`; returns the new text and caret. */
export function insertNewline(text: string, start: number, end: number): { text: string; caret: number } {
    return { text: text.slice(0, start) + '\n' + text.slice(end), caret: start + 1 };
}
