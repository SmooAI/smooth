// Smooth model lineup — derived from the published benchmark, not hand-listed.
//
// th-3a5d22: the fixed preset chips (Flash/Code/UI/Plan/…) are gone. The composer
// now offers a searchable list of EVERY benchmarked model, each carrying capability
// badges mapped straight from `docs/model-scores.json` (copied to `./model-scores.json`
// for bundling). Default = gpt-6-luna. The `SmoothMode` shape is kept so the rest
// of the app (operator send path, CostBar, persistence) is unchanged — one model IS
// one "mode" now, its `id`/`model` both the model string.

import scores from './model-scores.json' with { type: 'json' };

// ── Raw benchmark shape ──────────────────────────────────────────────────────
// One entry per model from a `smooth-bench agentic` run. `cost_usd` /
// `cost_per_pass_usd` are ABSENT (not 0) for a model whose spend couldn't be
// priced — treat missing as unknown, never as free.

export interface ModelScore {
    model: string;
    pass_rate_pct: number;
    passed: number;
    conclusive: number;
    inconclusive: number;
    cost_usd?: number | null;
    cost_per_pass_usd?: number | null;
    duration_s: number;
    safety_violations: number;
}

interface BenchFile {
    /** The bench run date, `YYYY-MM-DD`. Absent in catalogs published before
     * SMOODEV-4174 — the picker then says "date unknown", never a guess. */
    date?: string;
    suite: string;
    trials: number;
    scenario_count: number;
    /** Benched models, filtered to the offered set (SMOODEV-3342 model policy). */
    models: ModelScore[];
    /** Offered models the bench has not scored yet — listed with no score
     * ("not yet benched"), never an invented number. Absent in older catalogs. */
    unbenched?: string[];
}

const BENCH = scores as BenchFile;

// ── Badges — EXACTLY these three, earned from the data ────────────────────────
// 🏆 top score (ties allowed), 💚 best value (lowest known $/pass), 🛡️ safest
// (zero safety violations, independent of pass rate). A badge is a comparison,
// so it needs a field to compare against: 🏆 and 💚 need at least
// MIN_RANKED_MODELS contenders, and 🏆 also needs a score worth winning with
// (SMOODEV-4174: a lone 17.9% model wore the trophy). Unbenched models earn
// nothing. Premium is a tier, not an award — a text tag, never an emoji badge.
// Deliberately NO "fastest": `duration_s` is wall-clock for the whole run on a
// loaded machine, not latency — it would mislead.

export interface Badge {
    emoji: string;
    /** Human label, used as the badge's tooltip / aria-label. */
    label: string;
}

export const BADGE_TOP: Badge = { emoji: '🏆', label: 'Top score' };
export const BADGE_VALUE: Badge = { emoji: '💚', label: 'Best value' };
export const BADGE_SAFE: Badge = { emoji: '🛡️', label: 'Safest' };

/** 🏆 and 💚 are only awarded among at least this many benched (resp. priced)
 * models — "best of one" is not a ranking. */
export const MIN_RANKED_MODELS = 2;

/** 🏆 is withheld when even the leader passes less than this share of the
 * suite: the best of a weak field is not a recommendation. */
export const TOP_SCORE_MIN_PCT = 60;

/** The premium tier is a curated set, not a cost threshold. From th-3a5d22.
 * SMOODEV-3342: the offered set is the gpt-6-luna family + Groq, with gpt-6-sol
 * as the one explicit high-quality choice — so it is the whole premium tier. */
export const PREMIUM_MODELS = new Set(['gpt-6-sol']);

/** The model a fresh session lands on. */
export const DEFAULT_MODEL = 'gpt-6-luna';

/** The main models, listed first in the picker in this order (SMOODEV-4148,
 * SMOODEV-4174). The default is always main, even if it is not listed here. */
export const MAIN_MODELS: readonly string[] = ['gpt-6-luna', 'claude-haiku-5-5'];

/** Human names for the models we offer. Anything not listed shows its id. */
const MODEL_LABELS: Record<string, string> = {
    'gpt-6-luna': 'GPT-6 Luna',
    'gpt-6-luna-fast': 'GPT-6 Luna Fast',
    'gpt-6-luna-high': 'GPT-6 Luna High',
    'gpt-6-sol': 'GPT-6 Sol',
    'claude-haiku-5-5': 'Claude Haiku 5.5',
    'groq-gpt-oss-120b': 'GPT-OSS 120B · Groq',
    'groq-gpt-oss-20b': 'GPT-OSS 20B · Groq',
    'groq-qwen3.8-27b': 'Qwen3.8 27B · Groq',
};

/** The name the picker shows for a model id (the id itself when unknown). */
export function modelLabel(id: string): string {
    return MODEL_LABELS[id] ?? id;
}

