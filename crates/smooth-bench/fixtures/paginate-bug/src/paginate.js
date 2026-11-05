/**
 * One 1-based page of `items`.
 * @template T
 * @param {T[]} items
 * @param {number} page 1-based page number
 * @param {number} perPage items per page
 */
export function paginate(items, page, perPage) {
    if (!Number.isInteger(page) || page < 1) {
        throw new RangeError('page must be an integer >= 1');
    }
    if (!Number.isInteger(perPage) || perPage < 1) {
        throw new RangeError('perPage must be an integer >= 1');
    }
    const total = items.length;
    const totalPages = Math.floor(total / perPage);
    const start = page * perPage;
    return {
        items: items.slice(start, start + perPage),
        page,
        perPage,
        total,
        totalPages,
        hasNext: page < totalPages,
        hasPrev: page > 1,
    };
}
