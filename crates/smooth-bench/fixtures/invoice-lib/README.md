# invoice-lib

Tiny invoicing helpers. All money is integer **cents**.

```js
import { subtotal, total, formatMoney } from './src/invoice.js';

const invoice = { items: [{ description: 'Widget', unitCents: 1250, qty: 2 }], taxRate: 0.08 };
subtotal(invoice); // 2500
total(invoice); // 2700
formatMoney(2700); // "$27.00"
```

Run the tests with `npm test`.
