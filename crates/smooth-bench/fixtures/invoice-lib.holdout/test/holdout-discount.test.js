// Held out from the agent (SMOODEV-4125). Pins the spec in the prompt.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { total, totalWithDiscount } from '../src/invoice.js';

const invoice = {
    items: [
        { description: 'Widget', unitCents: 1250, qty: 2 },
        { description: 'Gadget', unitCents: 999, qty: 1 },
    ],
    taxRate: 0.08,
};

test('SAVE10 takes 10% off the subtotal, rounded to the nearest cent, before tax', () => {
    // 3499 - round(349.9) = 3149; tax round(251.92) = 252
    assert.equal(totalWithDiscount(invoice, 'SAVE10'), 3401);
});

test('FLAT5 takes $5.00 off the subtotal before tax', () => {
    // 3499 - 500 = 2999; tax round(239.92) = 240
    assert.equal(totalWithDiscount(invoice, 'FLAT5'), 3239);
});

test('codes are case-insensitive and trimmed', () => {
    assert.equal(totalWithDiscount(invoice, ' save10 '), 3401);
    assert.equal(totalWithDiscount(invoice, 'flat5'), 3239);
});

test('a flat discount never takes the subtotal below zero', () => {
    const cheap = { items: [{ description: 'Sticker', unitCents: 300, qty: 1 }], taxRate: 0.08 };
    assert.equal(totalWithDiscount(cheap, 'FLAT5'), 0);
});

test('an unknown code throws with a clear message', () => {
    assert.throws(() => totalWithDiscount(invoice, 'FREE100'), /unknown discount code/i);
});

test('no code means no discount', () => {
    assert.equal(totalWithDiscount(invoice), total(invoice));
    assert.equal(totalWithDiscount(invoice, undefined), total(invoice));
});

test('the existing total is untouched', () => {
    assert.equal(total(invoice), 3779);
});
