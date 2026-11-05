import assert from 'node:assert/strict';
import { test } from 'node:test';

import { paginate } from '../src/paginate.js';

const letters = ['a', 'b', 'c', 'd', 'e'];

test('rejects page 0', () => {
    assert.throws(() => paginate(letters, 0, 2), RangeError);
});

test('rejects a zero page size', () => {
    assert.throws(() => paginate(letters, 1, 0), RangeError);
});

test('page 1 is the first page', () => {
    assert.deepEqual(paginate(letters, 1, 2).items, ['a', 'b']);
});

test('a partial last page still counts as a page', () => {
    const last = paginate(letters, 3, 2);
    assert.deepEqual(last.items, ['e']);
    assert.equal(last.totalPages, 3);
    assert.equal(last.hasNext, false);
});
