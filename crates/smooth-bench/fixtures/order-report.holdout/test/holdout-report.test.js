// Held out from the agent (SMOODEV-4125). Characterization tests: these pin
// the ORIGINAL output byte for byte, quirks included, so a refactor that
// changes any behavior fails.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { buildReport } from '../src/report.js';

const lines = (...l) => [...l, ''].join('\n');

test('no orders', () => {
    assert.equal(buildReport([]), lines('ORDER REPORT', 'Orders: 0', 'Cancelled: 0', 'Revenue: $0.00', 'Average order: n/a'));
});

test('only cancelled orders', () => {
    const orders = [{ id: 1, customer: 'a', region: 'north', status: 'cancelled', items: [{ sku: 'x', qty: 1, priceCents: 500 }] }];
    assert.equal(buildReport(orders), lines('ORDER REPORT', 'Orders: 0', 'Cancelled: 1', 'Revenue: $0.00', 'Average order: n/a'));
});

test('single-digit cents are zero-padded everywhere', () => {
    const orders = [
        { id: 1, customer: 'a', region: 'south', status: 'paid', items: [{ sku: 'x', qty: 1, priceCents: 101 }] },
        { id: 2, customer: 'b', region: 'south', status: 'shipped', items: [{ sku: 'y', qty: 1, priceCents: 100 }] },
    ];
    // average 100.5 rounds to 101
    assert.equal(buildReport(orders), lines('ORDER REPORT', 'Orders: 2', 'Cancelled: 0', 'Revenue: $2.01', 'Average order: $1.01', '  south: $2.01'));
});

test('an order with no items still counts, at zero', () => {
    const orders = [
        { id: 1, customer: 'a', region: 'west', status: 'paid', items: [] },
        { id: 2, customer: 'b', region: 'west', status: 'paid', items: [{ sku: 'y', qty: 3, priceCents: 333 }] },
    ];
    assert.equal(buildReport(orders), lines('ORDER REPORT', 'Orders: 2', 'Cancelled: 0', 'Revenue: $9.99', 'Average order: $5.00', '  west: $9.99'));
});

test('regions sort by plain string order (uppercase first)', () => {
    const orders = [
        { id: 1, customer: 'a', region: 'beta', status: 'paid', items: [{ sku: 'x', qty: 1, priceCents: 1000 }] },
        { id: 2, customer: 'b', region: 'Zeta', status: 'paid', items: [{ sku: 'y', qty: 1, priceCents: 2000 }] },
        { id: 3, customer: 'c', region: 'alpha', status: 'paid', items: [{ sku: 'z', qty: 1, priceCents: 3000 }] },
    ];
    assert.equal(
        buildReport(orders),
        lines('ORDER REPORT', 'Orders: 3', 'Cancelled: 0', 'Revenue: $60.00', 'Average order: $20.00', '  Zeta: $20.00', '  alpha: $30.00', '  beta: $10.00'),
    );
});

test('only the exact status "cancelled" is skipped', () => {
    const orders = [
        { id: 1, customer: 'a', region: 'east', status: 'Cancelled', items: [{ sku: 'x', qty: 1, priceCents: 700 }] },
        { id: 2, customer: 'b', region: 'east', status: 'refunded', items: [{ sku: 'y', qty: 2, priceCents: 150 }] },
    ];
    assert.equal(buildReport(orders), lines('ORDER REPORT', 'Orders: 2', 'Cancelled: 0', 'Revenue: $10.00', 'Average order: $5.00', '  east: $10.00'));
});

test('large amounts keep every digit', () => {
    const orders = [{ id: 1, customer: 'a', region: 'west', status: 'paid', items: [{ sku: 'x', qty: 1000, priceCents: 123457 }] }];
    assert.equal(
        buildReport(orders),
        lines('ORDER REPORT', 'Orders: 1', 'Cancelled: 0', 'Revenue: $1234570.00', 'Average order: $1234570.00', '  west: $1234570.00'),
    );
});

test('the input is not mutated', () => {
    const orders = [{ id: 1, customer: 'a', region: 'west', status: 'paid', items: [{ sku: 'x', qty: 1, priceCents: 100 }] }];
    const before = JSON.stringify(orders);
    buildReport(orders);
    assert.equal(JSON.stringify(orders), before);
});
