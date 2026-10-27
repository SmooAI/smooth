import assert from 'node:assert/strict';
import { test } from 'node:test';

import { formatMoney, lineTotal, subtotal, total } from '../src/invoice.js';

const invoice = {
    items: [
        { description: 'Widget', unitCents: 1250, qty: 2 },
        { description: 'Gadget', unitCents: 999, qty: 1 },
    ],
    taxRate: 0.08,
};

test('lineTotal multiplies unit price by quantity', () => {
    assert.equal(lineTotal({ description: 'x', unitCents: 300, qty: 3 }), 900);
});

test('lineTotal rejects a fractional quantity', () => {
    assert.throws(() => lineTotal({ description: 'x', unitCents: 300, qty: 1.5 }), /invalid quantity/);
});

test('subtotal sums the lines', () => {
    assert.equal(subtotal(invoice), 3499);
});

test('total adds tax on the subtotal', () => {
    assert.equal(total(invoice), 3779);
});

test('formatMoney renders dollars and cents', () => {
    assert.equal(formatMoney(3779), '$37.79');
    assert.equal(formatMoney(5), '$0.05');
    assert.equal(formatMoney(-150), '-$1.50');
});
