// The computer switcher (pearl th-a49e21): which of your Macs this window
// drives. "marvin — This computer" plus every other Big Smooth on your Smoo
// Relay (e.g. smoo-hub). Picking one remembers it and reloads the window onto
// that computer's Big Smooth, tunnelled by this computer's daemon — the
// conversations, `/cd`, Plan/Auto and Stats all become that computer's.
// Unreachable computers stay listed with the reason (signed out, relay down,
// asleep) instead of silently vanishing.

import { Check, ChevronDown, Laptop, RefreshCw, Server } from 'lucide-react';
import { useCallback, useEffect, useRef, useState } from 'react';

import {
    activeComputer,
    computerNotice,
    computerRows,
    fetchPeers,
    readRemembered,
    relayReason,
    writeRemembered,
    type ComputerRow,
    type PeersResponse,
} from './computers';
import { resolveLocalTarget } from './operator';

export function ComputerSwitcher() {
    const [open, setOpen] = useState(false);
    const [resp, setResp] = useState<PeersResponse | null>(null);
    const [loading, setLoading] = useState(true);
    const [switching, setSwitching] = useState(false);
    const box = useRef<HTMLDivElement>(null);
    const active = activeComputer();
    const notice = computerNotice();

    const refresh = useCallback(() => {
        const { http, token } = resolveLocalTarget();
        return fetchPeers(http, token).then((r) => {
            setResp(r);
            setLoading(false);
        });
    }, []);
    // Once on mount for this computer's name; again on every open and on
    // Refresh (who's online changes), from the event handlers below.
    useEffect(() => {
        void refresh();
    }, [refresh]);
    const load = () => {
        setLoading(true);
        void refresh();
    };
    const toggle = () => {
        if (!open) load();
        setOpen((o) => !o);
    };

    // Click-away closes the list.
    useEffect(() => {
        if (!open) return;
        const onDown = (e: MouseEvent) => {
            if (box.current && !box.current.contains(e.target as Node)) setOpen(false);
        };
        window.addEventListener('mousedown', onDown);
        return () => window.removeEventListener('mousedown', onDown);
    }, [open]);

    const rows = computerRows(resp, active, readRemembered(localStorage));
    const current = rows.find((r) => r.active) ?? rows[0];
    const reason = relayReason(resp);

    const pick = (row: ComputerRow) => {
        if (row.active || !row.online) return;
        setSwitching(true);
        writeRemembered(localStorage, row.device ? { device: row.device, label: row.name } : null);
        window.location.reload();
    };

    return (
        <div ref={box} className="relative mx-3 mb-2">
            <button
                type="button"
                onClick={toggle}
                aria-haspopup="listbox"
                aria-expanded={open}
                title="Which computer's Big Smooth this window drives"
                className={`flex w-full items-center gap-2.5 rounded-xl border px-3 py-2 text-left transition ${
                    active.kind === 'remote'
                        ? 'border-(--color-th-teal)/40 bg-(--color-th-teal)/10 hover:bg-(--color-th-teal)/15'
                        : 'border-border bg-panel/60 hover:bg-panel-2'
                }`}
            >
                {active.kind === 'remote' ? (
                    <Server size={16} className="shrink-0 text-(--color-th-teal)" />
                ) : (
                    <Laptop size={16} className="shrink-0 text-(--color-muted-foreground)" />
                )}
                <span className="min-w-0 flex-1">
                    <span className="block truncate text-sm font-medium text-foreground">{current.name}</span>
                    <span className="block truncate text-xs text-(--color-muted-foreground)">
                        {active.kind === 'remote' ? 'Remote · via Smoo Relay' : 'This computer'}
                    </span>
                </span>
                <ChevronDown size={14} className={`shrink-0 text-(--color-muted-foreground) transition ${open ? 'rotate-180' : ''}`} />
            </button>

            {notice && !open && <p className="mt-1.5 px-1 text-xs leading-snug text-amber">{notice}</p>}

            {open && (
                <div
                    role="listbox"
                    aria-label="Computers"
                    className="absolute inset-x-0 top-full z-50 mt-1.5 overflow-hidden rounded-xl border border-border bg-panel shadow-xl"
                >
                    <div className="max-h-72 overflow-y-auto p-1">
                        {rows.map((row) => (
                            <button
                                key={row.device ?? 'local'}
                                type="button"
                                role="option"
                                aria-selected={row.active}
                                disabled={switching || !row.online}
                                onClick={() => pick(row)}
                                className={`flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 text-left transition ${
                                    row.active ? 'bg-(--color-th-teal)/12' : row.online ? 'hover:bg-panel-2' : 'cursor-not-allowed opacity-60'
                                }`}
                            >
                                <span
                                    className={`size-2 shrink-0 rounded-full ${row.online ? 'bg-(--color-online)' : 'bg-(--color-muted-foreground)/40'}`}
                                    aria-label={row.online ? 'online' : 'unreachable'}
                                />
                                <span className="min-w-0 flex-1">
                                    <span className="block truncate text-sm text-foreground">{row.name}</span>
                                    <span className="block text-xs leading-snug text-(--color-muted-foreground)">{row.detail}</span>
                                </span>
                                {row.active && <Check size={14} className="shrink-0 text-(--color-th-teal)" />}
                            </button>
                        ))}
                    </div>
                    <div className="flex items-start gap-2 border-t border-border px-3 py-2">
                        <p className="min-w-0 flex-1 text-xs leading-snug text-(--color-muted-foreground)">
                            {switching
                                ? 'Switching…'
                                : loading
                                  ? 'Looking for your other computers…'
                                  : (reason ?? (rows.length > 1 ? 'Your computers on the Smoo Relay.' : 'No other Big Smooth computers are online.'))}
                        </p>
                        <button
                            type="button"
                            onClick={load}
                            aria-label="Refresh"
                            title="Refresh"
                            className="shrink-0 text-(--color-muted-foreground) transition hover:text-foreground"
                        >
                            <RefreshCw size={13} className={loading ? 'animate-spin' : ''} />
                        </button>
                    </div>
                </div>
            )}
        </div>
    );
}
