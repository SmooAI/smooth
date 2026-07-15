// useMentionSearch — the `@`-mention data hook for the composer. Mirrors the
// `th code` TUI's `@` picker (crates/smooth-code/src/autocomplete.rs): the user
// types `@<query>` and we surface workspace files, filesystem paths, and pearls
// to drop in as a reference. EPIC th-c89c2a (th-58b5fe).
//
// The search itself is delegated to Big Smooth's ungated `GET {api}/search?q=…`
// endpoint (a backend agent owns that route). We debounce keystrokes, reuse the
// same API base + token the operator WS client resolved, and degrade silently to
// no results on any fetch error so a flaky/absent endpoint never traps the user
// inside a popup.

import { useEffect, useRef, useState } from 'react';

import { resolveTarget } from './operator';

/** One `@`-mention suggestion, matching the `/search` response contract. */
export interface MentionResult {
    /** Distinguishes the glyph + grouping: a workspace file, a filesystem path, or a pearl. */
    kind: 'file' | 'path' | 'pearl';
    /** The literal text inserted in place of the `@<query>` token (carries its own `@`). */
    value: string;
    /** Primary display line in the popup. */
    label: string;
    /** Optional secondary line (relative path, pearl title). */
    detail?: string;
}

/** Shape of `GET /search` — defensive: anything off-contract degrades to no results. */
interface SearchResponse {
    results?: MentionResult[];
}

/**
 * Debounced `@`-mention search. Pass the active query string (the chars after
 * `@` up to the caret) to fetch matches, or `null` when no `@` token is active
 * to clear results and skip the network entirely. An empty-string query is a
 * valid "just typed `@`" search and asks the endpoint for its top results.
 */
export function useMentionSearch(query: string | null): MentionResult[] {
    const [results, setResults] = useState<MentionResult[]>([]);
    // Resolve the API base + token once — the same target the WS client uses.
    const targetRef = useRef(resolveTarget());

    useEffect(() => {
        if (query === null) {
            setResults([]);
            return;
        }

        let aborted = false;
        const controller = new AbortController();
        const handle = setTimeout(() => {
            const { http, token } = targetRef.current;
            fetch(`${http}/search?q=${encodeURIComponent(query)}`, {
                headers: token ? { authorization: `Bearer ${token}` } : {},
                signal: controller.signal,
            })
                .then((r) => (r.ok ? (r.json() as Promise<SearchResponse>) : null))
                .then((data) => {
                    if (!aborted) setResults(Array.isArray(data?.results) ? data!.results! : []);
                })
                .catch(() => {
                    // AbortError or network failure — degrade silently.
                    if (!aborted) setResults([]);
                });
        }, 150);

        return () => {
            aborted = true;
            controller.abort();
            clearTimeout(handle);
        };
    }, [query]);

    return results;
}

/** An active `@` token under the caret: where it starts/ends and its query. */
export interface ActiveMention {
    /** Index of the `@` character in the input. */
    start: number;
    /** Index just past the token (first whitespace after `@`, or end of input). */
    tokenEnd: number;
    /** The chars between `@` and the caret — what we search for. */
    query: string;
}

/**
 * Find the `@` token the caret currently sits inside, if any. Matches the TUI's
 * trigger rule: the `@` must be at the start of input or immediately preceded by
 * whitespace, and there must be no whitespace between it and the caret. Returns
 * `null` when the caret isn't inside such a token.
 */
export function activeMention(text: string, caret: number): ActiveMention | null {
    // Scan back from the caret to the nearest `@`, bailing if we hit whitespace
    // first (the caret isn't in a contiguous `@…` token).
    let i = caret - 1;
    while (i >= 0) {
        const ch = text[i];
        if (ch === '@') {
            const prev = i > 0 ? text[i - 1] : '';
            // `@` must open a token: start-of-input or whitespace before it.
            if (i !== 0 && !/\s/.test(prev)) return null;
            // The whole token, up to the next whitespace (or end), is replaced on select.
            let end = i + 1;
            while (end < text.length && !/\s/.test(text[end])) end++;
            return { start: i, tokenEnd: end, query: text.slice(i + 1, caret) };
        }
        if (/\s/.test(ch)) return null;
        i--;
    }
    return null;
}
