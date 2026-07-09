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
    { id: 'flash', label: 'Flash', emoji: '⚡', model: 'deepseek-v4-flash', tier: 'budget' },
    { id: 'code', label: 'Code', emoji: '💻', model: 'minimax-m2.7', tier: 'budget' },
    { id: 'ui', label: 'UI', emoji: '🎨', model: 'glm-5.1', tier: 'budget' },
    { id: 'plan', label: 'Plan', emoji: '🧠', model: 'deepseek-v4-pro', tier: 'budget' },
    { id: 'fast', label: 'Fast', emoji: '🏎️', model: 'groq-gpt-oss-20b', tier: 'budget' },
    // Premium — the "spend real money" tier.
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
