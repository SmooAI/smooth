//! Where the queue view's data comes from (SMOODEV-3371). Three sources, one
//! shape (`StatusResponse`):
//!
//! - `stream`: `th ci-queue web` pushes a snapshot a second as Server-Sent
//!   Events on `/api/events`; the first event carries the whole ten-minute
//!   sample window, later ones only what is new. EventSource reconnects by
//!   itself; the page says so while it does.
//! - `daemon`: Big Smooth's `GET /api/ci-queue/status`, polled each second
//!   while the tab is visible (`?since_ms=` keeps it to new samples).
//! - `?demo` (either host): the night replay from ci-queue-demo.ts, one
//!   simulated second per real second, opening mid-storm.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { flushSync } from 'react-dom';

import { appendSamples, diff, type Sample, type Snapshot, type StatusResponse } from './ci-queue';
import { NightReplay, PERIOD_S, phase } from './ci-queue-demo';
import { resolveTarget } from './operator';

const WINDOW_MS = 10 * 60_000;

/** Where the replay opens: 150s into the night, as load nears 1,000.
 * `?demo=<seconds>` opens elsewhere (`?demo=230` catches the gate opening). */
const DEMO_OPEN_S = 150;

function demoOpen(): number | null {
    const v = new URLSearchParams(window.location.search).get('demo');
    if (v == null) return null;
    const n = Number(v);
    return v !== '' && Number.isFinite(n) && n >= 0 ? n % PERIOD_S : DEMO_OPEN_S;
}

export type Source = 'stream' | 'daemon';

export interface Feed {
    snap: Snapshot | null;
    samples: Sample[];
    error: string | null;
    /** True while the stream is reconnecting (the last snapshot stays up). */
    stale: boolean;
    demo: { phase: string; t: number } | null;
    paused: boolean;
    setPaused: (p: boolean) => void;
    /** Bumps whenever a new snapshot lands, for layers that re-measure. */
    version: number;
}

function reducedMotion(): boolean {
    return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

/** Apply a state change inside a view transition when a job moved, so the
 * element keyed by its ticket animates between places. */
function withTransition(moved: boolean, apply: () => void): void {
    const doc = document as Document & { startViewTransition?: (cb: () => void) => unknown };
    if (moved && doc.startViewTransition && !reducedMotion() && document.visibilityState === 'visible') {
        doc.startViewTransition(() => flushSync(apply));
    } else {
        apply();
    }
}

export function useCiQueue(source: Source): Feed {
    const openAt = useMemo(() => demoOpen(), []);
    const isDemo = openAt != null;
    const [snap, setSnap] = useState<Snapshot | null>(null);
    const [samples, setSamples] = useState<Sample[]>([]);
    const [error, setError] = useState<string | null>(null);
    const [stale, setStale] = useState(false);
    const [paused, setPaused] = useState(false);
    const [version, setVersion] = useState(0);
    const [demo, setDemo] = useState<Feed['demo']>(null);
    const prev = useRef<Snapshot | null>(null);
    const pausedRef = useRef(paused);
    useEffect(() => {
        pausedRef.current = paused;
    }, [paused]);

    const accept = useCallback((next: Snapshot, add: Sample[]) => {
        if (pausedRef.current) return;
        const moved = diff(prev.current, next).some((e) => e.kind !== 'queued');
        prev.current = next;
        withTransition(moved, () => {
            setSnap(next);
            setSamples((s) => appendSamples(s, add, WINDOW_MS));
            setError(null);
            setStale(false);
            setVersion((v) => v + 1);
        });
    }, []);

    // The night replay.
    useEffect(() => {
        if (!isDemo) return;
        const night = new NightReplay();
        const warm: Sample[] = [];
        // Open at DEMO_OPEN_S into a loop, with at least ten minutes of
        // history behind it for the sparklines.
        const at = openAt ?? DEMO_OPEN_S;
        const steps = Math.ceil((600 - at) / PERIOD_S) * PERIOD_S + at;
        for (let i = 0; i < steps; i++) {
            night.step();
            if (i >= steps - 600 && i % 5 === 0) warm.push(night.sample());
        }
        const first = window.setTimeout(() => {
            accept(night.snapshot(), warm);
            setDemo({ phase: phase(night.t).name, t: night.t });
        }, 0);
        const id = window.setInterval(() => {
            if (document.visibilityState !== 'visible' || pausedRef.current) return;
            const next = night.step();
            accept(next, [night.sample()]);
            setDemo({ phase: phase(night.t).name, t: night.t });
        }, 1000);
        return () => {
            window.clearTimeout(first);
            window.clearInterval(id);
        };
    }, [isDemo, openAt, accept]);

    // `th ci-queue web`: Server-Sent Events.
    useEffect(() => {
        if (isDemo || source !== 'stream') return;
        const es = new EventSource('/api/events');
        es.addEventListener('snapshot', (e) => {
            const body = JSON.parse((e as MessageEvent<string>).data) as StatusResponse;
            if (body.snapshot?.unsupported) {
                setError(`No queue on this machine: ${body.snapshot.unsupported}`);
                return;
            }
            if (body.error || !body.snapshot) {
                setError(body.error ?? 'th sent no snapshot.');
                return;
            }
            accept(body.snapshot, body.samples);
        });
        es.onerror = () => setStale(true);
        return () => es.close();
    }, [isDemo, source, accept]);

    // Big Smooth: poll the daemon.
    useEffect(() => {
        if (isDemo || source !== 'daemon') return;
        let lastSample = 0;
        const tick = async () => {
            if (document.visibilityState !== 'visible' || pausedRef.current) return;
            const { http, token } = resolveTarget();
            try {
                const r = await fetch(`${http}/api/ci-queue/status?since_ms=${lastSample}`, {
                    headers: token ? { authorization: `Bearer ${token}` } : {},
                });
                if (!r.ok) {
                    setError(
                        r.status === 404
                            ? 'This Big Smooth is older than the Queue tab. Update it with `pnpm install:th` or the menu bar.'
                            : `The daemon answered ${r.status}.`,
                    );
                    return;
                }
                const body = (await r.json()) as StatusResponse;
                if (body.samples.length) lastSample = body.samples[body.samples.length - 1].t_ms;
                if (body.snapshot?.unsupported) {
                    setError(`No queue on this machine: ${body.snapshot.unsupported}`);
                    return;
                }
                if (body.error || !body.snapshot) {
                    setError(body.error ?? 'The daemon returned no queue snapshot.');
                    return;
                }
                accept(body.snapshot, body.samples);
            } catch {
                setStale(true);
                setError('Big Smooth is not answering. Start it with `th up`.');
            }
        };
        void tick();
        const id = window.setInterval(() => void tick(), 1000);
        return () => window.clearInterval(id);
    }, [isDemo, source, accept]);

    return { snap, samples, error, stale, demo, paused, setPaused, version };
}
