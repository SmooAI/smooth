import assert from 'node:assert/strict';
import { test } from 'node:test';

import { buildReport } from '../src/report.js';

test('summarises orders, skipping cancelled ones', () => {
    const orders = [
        { id: 1, customer: 'ann', region: 'west', status: 'paid', items: [{ sku: 'a', qty: 2, priceCents: 1050 }] },
        { id: 2, customer: 'bob', region: 'east', status: 'paid', items: [{ sku: 'b', qty: 1, priceCents: 305 }] },
        { id: 3, customer: 'cy', region: 'east', status: 'cancelled', items: [{ sku: 'c', qty: 9, priceCents: 9999 }] },
    ];
    assert.equal(
        buildReport(orders),
        ['ORDER REPORT', 'Orders: 2', 'Cancelled: 1', 'Revenue: $24.05', 'Average order: $12.03', '  east: $3.05', '  west: $21.00', ''].join('\n'),
    );
});
