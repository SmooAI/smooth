import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
    DEFAULT_MODEL,
    MODEL_ROWS,
    MIN_RANKED_MODELS,
    TOP_SCORE_MIN_PCT,
    appendUnbenched,
    benchFooter,
    benchMetaOf,
    bestValueModel,
    catalogRows,
    badgesFor,
    costPerPass,
    deriveRows,
    ensureDefaultRow,
    fetchModelCatalog,
    fetchModelRows,
    initialModeId,
    isMainModel,
    mainFirst,
    modeById,
    modelLabel,
    priceText,
    rowMeta,
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

test('premium is a curated tier tag, never an emoji badge', () => {
    // SMOODEV-3342: gpt-6-sol is the whole premium tier. SMOODEV-4174: it is a
    // text tag, not an award — an unbenched premium model wore 💎 like a medal.
    const sol: ModelScore[] = [...fixture, { ...fixture[0], model: 'gpt-6-sol' }];
    assert.ok(deriveRows(sol).find((r) => r.model === 'gpt-6-sol')!.premium);
    assert.ok(!emojis(sol, 'gpt-6-sol').includes('💎'));
    assert.ok(!deriveRows(fixture).find((r) => r.model === 'claude-fable-5')!.premium);
    const unbenched = appendUnbenched([], ['gpt-6-sol'])[0];
    assert.ok(unbenched.premium);
    assert.deepEqual(unbenched.badges, [], 'an unbenched model earns no badge');
});

// ── SMOODEV-4174: badges must mean something ──────────────────────────────────

const groqOnly: ModelScore[] = [fixture.find((m) => m.model === 'groq-gpt-oss-20b')!];

test('🏆 is never awarded to a lone benched model (the 17.9% trophy)', () => {
    assert.ok(groqOnly.length < MIN_RANKED_MODELS);
    assert.equal(topScorePct(groqOnly), null);
    assert.ok(!emojis(groqOnly, 'groq-gpt-oss-20b').includes('🏆'));
});

test('🏆 is withheld when even the leader is under the bar', () => {
    const weak = fixture.map((m) => ({ ...m, pass_rate_pct: Math.min(m.pass_rate_pct, TOP_SCORE_MIN_PCT - 1) }));
    assert.equal(topScorePct(weak), null);
    for (const m of weak) assert.ok(!emojis(weak, m.model).includes('🏆'), m.model);
});

test('🏆 at exactly the bar is earned', () => {
    const field = [
        { ...fixture[0], pass_rate_pct: TOP_SCORE_MIN_PCT },
        { ...fixture[1], pass_rate_pct: 40 },
    ];
    assert.deepEqual(emojis(field, fixture[0].model).slice(0, 1), ['🏆']);
});

test('💚 needs at least two priced models to compare', () => {
    const onePriced = [fixture[0], groqOnly[0]];
    assert.equal(bestValueModel(onePriced), null);
});

test('unbenched rows carry no badges and say "not benched" once', () => {
    const rows = catalogRows({ models: groqOnly, unbenched: ['gpt-6-luna', 'gpt-6-sol', 'claude-haiku-5-5'] });
    for (const r of rows.filter((x) => x.passRatePct === null)) {
        assert.deepEqual(r.badges, [], r.model);
        assert.equal(rowMeta(r), 'not benched');
    }
});

test('rowMeta shows score and known $/pass, and omits an unknown cost', () => {
    const rows = deriveRows(fixture);
    assert.equal(rowMeta(rows.find((r) => r.model === 'gemini-3.6-flash')!), '85.7% pass · $0.00016/pass');
    assert.equal(rowMeta(rows.find((r) => r.model === 'groq-gpt-oss-20b')!), '17.9% pass');
});

test('priceText replaces the heart glyph with a number, and is null when unpriced', () => {
    assert.equal(priceText(undefined), null);
    assert.equal(priceText({ inputCostPerToken: 0.1e-6, outputCostPerToken: 0.5e-6 }), '$0.30/M tok');
    assert.equal(priceText({ inputCostPerToken: 3e-6, outputCostPerToken: 15e-6 }), '$9.0/M tok');
    assert.equal(priceText({ inputCostPerToken: 15e-6, outputCostPerToken: 75e-6 }), '$45/M tok');
});

// ── SMOODEV-4174: readable names, main models first ───────────────────────────

test('modelLabel gives the main models human names and falls back to the id', () => {
    assert.equal(modelLabel('gpt-6-luna'), 'GPT-6 Luna');
    assert.equal(modelLabel('claude-haiku-5-5'), 'Claude Haiku 5.5');
    assert.equal(modelLabel('some-new-model-with-a-very-long-identifier-v2'), 'some-new-model-with-a-very-long-identifier-v2');
    // Every label must keep its distinguishing part, so no two offered models read alike.
    const ids = [
        'gpt-6-luna',
        'gpt-6-luna-fast',
        'gpt-6-luna-high',
        'gpt-6-sol',
        'claude-haiku-5-5',
        'groq-gpt-oss-120b',
        'groq-gpt-oss-20b',
        'groq-qwen3.8-27b',
    ];
    assert.equal(new Set(ids.map(modelLabel)).size, ids.length);
});

test('main models lead the list: default first, then Haiku, then the rest in bench order', () => {
    const rows = catalogRows({ models: fixture, unbenched: ['gpt-6-sol', 'claude-haiku-5-5', 'gpt-6-luna'] });
    assert.deepEqual(
        rows.slice(0, 2).map((r) => r.model),
        ['gpt-6-luna', 'claude-haiku-5-5'],
    );
    assert.equal(rows[2].model, 'gpt-5.6-luna', 'benched best-first after the main group');
    assert.equal(rows[rows.length - 1].model, 'gpt-6-sol');
    assert.ok(isMainModel('gpt-6-luna') && isMainModel('claude-haiku-5-5') && !isMainModel('gpt-6-sol'));
});

