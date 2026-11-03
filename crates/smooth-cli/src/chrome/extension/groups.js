// Pure tab-group logic behind `th chrome browse groups` and browse_groups. No chrome.* calls:
// background.js passes in what chrome.tabGroups/tabs/windows return, and
// scripts/chrome-groups-smoke.mjs tests the selection and safety rules directly.
const SmoothGroups = (() => {
    /**
     * One row per group: its tabs, the newest tab.lastAccessed (null when Chrome reports none),
     * and whether it holds the active tab of some window (`active`) or of the focused one (`focused`).
     */
    function summarize(groups, tabs, windows) {
        const focused = new Set(windows.filter((w) => w.focused).map((w) => w.id));
        return groups.map((g) => {
            const members = tabs.filter((t) => t.groupId === g.id);
            const seen = members.map((t) => t.lastAccessed).filter((n) => typeof n === 'number');
            const active = members.find((t) => t.active);
            return {
                id: g.id,
                title: g.title ?? '',
                color: g.color,
                collapsed: !!g.collapsed,
                windowId: g.windowId,
                tabs: members.length,
                tabIds: members.map((t) => t.id),
                lastAccessed: seen.length ? Math.max(...seen) : null,
                active: !!active,
                focused: !!active && focused.has(active.windowId),
            };
        });
    }

    /** `*` and `?` wildcards, whole title, case-insensitive. */
    function titleMatches(pattern, title) {
        const re = pattern
            .replace(/[.+^${}()|[\]\\]/g, '\\$&')
            .replace(/\*/g, '.*')
            .replace(/\?/g, '.');
        return new RegExp(`^${re}$`, 'i').test(title ?? '');
    }

    /**
     * Which summarized groups a close/ungroup touches. Ids and a title pick candidates (either
     * matches); `all`, or `idleMs` alone, picks every group. `idleMs` then keeps only groups whose
     * every tab was last used at least that long ago. A group holding a window's active tab is
     * never selected. Returns { selected, skipped: [{ group, reason }], missing: [id] }, where
     * reason is 'active', 'recent' or 'unknown' (Chrome reported no last-used time).
     */
    function select(summaries, { ids = [], title, idleMs, all = false } = {}, now = Date.now()) {
        const hasIdle = typeof idleMs === 'number';
        if (!ids.length && (title === undefined || title === null) && !hasIdle && !all) {
            throw new Error('say which groups: give group ids, a title, an idle time, or all');
        }
        if (hasIdle && !(idleMs > 0)) throw new Error('the idle time must be more than zero');
        const everything = all || (!ids.length && (title === undefined || title === null));
        const wanted = new Set(ids);
        const candidates = summaries.filter((g) => everything || wanted.has(g.id) || (typeof title === 'string' && titleMatches(title, g.title)));
        const found = new Set(summaries.map((g) => g.id));
        const out = { selected: [], skipped: [], missing: ids.filter((id) => !found.has(id)) };
        for (const g of candidates) {
            let reason = null;
            if (g.active) reason = 'active';
            else if (hasIdle && g.lastAccessed === null) reason = 'unknown';
            else if (hasIdle && now - g.lastAccessed < idleMs) reason = 'recent';
            if (reason) out.skipped.push({ group: g, reason });
            else out.selected.push(g);
        }
        return out;
    }

    return { summarize, titleMatches, select };
})();

if (typeof module !== 'undefined') module.exports = SmoothGroups;