/** Whether a model belongs in the picker's "Main" group. */
export function isMainModel(id: string, defaultModel: string = DEFAULT_MODEL): boolean {
    return id === defaultModel || MAIN_MODELS.includes(id);
}

// ── Pure derivation (unit-tested; takes data, imports nothing) ────────────────

/** A finite, real cost-per-pass, or null when the model couldn't be priced. */
export function costPerPass(m: ModelScore): number | null {
    return typeof m.cost_per_pass_usd === 'number' && Number.isFinite(m.cost_per_pass_usd) ? m.cost_per_pass_usd : null;
}

/** The pass rate that earns 🏆 (ties share it), or null when no one earns it:
 * fewer than MIN_RANKED_MODELS benched, or the leader is under
 * TOP_SCORE_MIN_PCT. */
export function topScorePct(models: ModelScore[]): number | null {
    if (models.length < MIN_RANKED_MODELS) return null;
    const top = models.reduce((max, m) => Math.max(max, m.pass_rate_pct), Number.NEGATIVE_INFINITY);
    return top >= TOP_SCORE_MIN_PCT ? top : null;
}

/** The single lowest-$/pass model, ignoring any whose cost is unknown (they
 * must never win a value comparison). Null unless at least MIN_RANKED_MODELS
 * are priced. */
export function bestValueModel(models: ModelScore[]): string | null {
    const priced = models.filter((m) => costPerPass(m) !== null);
    if (priced.length < MIN_RANKED_MODELS) return null;
    let best = priced[0];
    for (const m of priced) if (costPerPass(m)! < costPerPass(best)!) best = m;
    return best.model;
}

/** The badges a model earns, in display order (🏆 💚 🛡️). */
export function badgesFor(m: ModelScore, topPct: number | null, bestValue: string | null): Badge[] {
    const out: Badge[] = [];
    if (topPct !== null && m.pass_rate_pct === topPct) out.push(BADGE_TOP);
    if (m.model === bestValue) out.push(BADGE_VALUE);
    if (m.safety_violations === 0) out.push(BADGE_SAFE);
    return out;
}

/** One selectable row: model id + the numbers the picker renders. `passRatePct`
 * is taken verbatim from the bench (for claude-fable-5 that is passed/conclusive
 * over 25, NOT passed/28 — never recompute it here). */
export interface ModelRow {
    model: string;
    /** null = the default model before its first bench run (rendered "not yet
     * benched", never as 0%). Every bench-derived row has a number. */
    passRatePct: number | null;
    /** null renders as "unknown", never $0. */
    costPerPassUsd: number | null;
    badges: Badge[];
    premium: boolean;
}

/** Derive the ordered picker rows from raw bench models: best first — pass rate
 * desc, then cheapest-per-pass (unknown cost sorts last). */
export function deriveRows(models: ModelScore[]): ModelRow[] {
    const topPct = topScorePct(models);
    const bestValue = bestValueModel(models);
    return models
        .map((m) => ({
            model: m.model,
            passRatePct: m.pass_rate_pct,
            costPerPassUsd: costPerPass(m),
            badges: badgesFor(m, topPct, bestValue),
            premium: PREMIUM_MODELS.has(m.model),
        }))
        .sort((a, b) => (b.passRatePct ?? 0) - (a.passRatePct ?? 0) || (a.costPerPassUsd ?? Infinity) - (b.costPerPassUsd ?? Infinity));
}

/** Guarantee the default model is selectable. The picker lists only benched
 * models, so a new default that has not been benched yet (gpt-6-luna when it
 * landed, th-3030cd) would be missing from it, and `modeById` would have
 * nothing to fall back to. Such a default goes FIRST with no score or cost —
 * never an invented number. Once the bench scores it, its real row is used. */
export function ensureDefaultRow(rows: ModelRow[], defaultModel: string = DEFAULT_MODEL): ModelRow[] {
    if (rows.some((r) => r.model === defaultModel)) return rows;
    return [{ model: defaultModel, passRatePct: null, costPerPassUsd: null, badges: [], premium: PREMIUM_MODELS.has(defaultModel) }, ...rows];
}

/** Append the catalog's offered-but-unbenched models after the benched rows,
 * with no score or cost (rendered "not yet benched" / "unknown"). A model that
 * already has a row keeps it. */
export function appendUnbenched(rows: ModelRow[], unbenched: readonly string[] | undefined): ModelRow[] {
    const seen = new Set(rows.map((r) => r.model));
    const extra: ModelRow[] = [];
    for (const model of unbenched ?? []) {
        if (seen.has(model)) continue;
        seen.add(model);
        extra.push({ model, passRatePct: null, costPerPassUsd: null, badges: [], premium: PREMIUM_MODELS.has(model) });
    }
    return [...rows, ...extra];
}

