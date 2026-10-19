// First-run model setup card (SMOODEV-3918). Shown once, in the chat view, when
// the daemon reports no model (GET /api/llm/provider → configured:false). The
// same two doors `th up` offers: the Family plan (sign in with Smoo, start the
// trial, the daemon mints this machine's gateway key) or your own
// OpenAI-compatible / Anthropic key (tested with one request, then saved).
// Big Smooth reads its model at startup, so both end with "restart me".

import { KeyRound, Sparkles, X } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';

import { resolveTarget } from './operator';
import {
    DISMISS_KEY,
    FAMILY_PLAN_PITCH,
    byoDefaults,
    byoResultFrom,
    familyStepFrom,
    shouldShowCard,
    validateByoForm,
    type ByoForm,
    type ByoFormat,
    type FamilyStep,
    type ProviderStatus,
} from './provider-setup';
import { useSmooAuth } from './SmooSignIn';

type Door = 'choose' | 'family' | 'byo' | 'done';

const RESTART = 'th down && th up';

function readDismissed(): boolean {
    try {
        return localStorage.getItem(DISMISS_KEY) === '1';
    } catch {
        return false;
    }
}

async function post(path: string, body?: unknown): Promise<{ ok: boolean; body: unknown }> {
    const { http, token } = resolveTarget();
    const r = await fetch(`${http}${path}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', ...(token ? { authorization: `Bearer ${token}` } : {}) },
        body: JSON.stringify(body ?? {}),
    });
    return { ok: r.ok, body: await r.json().catch(() => null) };
}

export function FirstRunCard() {
    const [status, setStatus] = useState<ProviderStatus | null>(null);
    const [dismissed, setDismissed] = useState(readDismissed);
    const [door, setDoor] = useState<Door>('choose');
    const [doneMessage, setDoneMessage] = useState('');

    useEffect(() => {
        fetch('/api/llm/provider')
            .then((r) => (r.ok ? r.json() : null))
            .then((s: ProviderStatus | null) => setStatus(s))
            .catch(() => {});
    }, []);

    if (door !== 'done' && !shouldShowCard(status, dismissed)) return null;

    const dismiss = () => {
        try {
            localStorage.setItem(DISMISS_KEY, '1');
        } catch {
            /* private mode — dismissal lasts this page view */
        }
        setDismissed(true);
        setDoor('choose');
    };
    const finish = (message: string) => {
        setDoneMessage(message);
        setDoor('done');
    };

    return (
        <section
            className="mx-auto mt-6 w-full max-w-xl rounded-(--radius) border bg-panel/90 p-5 shadow-lg backdrop-blur"
            aria-label="Set up Big Smooth's model"
        >
            <div className="mb-3 flex items-start justify-between gap-3">
                <div>
                    <h2 className="font-(family-name:--font-display) text-lg">Pick a model for Big Smooth</h2>
                    <p className="text-sm text-(--color-muted-foreground)">He needs one to think with. You can switch later (th model setup).</p>
                </div>
                {door !== 'done' && (
                    <button onClick={dismiss} aria-label="Not now" title="Not now" className="text-(--color-muted-foreground) hover:text-foreground">
                        <X className="size-4" />
                    </button>
                )}
            </div>
            {door === 'choose' && (
                <div className="grid gap-2 sm:grid-cols-2">
                    <button
                        onClick={() => setDoor('family')}
                        className="rounded-xl bg-(--color-coral) px-4 py-3 text-left text-(--color-coral-ink) transition hover:opacity-90"
                    >
                        <span className="flex items-center gap-1.5 font-semibold">
                            <Sparkles className="size-4" /> Family plan
                        </span>
                        <span className="text-xs opacity-80">$9/month · 14-day free trial · hosted models, no keys</span>
                    </button>
                    <button
                        onClick={() => setDoor('byo')}
                        className="rounded-xl border bg-panel-2 px-4 py-3 text-left transition hover:border-(--color-muted-foreground)"
                    >
                        <span className="flex items-center gap-1.5 font-semibold">
                            <KeyRound className="size-4" /> Your own key
                        </span>
                        <span className="text-xs text-(--color-muted-foreground)">OpenAI-compatible or Anthropic endpoint</span>
                    </button>
                </div>
            )}
            {door === 'family' && <FamilyDoor onDone={finish} onBack={() => setDoor('choose')} />}
            {door === 'byo' && <ByoDoor onDone={finish} onBack={() => setDoor('choose')} />}
            {door === 'done' && (
                <div className="space-y-2 text-sm">
                    <p>{doneMessage}</p>
                    <p className="text-(--color-muted-foreground)">
                        Big Smooth reads his model at startup. Restart him to start chatting:{' '}
                        <code className="rounded bg-black/20 px-1.5 py-0.5">{RESTART}</code> (or quit and reopen the app).
                    </p>
                </div>
            )}
        </section>
    );
}

function FamilyDoor({ onDone, onBack }: { onDone: (m: string) => void; onBack: () => void }) {
    const auth = useSmooAuth();
    const [step, setStep] = useState<FamilyStep | null>(null);
    const [busy, setBusy] = useState(false);
    const [waiting, setWaiting] = useState(false);
    const asked = useRef(false);

    const mint = async () => {
        setBusy(true);
        try {
            const r = await post('/api/llm/provider/family');
            const next = familyStepFrom(r.body);
            setStep(next);
            if (next.kind === 'saved') onDone(next.message || 'Family plan active.');
        } catch {
            setStep({ kind: 'error', message: 'Big Smooth did not answer — is it still running?' });
        } finally {
            setBusy(false);
        }
    };

    // Signed in → try the mint once on our own (the plan may already be active).
    useEffect(() => {
        if (auth.status?.loggedIn && !asked.current) {
            asked.current = true;
            void mint();
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [auth.status?.loggedIn]);

    // After the checkout tab opens, retry every 5s until the trial is active.
    useEffect(() => {
        if (!waiting || step?.kind === 'saved') return;
        const id = setInterval(() => void mint(), 5000);
        return () => clearInterval(id);
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [waiting, step?.kind]);

    const signedIn = auth.status?.loggedIn === true;
    return (
        <div className="space-y-3 text-sm">
            <p>{FAMILY_PLAN_PITCH} through the Smoo AI gateway.</p>
            {!signedIn &&
                (auth.device ? (
                    <a
                        href={auth.device.verification_uri_complete}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="block rounded-xl border bg-panel-2 px-4 py-3"
                    >
                        Approve this sign-in on smoo.ai, code{' '}
                        <code className="rounded bg-black/20 px-1.5 py-0.5 font-mono tracking-widest">{auth.device.user_code}</code>
                    </a>
                ) : (
                    <button onClick={auth.startSignIn} className="rounded-xl bg-(--color-coral) px-4 py-2 font-semibold text-(--color-coral-ink)">
                        {auth.error ? 'Sign-in failed — retry' : 'Sign in with Smoo'}
                    </button>
                ))}
            {signedIn && step?.kind === 'checkout' && (
                <div className="space-y-2">
                    {step.message && <p className="text-(--color-muted-foreground)">{step.message}</p>}
                    <button
                        onClick={() => {
                            window.open(step.upgradeUrl, '_blank', 'noopener');
                            setWaiting(true);
                        }}
                        className="rounded-xl bg-(--color-coral) px-4 py-2 font-semibold text-(--color-coral-ink)"
                    >
                        Start the 14-day free trial
                    </button>
                    {waiting && <p className="text-(--color-muted-foreground)">Waiting for checkout to finish… this card updates on its own.</p>}
                </div>
            )}
            {signedIn && (step === null || busy) && step?.kind !== 'checkout' && <p className="text-(--color-muted-foreground)">Checking your plan…</p>}
            {step?.kind === 'signin' && <p className="text-(--color-amber)">{step.message}</p>}
            {step?.kind === 'error' && (
                <p className="text-(--color-amber)">
                    {step.message}{' '}
                    <button className="underline" onClick={() => void mint()}>
                        Retry
                    </button>
                </p>
            )}
            <button onClick={onBack} className="text-xs text-(--color-muted-foreground) underline">
                Back
            </button>
        </div>
    );
}

function ByoDoor({ onDone, onBack }: { onDone: (m: string) => void; onBack: () => void }) {
    const [form, setForm] = useState<ByoForm>(byoDefaults('openai'));
    const [error, setError] = useState<string | null>(null);
    const [busy, setBusy] = useState(false);

    const setFormat = (format: ByoFormat) => setForm({ ...byoDefaults(format), api_key: form.api_key });
    const field = (k: keyof Omit<ByoForm, 'format'>) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [k]: e.target.value });

    const save = async () => {
        const problem = validateByoForm(form);
        if (problem) {
            setError(problem);
            return;
        }
        setBusy(true);
        setError(null);
        try {
            const r = await post('/api/llm/provider', form);
            const res = byoResultFrom(r.ok, r.body);
            if (res.saved) onDone(res.message);
            else setError(res.message);
        } catch {
            setError('Big Smooth did not answer — is it still running?');
        } finally {
            setBusy(false);
        }
    };

    const input = 'w-full rounded-lg border bg-(--color-background) px-3 py-2 text-sm outline-none focus:border-(--color-muted-foreground)';
    return (
        <form
            className="space-y-2 text-sm"
            onSubmit={(e) => {
                e.preventDefault();
                void save();
            }}
        >
            <div className="flex gap-1" role="radiogroup" aria-label="API format">
                {(['openai', 'anthropic'] as const).map((f) => (
                    <button
                        key={f}
                        type="button"
                        role="radio"
                        aria-checked={form.format === f}
                        onClick={() => setFormat(f)}
                        className={`rounded-full px-3 py-1 text-xs ${form.format === f ? 'bg-foreground text-(--color-background)' : 'border text-(--color-muted-foreground)'}`}
                    >
                        {f === 'openai' ? 'OpenAI-compatible' : 'Anthropic'}
                    </button>
                ))}
            </div>
            <label className="block">
                <span className="text-xs text-(--color-muted-foreground)">Base URL</span>
                <input className={input} value={form.base_url} onChange={field('base_url')} autoComplete="off" spellCheck={false} />
            </label>
            <label className="block">
                <span className="text-xs text-(--color-muted-foreground)">API key (blank for a local server)</span>
                <input className={input} type="password" value={form.api_key} onChange={field('api_key')} autoComplete="off" />
            </label>
            <label className="block">
                <span className="text-xs text-(--color-muted-foreground)">Model</span>
                <input className={input} value={form.model} onChange={field('model')} autoComplete="off" spellCheck={false} />
            </label>
            {error && <p className="text-(--color-amber)">{error}</p>}
            <div className="flex items-center gap-3 pt-1">
                <button
                    type="submit"
                    disabled={busy}
                    className="rounded-xl bg-(--color-coral) px-4 py-2 font-semibold text-(--color-coral-ink) disabled:opacity-50"
                >
                    {busy ? 'Testing…' : 'Test and save'}
                </button>
                <button type="button" onClick={onBack} className="text-xs text-(--color-muted-foreground) underline">
                    Back
                </button>
            </div>
        </form>
    );
}
