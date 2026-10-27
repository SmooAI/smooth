// Held out from the agent (SMOODEV-4125). Edge cases the visible tests skip.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { paginate } from '../src/paginate.js';

const nums = Array.from({ length: 10 }, (_, i) => i + 1);

test('every page in order covers the list exactly once', () => {
    const seen = [];
    for (let page = 1; page <= paginate(nums, 1, 3).totalPages; page += 1) {
        seen.push(...paginate(nums, page, 3).items);
    }
    assert.deepEqual(seen, nums);
});

test('an exact multiple has no extra empty page', () => {
    const p = paginate(nums, 2, 5);
    assert.deepEqual(p.items, [6, 7, 8, 9, 10]);
    assert.equal(p.totalPages, 2);
    assert.equal(p.hasNext, false);
    assert.equal(p.hasPrev, true);
});

test('hasNext is true before the last page', () => {
    const p = paginate(nums, 1, 3);
    assert.deepEqual(p.items, [1, 2, 3]);
    assert.equal(p.totalPages, 4);
    assert.equal(p.hasNext, true);
    assert.equal(p.hasPrev, false);
});

test('a page past the end is empty, not an error', () => {
    const p = paginate(nums, 9, 3);
    assert.deepEqual(p.items, []);
    assert.equal(p.hasNext, false);
});

test('an empty list has zero pages', () => {
    const p = paginate([], 1, 10);
    assert.deepEqual(p.items, []);
    assert.equal(p.total, 0);
    assert.equal(p.totalPages, 0);
    assert.equal(p.hasNext, false);
});

test('a page size larger than the list returns everything on page 1', () => {
    const p = paginate(nums, 1, 50);
    assert.equal(p.items.length, 10);
    assert.equal(p.totalPages, 1);
});

test('the input array is not mutated', () => {
    const copy = [...nums];
    paginate(nums, 2, 4);
    assert.deepEqual(nums, copy);
});

test('a fractional page is rejected', () => {
    assert.throws(() => paginate(nums, 1.5, 3), RangeError);
});