/** Move the main models to the front, in MAIN_MODELS order with the default
 * first; everything else keeps its order behind them. */
export function mainFirst(rows: ModelRow[], defaultModel: string = DEFAULT_MODEL): ModelRow[] {
    const rank = (id: string) => (id === defaultModel ? -1 : MAIN_MODELS.indexOf(id));
    const main = rows.filter((r) => isMainModel(r.model, defaultModel)).sort((a, b) => rank(a.model) - rank(b.model));
    return [...main, ...rows.filter((r) => !isMainModel(r.model, defaultModel))];
}

/** The picker rows for a catalog: the main models first (default leading), then
 * benched rows best-first, then the offered models the bench has not scored. */
export function catalogRows(bench: Pick<BenchFile, 'models' | 'unbenched'>): ModelRow[] {
    return mainFirst(appendUnbenched(ensureDefaultRow(deriveRows(bench.models)), bench.unbenched));
}

/** The second line under a model's name: score and $/pass for a benched model,
 * a single "not benched" for one that isn't. Unknown $/pass is left out rather
 * than repeated on every row — the footer says it once. */
export function rowMeta(row: ModelRow): string {
    if (row.passRatePct === null) return 'not benched';
    const parts = [`${row.passRatePct.toFixed(1)}% pass`];
    if (row.costPerPassUsd !== null) parts.push(fmtPerPass(row.costPerPassUsd));
    return parts.join(' · ');
}

/** $/pass with trailing zeros trimmed. */
export function fmtPerPass(n: number): string {
    return `$${(n < 0.001 ? n.toFixed(5) : n.toFixed(4)).replace(/0+$/, '').replace(/\.$/, '')}/pass`;
}

/** The live blended token price as text ("$0.35/M tok"), or null when the
 * gateway hasn't priced the model. Replaces the unexplained 💚💛🧡❤️ glyph. */
export function priceText(cost: ModelCost | undefined): string | null {
    if (!cost) return null;
    const m = blendedPerMillion(cost);
    return `$${m < 1 ? m.toFixed(2) : m.toFixed(m < 10 ? 1 : 0)}/M tok`;
}

/** Bench metadata shown in the picker footer. */
export interface BenchMeta {
    date: string | null;
    suite: string;
    trials: number;
    scenarios: number;
}

export function benchMetaOf(bench: Pick<BenchFile, 'date' | 'suite' | 'trials' | 'scenario_count'>): BenchMeta {
    const date = typeof bench.date === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(bench.date) ? bench.date : null;
    return { date, suite: bench.suite, trials: bench.trials, scenarios: bench.scenario_count };
}

/** The footer line: when the bench ran (and how long ago), and on what. Says
 * "date unknown" rather than guessing; flags rows whose $/pass wasn't measured. */
export function benchFooter(meta: BenchMeta, rows: readonly ModelRow[], now: Date = new Date()): string {
    let when = 'bench date unknown';
    if (meta.date) {
        const days = Math.floor((now.getTime() - Date.parse(`${meta.date}T00:00:00Z`)) / 86_400_000);
        when = `benchmarked ${meta.date}${days >= 1 ? ` (${days} day${days === 1 ? '' : 's'} ago)` : ''}`;
    }
    const out = [when, `${meta.suite} · ${meta.trials} trials × ${meta.scenarios} scenarios`];
    if (rows.some((r) => r.passRatePct !== null && r.costPerPassUsd === null)) out.push('$/pass not measured for some models');
    return out.join(' · ');
}

/** Defaults a saved choice may be left over from. A session whose saved model
 * is one of these most likely never picked it, so it follows the new default
 * once; the flag stops that from overriding a deliberate re-pick later. */
export const PREVIOUS_DEFAULTS = new Set(['gpt-5.6-luna']);
export const DEFAULT_MIGRATION_KEY = `smooth.mode.default-migrated.${DEFAULT_MODEL}`;

/** The model a session should start on, given what localStorage holds. Pure:
 * returns the id plus whether to record that the one-time migration ran. */
export function initialModeId(saved: string | null, migrated: boolean, defaultModel: string = DEFAULT_MODEL): { id: string; markMigrated: boolean } {
    if (!saved) return { id: defaultModel, markMigrated: false };
    if (!migrated && PREVIOUS_DEFAULTS.has(saved) && saved !== defaultModel) return { id: defaultModel, markMigrated: true };
    return { id: saved, markMigrated: false };
}

// ── Live catalog fetch (single source of truth) ───────────────────────────────

/** Fetch the bench catalog the daemon serves (`GET /api/model-catalog`,
 * th-1d8007) and derive picker rows from it — one source of truth so a bench
 * refresh reaches every client without the old per-platform hand-copy. Returns
 * null on any failure (offline, old daemon, malformed body); callers fall back
 * to the bundled `MODEL_ROWS`. Same-origin relative URL: the page is served by
 * the daemon that also serves this route. */
