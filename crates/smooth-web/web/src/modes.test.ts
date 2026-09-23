import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
    DEFAULT_MODEL,
    MODEL_ROWS,
    bestValueModel,
    badgesFor,
    costPerPass,
    deriveRows,
    ensureDefaultRow,
    fetchModelRows,
    initialModeId,
    modeById,
    topScorePct,
    type ModelScore,
} from './modes.ts';

// A trimmed fixture mirroring the real bench: two tied leaders, a cheap-but-not-top
// value winner, a clean-safety model, a premium model with an inconclusive-driven
// denominator, and a null-cost straggler.
const fixture: ModelScore[] = [
    {
        model: 'gpt-5.6-luna',
        pass_rate_pct: 89.3,
        passed: 25,
        conclusive: 28,
        inconclusive: 0,
        cost_usd: 0.01333,
        cost_per_pass_usd: 0.000533,
        duration_s: 1,
        safety_violations: 3,
    },
    {
        model: 'deepseek-v4-pro',
        pass_rate_pct: 89.3,
        passed: 25,
        conclusive: 28,
        inconclusive: 0,
        cost_usd: 0.0137,
        cost_per_pass_usd: 0.000547,
        duration_s: 1,
        safety_violations: 2,
    },
    {
        model: 'gemini-3.6-flash',
        pass_rate_pct: 85.7,
        passed: 24,
        conclusive: 28,
        inconclusive: 0,
        cost_usd: 0.0038,
        cost_per_pass_usd: 0.00016,
        duration_s: 1,
        safety_violations: 2,
    },
    {
        model: 'gpt-5.6-sol-high',
        pass_rate_pct: 85.7,
        passed: 24,
        conclusive: 28,
        inconclusive: 0,
        cost_usd: 0.478,
        cost_per_pass_usd: 0.019925,
        duration_s: 1,
        safety_violations: 1,
    },
    {
        model: 'claude-fable-5',
        pass_rate_pct: 76.0,
        passed: 19,
        conclusive: 25,
        inconclusive: 3,
        cost_usd: 0.749,
        cost_per_pass_usd: 0.039455,
        duration_s: 1,
        safety_violations: 4,
    },
    {
        model: 'claude-sonnet-5',
        pass_rate_pct: 75.0,
        passed: 21,
        conclusive: 28,
        inconclusive: 0,
        cost_usd: 0.174,
        cost_per_pass_usd: 0.00832,
        duration_s: 1,
        safety_violations: 0,
    },
    // No cost_usd / cost_per_pass_usd — the groq straggler.
    { model: 'groq-gpt-oss-20b', pass_rate_pct: 17.9, passed: 5, conclusive: 28, inconclusive: 0, duration_s: 1, safety_violations: 5 },
];

const emojis = (models: ModelScore[], id: string) => {
    const top = topScorePct(models);
    const val = bestValueModel(models);
    const m = models.find((x) => x.model === id)!;
    return badgesFor(m, top, val).map((b) => b.emoji);
};

test('🏆 top score is shared by the tied leaders, no one else', () => {
    assert.equal(topScorePct(fixture), 89.3);
    assert.ok(emojis(fixture, 'gpt-5.6-luna').includes('🏆'));
    assert.ok(emojis(fixture, 'deepseek-v4-pro').includes('🏆'));
    assert.ok(!emojis(fixture, 'gemini-3.6-flash').includes('🏆'));
});

test('💚 best value is the lowest KNOWN $/pass — the null-cost model cannot win', () => {
    assert.equal(bestValueModel(fixture), 'gemini-3.6-flash');
    assert.ok(emojis(fixture, 'gemini-3.6-flash').includes('💚'));
    assert.ok(!emojis(fixture, 'groq-gpt-oss-20b').includes('💚'));
});

test('💚 falls to the cheapest priced model even when every cost is unknown', () => {
    const allUnknown = fixture.map((m) => ({ ...m, cost_per_pass_usd: undefined }));
    assert.equal(bestValueModel(allUnknown), null);
});

test('🛡️ safest is exactly the zero-violation model, independent of pass rate', () => {
    assert.deepEqual(emojis(fixture, 'claude-sonnet-5'), ['🛡️']);
    assert.ok(!emojis(fixture, 'gpt-5.6-luna').includes('🛡️')); // top score, but 3 violations
});

test('💎 premium is the curated set, not a cost threshold', () => {
    assert.ok(emojis(fixture, 'claude-fable-5').includes('💎'));
    assert.ok(emojis(fixture, 'gpt-5.6-sol-high').includes('💎'));
    assert.ok(!emojis(fixture, 'claude-sonnet-5').includes('💎')); // pricier than value winner, still not premium
});

test('null cost stays null (unknown), never coerced to 0/free', () => {
    const groq = fixture.find((m) => m.model === 'groq-gpt-oss-20b')!;
    assert.equal(costPerPass(groq), null);
    const row = deriveRows(fixture).find((r) => r.model === 'groq-gpt-oss-20b')!;
    assert.equal(row.costPerPassUsd, null);
});

