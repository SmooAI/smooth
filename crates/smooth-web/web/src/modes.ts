// Smooth Modes — the model lineup the composer's `/smooth-mode` switcher drives.
// Each mode pins a turn to a specific model; budget modes are the daily-driver
// defaults, premium modes the "spend real money" tier. Cost is surfaced live so
// switching into something pricey is a deliberate, visible act (th-f512b1,
// th-2a6330). Mirrors the TUI's slash-command UX.

export type ModeTier = 'budget' | 'premium';

export interface SmoothMode {
    /** Stable id used by `/smooth-mode <id>` and persisted to localStorage. */
    id: string;
    /** Short human label shown in the cost bar + picker. */
    label: string;
    /** A glyph that reads the mode at a glance. */
    emoji: string;
    /** The model id sent on `send_message`. */
    model: string;
    tier: ModeTier;
}

/** The full lineup, budget first then premium — also the picker order. */
export const MODES: SmoothMode[] = [
    // ── Every model here is BENCHMARKED. ────────────────────────────────
    //
    // `smooth-bench agentic`, 28 scenarios, **3 trials each**, 2026-08-11.
    // A scenario counts as passed only when every trial passed, so these
    // rates are lower and far harder than the old single-trial numbers —
    // one run is an anecdote, three is a rate.
    //
    //   model                  rate    $/pass   passes/$1  safety
    //   gpt-5.6-luna          89.3%   0.0005      1,876      3
    //   deepseek-v4-pro       89.3%   0.0005      1,828      2
    //   gemini-3.6-flash      85.7%   0.0002      6,250      2
    //   gpt-5.6-sol-high      85.7%   0.0199         50      1
    //   gpt-5.5               85.7%   0.4255          2      2
    //   qwen-3.7-max-direct   82.1%   0.0045        222      2
    //   gpt-5.4               82.1%   0.2006          5      2
    //   gemini-3.5-flash      78.6%   0.0002      5,435      4
    //   glm-5.2-direct        78.6%   0.0043        232      2
    //   claude-fable-5        76.0%   0.0395         25      4
    //   deepseek-v4-flash     75.0%   0.0012        846      2
    //   kimi-k2.7-code-direct 75.0%   0.0023        437      3
    //   claude-sonnet-5       75.0%   0.0083        120  clean
    //   groq-gpt-oss-20b      17.9%  unknown          —      5
    //
    // `safety` counts trials that breached a safety invariant — destroyed
    // data the scenario told it to protect, or leaked a secret. It is NOT
    // the pass rate and must not be read as one: a model can fail a
    // scenario for skipping a required note while having protected the
    // data perfectly. Most breaches across the field are one scenario,
    // `cancel-without-approval`, where the agent flips a subscription's
    // status without the approval ticket the policy requires.
    //
    // Cost is the run's own tokens at the gateway's published rate. The
    // previous figures were a SHARED key's spend delta and were wrong by
    // up to 1,324x (th-adf614) — do not compare these against them.

    // Budget — the everyday tier, and now also the best tier.
    // The headline result: the two highest-scoring models in the entire
    // lineup cost half a cent per passing scenario.
    { id: 'flash', label: 'Flash', emoji: '⚡', model: 'gpt-5.6-luna', tier: 'budget' },
    // th-170c67: was deepseek-v4-flash (75.0%). Luna scores 89.3% at
    // roughly half the cost per pass — better AND cheaper, which is why
    // it takes the default every session lands on.
    { id: 'code', label: 'Code', emoji: '💻', model: 'deepseek-v4-pro', tier: 'budget' },
    // Was qwen3.7-plus-direct, which this round did not measure. Pro ties
    // luna at 89.3% and is the fastest of the leaders (~14s/scenario).
    { id: 'ui', label: 'UI', emoji: '🎨', model: 'gemini-3.6-flash', tier: 'budget' },
    // Was glm-5.2-direct (78.6%). Gemini 3.6 Flash is both higher-scoring
    // and the cheapest model measured per passing scenario, at 6,250
    // passes per dollar.
    { id: 'plan', label: 'Plan', emoji: '🧠', model: 'qwen-3.7-max-direct', tier: 'budget' },
    { id: 'fast', label: 'Fast', emoji: '🏎️', model: 'gemini-3.5-flash', tier: 'budget' },
    // th-170c67: was groq-gpt-oss-20b, which is retired outright. It
    // scored 17.9% — a quarter of the next-worst model — breached safety
    // in 5 trials, and, for a slot literally named Fast, was the SLOWEST
    // model in the lineup by 3.5x (67s/scenario against gemini's 15s).
    // It was picked for Groq's reputation for speed and never measured.

    // Premium — the "spend real money" tier, now TWO slots instead of
    // five, because only two survived contact with a real cost column.
    //
    // Read this before promoting anything back in: on THIS suite, premium
    // buys nothing. gpt-5.5 scores 85.7% — BELOW the free-tier default —
    // and costs $10.21 against luna's $0.013 for the same 28 scenarios.
    // That is 785x for a worse result, and it is now a measured number
    // rather than a shared-key artefact. What premium may still buy is
    // robustness on work harder than this suite models: long-horizon
    // tasks, big refactors, ambiguous specs. That is a hypothesis the
    // bench does not yet test — which is exactly why these three are kept
    // and the other two were dropped, not the other way round.
    { id: 'code+', label: 'Code+', emoji: '💻', model: 'claude-fable-5', tier: 'premium' },
    // Replaces claude-opus-4-8 (never benchmarked at 3 trials). Fable
    // reads at 76.0% here, but 3 of its turns were terminated by the
    // provider's content filter on scenarios carrying prompt-injection
    // fixtures — those are scored INCONCLUSIVE, not failed (th-05edac).
    // Its measured rate is a floor, not a ceiling.
    { id: 'max', label: 'Max', emoji: '💎', model: 'gpt-5.6-sol-high', tier: 'premium' },
    // Replaces gpt-5.5-pro (never benchmarked). Sol-high matches gpt-5.5's
    // 85.7% at 1/21st the cost per pass, and carries the best safety
    // record of any high scorer (1 breach). If you want the expensive
    // option, this is the one with evidence behind it.
    //
    // Dropped: `flash+` (gemini-3.5-flash moved down to budget `fast`,
    // where its price belongs), `plan+` (gpt-5.4, dominated by sol-high
    // on both score and cost at 10x the price), and `ui+` (gpt-5.5 —
    // 85.7% for $10.21 against luna's $0.013 on the same suite; there is
    // no reading of that trade that favours it, and re-benching it just
    // spends money to re-learn the same thing). `modeById`
    // falls back to the default for an unknown id, so a saved preference
    // pointing at a retired slot lands on Flash rather than breaking.
    { id: 'smoo-jr', label: 'Smoo Jr', emoji: '🧒', model: 'claude-sonnet-5', tier: 'budget' },
    // th-170c67: was deepseek-v4-flash. claude-sonnet-5 is the ONLY model
    // in the lineup with a clean safety record across 84 trials — zero
    // breaches — which is the property this mode is actually about. It
    // also scores lowest of the working models (75.0%), and the two facts
    // are related: it refuses and asks where the others act. For a child's
    // session that trade is the right way round.
];

/** The mode a fresh session lands on. */
export const DEFAULT_MODE_ID = 'flash';

const MODE_BY_ID = new Map(MODES.map((m) => [m.id, m]));

/** Look a mode up by id, falling back to the default when unknown. */
export function modeById(id: string | null | undefined): SmoothMode {
    return (id && MODE_BY_ID.get(id)) || MODE_BY_ID.get(DEFAULT_MODE_ID)!;
}

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
