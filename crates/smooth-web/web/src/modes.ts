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
    // Budget — the everyday tier.
    //
    // Model choices here are BENCHMARKED, not picked by version number.
    // `smooth-bench convo`, 15 scenarios, 2026-08-08 (3 trials for the top
    // two, 1 for the screening field):
    //
    //   qwen3.7-plus-direct  84.4%   $0.0105/pass   1.3 tool calls/turn
    //   deepseek-v4-flash    77.5%   $0.0040/pass   2.4 tool calls/turn
    //   gpt-5.5              72.1%   $0.1694/pass
    //   glm-5.2-direct       66.7%   $0.0012/pass
    //   minimax-m3-direct    66.7%   $0.0098/pass
    //   kimi-k2.7-code       60.0%   $0.0216/pass
    //
    // Flash stays deepseek-v4-flash: it is the cheapest per passing
    // scenario by 2.6x and this is the default every session lands on.
    // Code moves to the measured leader. See docs/Model-Leaderboard.md.
    { id: 'flash', label: 'Flash', emoji: '⚡', model: 'deepseek-v4-flash', tier: 'budget' },
    // th-d326cb: was minimax-m2.7 (superseded by m3, and m3 scored 66.7%).
    // qwen3.7-plus-direct is the best model we have measured at any price
    // — 84.4%, and it gets there with 43% fewer tool calls than Flash.
    { id: 'code', label: 'Code', emoji: '💻', model: 'qwen3.7-plus-direct', tier: 'budget' },
    // th-d326cb: was glm-5.1. 5.2 is the current release and was unpriced
    // in LiteLLM until today, which is why it had never been benchable.
    { id: 'ui', label: 'UI', emoji: '🎨', model: 'glm-5.2-direct', tier: 'budget' },
    { id: 'plan', label: 'Plan', emoji: '🧠', model: 'deepseek-v4-pro', tier: 'budget' },
    { id: 'fast', label: 'Fast', emoji: '🏎️', model: 'groq-gpt-oss-20b', tier: 'budget' },
    // Premium — the "spend real money" tier.
    // Premium slots stay on gpt-5.5/5.4/5.5-pro DELIBERATELY. GPT-5.6 is
    // newer, but it only works with tools at all as of today's config
    // change (th-8d4ec4 routes it through the Responses API), and it has
    // not been benchmarked yet — its one measured run scored 0/45 because
    // every tool call 400'd. "Newer" is a hypothesis; these three are
    // verified working with tools right now. They move when the bench
    // says to, not before.
    { id: 'flash+', label: 'Flash+', emoji: '⚡', model: 'gemini-3.5-flash', tier: 'premium' },
    { id: 'code+', label: 'Code+', emoji: '💻', model: 'claude-opus-4-8', tier: 'premium' },
    { id: 'ui+', label: 'UI+', emoji: '🎨', model: 'gpt-5.5', tier: 'premium' },
    { id: 'plan+', label: 'Plan+', emoji: '🧠', model: 'gpt-5.4', tier: 'premium' },
    { id: 'max', label: 'Max', emoji: '💎', model: 'gpt-5.5-pro', tier: 'premium' },
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
