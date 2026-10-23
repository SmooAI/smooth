# order-report

`buildReport(orders)` turns a list of orders into a plain-text summary
for the ops channel. Each order is
`{ id, customer, region, status, items: [{ sku, qty, priceCents }] }`.