export async function fetchModelRows(): Promise<ModelRow[] | null> {
    return (await fetchModelCatalog())?.rows ?? null;
}

/** `fetchModelRows` plus the catalog's bench metadata (date, suite, trials), so
 * the footer describes the data actually shown. */
export async function fetchModelCatalog(): Promise<{ rows: ModelRow[]; meta: BenchMeta } | null> {
    try {
        const res = await fetch('/api/model-catalog', { headers: { accept: 'application/json' } });
        if (!res.ok) return null;
        const data = (await res.json()) as Partial<BenchFile>;
        if (!data || !Array.isArray(data.models) || data.models.length === 0) return null;
        const rows = catalogRows({ models: data.models, unbenched: Array.isArray(data.unbenched) ? data.unbenched : undefined });
        registerLiveRows(rows);
        const meta = benchMetaOf({
            date: data.date,
            suite: data.suite ?? BENCH.suite,
            trials: data.trials ?? BENCH.trials,
            scenario_count: data.scenario_count ?? BENCH.scenario_count,
        });
        return { rows, meta };
    } catch {
        return null;
    }
}

// ── Bound to the bundled data (synchronous default + offline fallback) ─────────

export const MODEL_ROWS: ModelRow[] = catalogRows(BENCH);
export const BENCH_META: BenchMeta = benchMetaOf(BENCH);

// ── SmoothMode back-compat (one model = one mode) ─────────────────────────────

export type ModeTier = 'budget' | 'premium';

export interface SmoothMode {
    /** Stable id — now the model string itself. Persisted to localStorage. */
    id: string;
    /** Human label shown in the cost bar. */
    label: string;
    /** A glyph that reads the mode at a glance (its primary badge). */
    emoji: string;
    /** The model id sent on `send_message`. */
    model: string;
    tier: ModeTier;
}

/** One picker row as a mode. */
export function rowToMode(r: ModelRow): SmoothMode {
    return { id: r.model, label: modelLabel(r.model), emoji: r.badges[0]?.emoji ?? '◍', model: r.model, tier: r.premium ? 'premium' : 'budget' };
}

export const MODES: SmoothMode[] = MODEL_ROWS.map(rowToMode);

/** The mode a fresh session lands on. */
export const DEFAULT_MODE_ID = DEFAULT_MODEL;

const MODE_BY_ID = new Map(MODES.map((m) => [m.id, m]));

/** Models the live catalog offers but the bundled one predates (SMOODEV-4148:
 * Claude Haiku 5.5 on older bundles). Without these, picking one fell back to
 * luna in `modeById` and the turn silently ran on the wrong model. */
const LIVE_MODE_BY_ID = new Map<string, SmoothMode>();

/** Make the live catalog's rows resolvable by `modeById`. */
export function registerLiveRows(rows: readonly ModelRow[]): void {
    for (const r of rows) if (!MODE_BY_ID.has(r.model)) LIVE_MODE_BY_ID.set(r.model, rowToMode(r));
}

/** Look a mode up by id (model string), falling back to the default (luna) when
 * unknown — a saved preference pointing at a retired preset lands on luna. */
export function modeById(id: string | null | undefined): SmoothMode {
    return (id && (MODE_BY_ID.get(id) ?? LIVE_MODE_BY_ID.get(id))) || MODE_BY_ID.get(DEFAULT_MODE_ID)!;
}

// ── Live per-token cost (unchanged — the /admin/model-costs path) ─────────────

/** Per-token costs from `GET /admin/model-costs`, keyed by model id. */
export interface ModelCost {
    inputCostPerToken: number;
    outputCostPerToken: number;
    tier?: string;
    useCases?: string[];
}

export type ModelCosts = Record<string, ModelCost>;

/** A traffic-light glyph for a model's blended $/1M-token rate.
 * 💚 <$1, 💛 $1–5, 🧡 $5–30, ❤️ >$30. */
export function costBadge(inputCostPerToken: number, outputCostPerToken: number): string {
    const perMillion = ((inputCostPerToken + outputCostPerToken) / 2) * 1e6;
    if (perMillion < 1) return '💚';
    if (perMillion < 5) return '💛';
    if (perMillion <= 30) return '🧡';
    return '❤️';
}

/** Blended $/1M-token rate — the number behind the badge. */
export function blendedPerMillion(cost: ModelCost): number {
    return ((cost.inputCostPerToken + cost.outputCostPerToken) / 2) * 1e6;
}

/** A mode is "expensive" when its badge is 🧡 or ❤️ (≥ $5/1M blended). */
export function isExpensiveBadge(badge: string): boolean {
    return badge === '🧡' || badge === '❤️';
}
