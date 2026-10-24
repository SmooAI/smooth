// buildReport: summary of orders for the ops channel
export function buildReport(orders) {
    var out = '';
    var total = 0;
    var count = 0;
    var cancelled = 0;
    var byRegion = {};
    for (var i = 0; i < orders.length; i++) {
        var o = orders[i];
        if (o.status == 'cancelled') {
            cancelled = cancelled + 1;
            continue;
        }
        var t = 0;
        for (var j = 0; j < o.items.length; j++) {
            t = t + o.items[j].qty * o.items[j].priceCents;
        }
        total = total + t;
        count = count + 1;
        if (byRegion[o.region] == undefined) {
            byRegion[o.region] = 0;
        }
        var t2 = 0;
        for (var k = 0; k < o.items.length; k++) {
            t2 = t2 + o.items[k].qty * o.items[k].priceCents;
        }
        byRegion[o.region] = byRegion[o.region] + t2;
    }
    out = out + 'ORDER REPORT\n';
    out = out + 'Orders: ' + count + '\n';
    out = out + 'Cancelled: ' + cancelled + '\n';
    var d = Math.floor(total / 100);
    var c = total % 100;
    if (c < 10) {
        out = out + 'Revenue: $' + d + '.0' + c + '\n';
    } else {
        out = out + 'Revenue: $' + d + '.' + c + '\n';
    }
    if (count > 0) {
        var avg = Math.round(total / count);
        var d2 = Math.floor(avg / 100);
        var c2 = avg % 100;
        if (c2 < 10) {
            out = out + 'Average order: $' + d2 + '.0' + c2 + '\n';
        } else {
            out = out + 'Average order: $' + d2 + '.' + c2 + '\n';
        }
    } else {
        out = out + 'Average order: n/a\n';
    }
    var regions = Object.keys(byRegion);
    regions.sort();
    for (var r = 0; r < regions.length; r++) {
        var amt = byRegion[regions[r]];
        var d3 = Math.floor(amt / 100);
        var c3 = amt % 100;
        if (c3 < 10) {
            out = out + '  ' + regions[r] + ': $' + d3 + '.0' + c3 + '\n';
        } else {
            out = out + '  ' + regions[r] + ': $' + d3 + '.' + c3 + '\n';
        }
    }
    return out;
}