test('fable rate is the provided pass_rate_pct (over conclusive=25), not passed/28', () => {
    const row = deriveRows(fixture).find((r) => r.model === 'claude-fable-5')!;
    assert.equal(row.passRatePct, 76.0);
    // Guard against a future refactor recomputing from the 28-scenario total.
    assert.notEqual(row.passRatePct, Number(((19 / 28) * 100).toFixed(1))); // 67.9
});

test('rows are ordered best-first: pass rate desc, then cheapest $/pass, unknown last', () => {
    const order = deriveRows(fixture).map((r) => r.model);
    assert.equal(order[0], 'gpt-5.6-luna'); // 89.3, cheaper than pro
    assert.equal(order[1], 'deepseek-v4-pro'); // 89.3, pricier
    assert.equal(order[2], 'gemini-3.6-flash'); // 85.7, cheapest at its tier
    assert.equal(order[order.length - 1], 'groq-gpt-oss-20b'); // lowest rate, unknown cost
});

// ── fetchModelRows (th-1d8007): the live-catalog fetch clients derive against ──

/** Run `body` with `globalThis.fetch` stubbed to `stub`, always restoring it. */
async function withFetch<T>(stub: typeof fetch, body: () => Promise<T>): Promise<T> {
    const original = globalThis.fetch;
    globalThis.fetch = stub;
    try {
        return await body();
    } finally {
        globalThis.fetch = original;
    }
}

test('fetchModelRows derives ordered rows from the daemon catalog', async () => {
    const payload = { suite: 'agentic', trials: 1, scenario_count: 28, models: fixture };
    const rows = await withFetch(
        (async () => new Response(JSON.stringify(payload), { status: 200, headers: { 'content-type': 'application/json' } })) as typeof fetch,
        () => fetchModelRows(),
    );
    assert.ok(rows);
    // Same derivation as the bundled path — best-first, behind the default when
    // the catalog has not benched it yet (the fixture has no gpt-6-luna).
    assert.equal(rows![0].model, DEFAULT_MODEL);
    assert.equal(rows![0].passRatePct, null);
    assert.deepEqual(
        rows!.slice(1).map((r) => r.model),
        deriveRows(fixture).map((r) => r.model),
    );
});

test('ensureDefaultRow puts an unbenched default first with no invented numbers', () => {
    const rows = ensureDefaultRow(deriveRows(fixture), 'brand-new-default');
    assert.equal(rows[0].model, 'brand-new-default');
    assert.equal(rows[0].passRatePct, null, 'an unbenched model has no score, not 0%');
    assert.equal(rows[0].costPerPassUsd, null, 'and no cost, not $0');
    assert.deepEqual(rows[0].badges, []);
    assert.equal(rows.length, fixture.length + 1);
});

test('ensureDefaultRow leaves a benched default where the bench put it', () => {
    const derived = deriveRows(fixture);
    assert.deepEqual(ensureDefaultRow(derived, 'gpt-5.6-luna'), derived);
});

test('the bundled rows always contain the default, so modeById can fall back to it', () => {
    assert.ok(MODEL_ROWS.some((r) => r.model === DEFAULT_MODEL));
    assert.equal(modeById('some-retired-model').id, DEFAULT_MODEL);
});

test('initialModeId: no saved choice starts on the default', () => {
    assert.deepEqual(initialModeId(null, false, 'gpt-6-luna'), { id: 'gpt-6-luna', markMigrated: false });
});

test('initialModeId: a leftover previous default follows the new default once', () => {
    assert.deepEqual(initialModeId('gpt-5.6-luna', false, 'gpt-6-luna'), { id: 'gpt-6-luna', markMigrated: true });
});

test('initialModeId: after the migration, re-picking the old default sticks', () => {
    assert.deepEqual(initialModeId('gpt-5.6-luna', true, 'gpt-6-luna'), { id: 'gpt-5.6-luna', markMigrated: false });
});

test('initialModeId: a deliberate non-default choice is never touched', () => {
    assert.deepEqual(initialModeId('deepseek-v4-pro', false, 'gpt-6-luna'), { id: 'deepseek-v4-pro', markMigrated: false });
});

test('fetchModelRows returns null on a non-ok response (caller falls back to bundled)', async () => {
    const rows = await withFetch((async () => new Response('nope', { status: 503 })) as typeof fetch, () => fetchModelRows());
    assert.equal(rows, null);
});

test('fetchModelRows returns null on an empty/malformed body', async () => {
    const rows = await withFetch((async () => new Response(JSON.stringify({ models: [] }), { status: 200 })) as typeof fetch, () => fetchModelRows());
    assert.equal(rows, null);
});

test('fetchModelRows returns null when fetch throws (offline)', async () => {
    const rows = await withFetch(
        (async () => {
            throw new Error('offline');
        }) as typeof fetch,
        () => fetchModelRows(),
    );
    assert.equal(rows, null);
});
