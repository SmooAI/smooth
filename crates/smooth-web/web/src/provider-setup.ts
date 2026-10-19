// First-run model setup (SMOODEV-3918) — the pure half of the first-run card.
// The daemon answers GET /api/llm/provider; with no model it offers two doors:
// the Family plan (POST /api/llm/provider/family — mints this machine's Smoo
// gateway key, or answers `plan_required` with a checkout URL to open and poll)
// and your own key (POST /api/llm/provider — validated, probed, saved). The
// rules mirror `smooth_cast::provider_setup` so the form can reject the obvious
// before a round-trip; the daemon stays the authority.

export const FAMILY_PLAN_PITCH = 'Family plan — $9/month, 14-day free trial, hosted models';

/** Where a dismissed card is remembered (per browser). */
export const DISMISS_KEY = 'smooth.firstRunCard.dismissed';

export interface ProviderStatus {
    configured: boolean;
    model?: string;
    gateway_host?: string;
    restart_required?: boolean;
}

/** Show the card only when the daemon has no model and the user hasn't waved it off. */
export function shouldShowCard(status: ProviderStatus | null, dismissed: boolean): boolean {
    return status !== null && !status.configured && !dismissed;
}

export type ByoFormat = 'openai' | 'anthropic';

export interface ByoForm {
    format: ByoFormat;
    base_url: string;
    api_key: string;
    model: string;
}

/** Prefilled values for each format. */
export function byoDefaults(format: ByoFormat): ByoForm {
    return format === 'anthropic'
        ? { format, base_url: 'https://api.anthropic.com/v1', api_key: '', model: 'claude-sonnet-4-5' }
        : { format, base_url: 'https://api.openai.com/v1', api_key: '', model: 'gpt-4o-mini' };
}

function hostOf(url: string): string {
    const rest = url.includes('://') ? url.split('://')[1] : url;
    return (rest.split(/[/?#]/)[0] ?? '').toLowerCase();
}

function isLocal(url: string): boolean {
    const u = url.toLowerCase();
    return u.includes('localhost') || u.includes('127.0.0.1') || u.includes('0.0.0.0') || u.includes('[::1]');
}

/** First problem with the form, or null when it's worth sending. */
export function validateByoForm(f: ByoForm): string | null {
    const url = f.base_url.trim();
    if (!/^https?:\/\//.test(url)) return 'Base URL must start with https://';
    if (!hostOf(url)) return 'Base URL has no host';
    if (url.startsWith('http://') && !isLocal(url) && !hostOf(url).endsWith('.local') && !/^(10|192\.168|172\.(1[6-9]|2\d|3[01])|100)\./.test(hostOf(url)))
        return 'A remote endpoint must use https://';
    if (!f.api_key.trim() && !isLocal(url)) return 'An API key is required';
    if (!f.model.trim() || /\s/.test(f.model.trim())) return 'Model is required (e.g. gpt-4o-mini)';
    return null;
}

/** What the card shows after a POST /api/llm/provider/family answer. */
export type FamilyStep =
    | { kind: 'saved'; message: string }
    | { kind: 'checkout'; upgradeUrl: string; message: string; topUp: string }
    | { kind: 'signin'; message: string }
    | { kind: 'error'; message: string };

export function familyStepFrom(body: unknown): FamilyStep {
    const b = (body ?? {}) as Record<string, unknown>;
    const message = typeof b.message === 'string' ? b.message : '';
    switch (b.status) {
        case 'saved':
            return { kind: 'saved', message };
        case 'plan_required': {
            const url =
                typeof b.upgradeUrl === 'string' && b.upgradeUrl.startsWith('https://')
                    ? b.upgradeUrl
                    : 'https://smoo.ai/apps/start/product?plan=Family%20plan&term=monthly';
            // SMOODEV-3969: the daemon's "Top up $5 or $10 anytime during the trial …" line.
            const topUp = typeof b.topUp === 'string' ? b.topUp : '';
            return { kind: 'checkout', upgradeUrl: url, message, topUp };
        }
        case 'signed_out':
            return { kind: 'signin', message };
        default:
            return { kind: 'error', message: message || 'Something went wrong — try again, or run `th model setup` in a terminal.' };
    }
}

/** What the card shows after a POST /api/llm/provider answer. */
export function byoResultFrom(ok: boolean, body: unknown): { saved: boolean; message: string } {
    const b = (body ?? {}) as Record<string, unknown>;
    const message = typeof b.message === 'string' ? b.message : ok ? 'Saved.' : 'That endpoint did not work.';
    return { saved: ok && b.status === 'configured', message };
}