test('mainFirst keeps a non-listed default at the very top', () => {
    const rows = mainFirst(appendUnbenched([], ['x', 'claude-haiku-5-5', 'my-default']), 'my-default');
    assert.deepEqual(
        rows.map((r) => r.model),
        ['my-default', 'claude-haiku-5-5', 'x'],
    );
});

// ── SMOODEV-4174: honest bench date ────────────────────────────────────────────

test('benchFooter shows the run date and its age', () => {
    const meta = benchMetaOf({ date: '2026-08-11', suite: 'agentic', trials: 3, scenario_count: 28 });
    assert.equal(benchFooter(meta, [], new Date('2026-10-24T12:00:00Z')), 'benchmarked 2026-08-11 (74 days ago) · agentic · 3 trials × 28 scenarios');
});

test('benchFooter says "date unknown" instead of guessing, and flags unmeasured cost once', () => {
    const meta = benchMetaOf({ date: undefined, suite: 'agentic', trials: 3, scenario_count: 28 });
    assert.equal(meta.date, null);
    assert.equal(benchMetaOf({ date: 'last tuesday', suite: 'a', trials: 1, scenario_count: 1 }).date, null);
    const footer = benchFooter(meta, deriveRows(groqOnly));
    assert.ok(footer.startsWith('bench date unknown'), footer);
    assert.ok(footer.endsWith('$/pass not measured for some models'), footer);
});

test('fetchModelCatalog carries the live catalog date into the footer', async () => {
    const payload = { date: '2026-10-25', suite: 'agentic', trials: 3, scenario_count: 28, models: fixture };
    const cat = await withFetch(
        (async () => new Response(JSON.stringify(payload), { status: 200, headers: { 'content-type': 'application/json' } })) as typeof fetch,
        () => fetchModelCatalog(),
    );
    assert.equal(cat!.meta.date, '2026-10-25');
    assert.equal(cat!.meta.trials, 3);
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

// ── SMOODEV-3342: the offered set is policy-restricted ─────────────────────────

test('appendUnbenched lists offered models with no score, after the benched rows', () => {
    const rows = appendUnbenched(deriveRows(fixture), ['gpt-6-sol', 'gpt-5.6-luna', 'gpt-6-sol']);
    const sol = rows[rows.length - 1];
    assert.equal(sol.model, 'gpt-6-sol');
    assert.equal(sol.passRatePct, null, 'unbenched is "not yet benched", never 0%');
    assert.equal(sol.costPerPassUsd, null);
    assert.ok(sol.premium, 'gpt-6-sol is the premium tier');
    assert.equal(rows.length, fixture.length + 1, 'a benched model keeps its row; duplicates collapse');
});

test('catalogRows: default first, benched best-first, then unbenched', () => {
    const rows = catalogRows({ models: fixture, unbenched: ['gpt-6-luna', 'groq-gpt-oss-120b'] });
    assert.equal(rows[0].model, DEFAULT_MODEL);
    assert.equal(rows[rows.length - 1].model, 'groq-gpt-oss-120b');
    assert.equal(rows.filter((r) => r.model === DEFAULT_MODEL).length, 1);
});

test('the bundled catalog offers only the gpt-6-luna family, Groq, and gpt-6-sol', () => {
    const allowed = /^(gpt-6-luna(-fast|-high)?|gpt-6-sol|groq-.+)$/;
    const offending = MODEL_ROWS.map((r) => r.model).filter((m) => !allowed.test(m));
    assert.deepEqual(offending, []);
});

test('fetchModelRows appends the catalog unbenched list', async () => {
    const payload = { suite: 'agentic', trials: 3, scenario_count: 28, models: fixture, unbenched: ['groq-gpt-oss-120b'] };
    const rows = await withFetch(
        (async () => new Response(JSON.stringify(payload), { status: 200, headers: { 'content-type': 'application/json' } })) as typeof fetch,
        () => fetchModelRows(),
    );
    assert.equal(rows![rows!.length - 1].model, 'groq-gpt-oss-120b');
});

test('a model only the live catalog offers is pickable once fetched (Haiku 5.5, SMOODEV-4148)', async () => {
    assert.equal(modeById('claude-haiku-5-5').model, DEFAULT_MODEL, 'unknown before the live catalog loads');
    const payload = { suite: 'agentic', trials: 3, scenario_count: 28, models: fixture, unbenched: ['gpt-6-luna', 'claude-haiku-5-5'] };
    const rows = await withFetch(
        (async () => new Response(JSON.stringify(payload), { status: 200, headers: { 'content-type': 'application/json' } })) as typeof fetch,
        () => fetchModelRows(),
    );
    const haiku = rows!.find((r) => r.model === 'claude-haiku-5-5');
    assert.ok(haiku, 'offered');
    assert.equal(haiku!.passRatePct, null, 'not yet benched, never an invented score');
    assert.equal(modeById('claude-haiku-5-5').model, 'claude-haiku-5-5', 'picking it sends it, not luna');
    assert.equal(modeById('gpt-6-luna').model, 'gpt-6-luna');
    assert.deepEqual(initialModeId('claude-haiku-5-5', false), { id: 'claude-haiku-5-5', markMigrated: false }, 'a saved pick survives reload');
});
