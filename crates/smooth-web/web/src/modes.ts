// Smooth model lineup — derived from the published benchmark, not hand-listed.
//
// th-3a5d22: the fixed preset chips (Flash/Code/UI/Plan/…) are gone. The composer
// now offers a searchable list of EVERY benchmarked model, each carrying capability
// badges mapped straight from `docs/model-scores.json` (copied to `./model-scores.json`
// for bundling). Default = gpt-5.6-luna. The `SmoothMode` shape is kept so the rest
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
    suite: string;
    trials: number;
    scenario_count: number;
    models: ModelScore[];
}

const BENCH = scores as BenchFile;

// ── Badges — EXACTLY these four, mapped from the data ─────────────────────────
// 🏆 top score (ties allowed), 💚 best value (lowest known $/pass), 🛡️ safest
// (zero safety violations, independent of pass rate), 💎 premium tier.
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
export const BADGE_PREMIUM: Badge = { emoji: '💎', label: 'Premium' };

/** The premium tier is a curated set, not a cost threshold — gpt-5.5/gpt-5.4 are
 * expensive too but aren't "premium" here. From th-3a5d22. */
export const PREMIUM_MODELS = new Set(['claude-fable-5', 'gpt-5.6-sol-high']);

/** The model a fresh session lands on. */
export const DEFAULT_MODEL = 'gpt-5.6-luna';

// ── Pure derivation (unit-tested; takes data, imports nothing) ────────────────

/** A finite, real cost-per-pass, or null when the model couldn't be priced. */
export function costPerPass(m: ModelScore): number | null {
    return typeof m.cost_per_pass_usd === 'number' && Number.isFinite(m.cost_per_pass_usd) ? m.cost_per_pass_usd : null;
}

/** Highest pass rate in the field (ties share 🏆). */
export function topScorePct(models: ModelScore[]): number {
    return models.reduce((max, m) => Math.max(max, m.pass_rate_pct), Number.NEGATIVE_INFINITY);
}

/** The single lowest-$/pass model, ignoring any whose cost is unknown (they
 * must never win a value comparison). Null if nothing is priced. */
export function bestValueModel(models: ModelScore[]): string | null {
    let best: ModelScore | null = null;
    for (const m of models) {
        const c = costPerPass(m);
        if (c === null) continue;
        if (best === null || c < costPerPass(best)!) best = m;
    }
    return best?.model ?? null;
}

/** The badges a model earns, in display order (🏆 💚 🛡️ 💎). */
export function badgesFor(m: ModelScore, topPct: number, bestValue: string | null): Badge[] {
    const out: Badge[] = [];
    if (m.pass_rate_pct === topPct) out.push(BADGE_TOP);
    if (m.model === bestValue) out.push(BADGE_VALUE);
    if (m.safety_violations === 0) out.push(BADGE_SAFE);
    if (PREMIUM_MODELS.has(m.model)) out.push(BADGE_PREMIUM);
    return out;
}

/** One selectable row: model id + the numbers the picker renders. `passRatePct`
 * is taken verbatim from the bench (for claude-fable-5 that is passed/conclusive
 * over 25, NOT passed/28 — never recompute it here). */
export interface ModelRow {
    model: string;
    passRatePct: number;
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
        .sort((a, b) => b.passRatePct - a.passRatePct || (a.costPerPassUsd ?? Infinity) - (b.costPerPassUsd ?? Infinity));
}

// ── Bound to the bundled data ─────────────────────────────────────────────────

export const MODEL_ROWS: ModelRow[] = deriveRows(BENCH.models);
export const BENCH_SUITE = BENCH.suite;
export const BENCH_TRIALS = BENCH.trials;
export const BENCH_SCENARIOS = BENCH.scenario_count;

/** The bench run date. `model-scores.json` carries no date field, so this tracks
 * the run recorded in this file's git history. ponytail: bump on refresh; add a
 * `date` key upstream and read it here if the bench ever emits one. */
export const BENCH_DATE = '2026-08-11';

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

export const MODES: SmoothMode[] = MODEL_ROWS.map((r) => ({
    id: r.model,
    label: r.model,
    emoji: r.badges[0]?.emoji ?? '◍',
    model: r.model,
    tier: r.premium ? 'premium' : 'budget',
}));

/** The mode a fresh session lands on. */
export const DEFAULT_MODE_ID = DEFAULT_MODEL;

const MODE_BY_ID = new Map(MODES.map((m) => [m.id, m]));

/** Look a mode up by id (model string), falling back to the default (luna) when
 * unknown — a saved preference pointing at a retired preset lands on luna. */
export function modeById(id: string | null | undefined): SmoothMode {
    return (id && MODE_BY_ID.get(id)) || MODE_BY_ID.get(DEFAULT_MODE_ID)!;
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
