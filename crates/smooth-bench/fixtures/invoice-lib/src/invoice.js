import { taxOn } from './tax.js';

/**
 * @typedef {{ description: string, unitCents: number, qty: number }} LineItem
 * @typedef {{ items: LineItem[], taxRate?: number }} Invoice
 */

/** Total for one line, in cents. */
export function lineTotal(item) {
    if (!Number.isInteger(item.qty) || item.qty < 0) {
        throw new Error(`invalid quantity for ${item.description}`);
    }
    return item.unitCents * item.qty;
}

/** Sum of every line, in cents, before tax. */
export function subtotal(invoice) {
    return invoice.items.reduce((sum, item) => sum + lineTotal(item), 0);
}

/** Subtotal plus tax, in cents. */
export function total(invoice) {
    const sub = subtotal(invoice);
    return sub + taxOn(sub, invoice.taxRate ?? 0);
}

/** Format cents as US dollars, e.g. 2700 -> "$27.00". */
export function formatMoney(cents) {
    const sign = cents < 0 ? '-' : '';
    const abs = Math.abs(cents);
    return `${sign}$${Math.floor(abs / 100)}.${String(abs % 100).padStart(2, '0')}`;
}
