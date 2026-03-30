'use client';

import { useEffect, useRef, useState } from 'react';

import { api } from '@/lib/api';

interface SearchResult {
    type: 'bead' | 'file' | 'path';
    id: string;
    label: string;
    detail?: string;
}

interface AtAutocompleteProps {
    input: string;
    cursorPosition: number;
    onSelect: (result: SearchResult) => void;
}

export function AtAutocomplete({ input, cursorPosition, onSelect }: AtAutocompleteProps) {
    const [results, setResults] = useState<SearchResult[]>([]);
    const [selected, setSelected] = useState(0);
    const [loading, setLoading] = useState(false);
    const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

    // Extract the @query from the input at cursor position
    const atMatch = extractAtQuery(input, cursorPosition);
    const queryStr = atMatch?.query ?? '';

    useEffect(() => {
        if (!queryStr) {
            setResults([]);
            return;
        }

        if (debounceRef.current) clearTimeout(debounceRef.current);
        debounceRef.current = setTimeout(async () => {
            setLoading(true);
            try {
                const res = await api<{ data: SearchResult[] }>(`/api/search?q=${encodeURIComponent(queryStr)}&type=all`);
                setResults(res.data);
                setSelected(0);
            } catch {
                setResults([]);
            } finally {
                setLoading(false);
            }
        }, 150);

        return () => {
            if (debounceRef.current) clearTimeout(debounceRef.current);
        };
    }, [queryStr]);

    if (!atMatch || (results.length === 0 && !loading)) return null;

    return (
        <div className="absolute bottom-full mb-1 left-0 right-0 bg-neutral-900 border border-neutral-700 rounded-lg shadow-lg max-h-60 overflow-y-auto z-50">
            {loading && results.length === 0 && <div className="px-3 py-2 text-neutral-500 text-sm">Searching...</div>}
            {results.map((r, i) => {
                const typeIcon = r.type === 'bead' ? '◉' : r.type === 'file' ? '📄' : '📁';
                const typeColor = r.type === 'bead' ? 'text-cyan-400' : r.type === 'file' ? 'text-green-400' : 'text-yellow-400';

                return (
                    <button
                        key={`${r.type}-${r.id}`}
                        type="button"
                        className={`w-full text-left px-3 py-2 flex items-center gap-2 text-sm cursor-pointer transition-colors ${i === selected ? 'bg-neutral-800' : 'hover:bg-neutral-800/50'}`}
                        onClick={() => onSelect(r)}
                        onMouseEnter={() => setSelected(i)}
                    >
                        <span className={typeColor}>{typeIcon}</span>
                        <span className="flex-1 truncate">{r.label}</span>
                        {r.detail && <span className="text-neutral-600 text-xs">{r.detail}</span>}
                    </button>
                );
            })}
        </div>
    );
}

/** Extract @query from input text at cursor position */
function extractAtQuery(input: string, cursor: number): { query: string; start: number; end: number } | null {
    // Look backwards from cursor for @
    const before = input.slice(0, cursor);
    const atIndex = before.lastIndexOf('@');
    if (atIndex === -1) return null;

    // @ must be at start of word (preceded by space or start of string)
    if (atIndex > 0 && before[atIndex - 1] !== ' ') return null;

    const query = before.slice(atIndex + 1);

    // Don't trigger on empty @ or very short queries (except ~ and /)
    if (query.length === 0) return null;
    if (query.length < 2 && !query.startsWith('~') && !query.startsWith('/')) return null;

    return { query, start: atIndex, end: cursor };
}

/** Insert a search result into the input, replacing the @query */
export function insertAtResult(input: string, cursor: number, result: SearchResult): { newInput: string; newCursor: number } {
    const atMatch = extractAtQuery(input, cursor);
    if (!atMatch) return { newInput: input, newCursor: cursor };

    const before = input.slice(0, atMatch.start);
    const after = input.slice(atMatch.end);
    const insertion = `@${result.id} `;
    const newInput = before + insertion + after;
    const newCursor = before.length + insertion.length;

    return { newInput, newCursor };
}

export { type SearchResult };
