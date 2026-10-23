# paginate

`paginate(items, page, perPage)` returns one page of a list. Pages are
**1-based**: page 1 is the first page.

```js
paginate(['a', 'b', 'c', 'd', 'e'], 1, 2);
// { items: ['a', 'b'], page: 1, perPage: 2, total: 5, totalPages: 3, hasNext: true, hasPrev: false }
```

A page past the end returns an empty `items` array (not an error).
`page < 1` or `perPage < 1` throws a `RangeError`.
