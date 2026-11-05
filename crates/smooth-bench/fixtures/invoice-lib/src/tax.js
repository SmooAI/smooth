/**
 * Tax on an amount in cents, rounded half-up to the nearest cent.
 * @param {number} cents
 * @param {number} rate e.g. 0.08 for 8%
 */
export function taxOn(cents, rate) {
    if (rate < 0) {
        throw new Error('tax rate cannot be negative');
    }
    return Math.round(cents * rate);
}
