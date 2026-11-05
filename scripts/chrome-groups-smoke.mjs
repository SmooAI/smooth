// Checks the tab-group selection and safety rules (extension/groups.js) behind
// `th chrome browse groups close|ungroup` and browse_groups: ids, title globs, idle time, all,
// and that a group holding any window's active tab is never selected. Usage: node scripts/chrome-groups-smoke.mjs
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const src = readFileSync(new URL('../crates/smooth-cli/src/chrome/extension/groups.js', import.meta.url), 'utf8');
const ctx = vm.createContext({});
vm.runInContext(`${src}\nglobalThis.SmoothGroups = SmoothGroups;`, ctx);
const G = ctx.SmoothGroups;

const NOW = 1_000_000_000_000;
const MIN = 60_000;
const windows = [
    { id: 1, focused: true },
    { id: 2, focused: false },
];
const groups = [
    { id: 10, title: 'Claude', color: 'orange', collapsed: false, windowId: 1 },
    { id: 11, title: 'Claude', color: 'orange', collapsed: true, windowId: 1 },
    { id: 12, title: 'claude (2)', color: 'orange', collapsed: false, windowId: 2 },
    { id: 13, title: 'Work', color: 'blue', collapsed: false, windowId: 2 },
    { id: 14, title: '', color: 'grey', collapsed: false, windowId: 1 },
    { id: 15, title: 'Claude', color: 'orange', collapsed: false, windowId: 1 },
];
const tabs = [
    // 10: two tabs, newest used 3h ago.
    { id: 100, groupId: 10, windowId: 1, active: false, lastAccessed: NOW - 300 * MIN },
    { id: 101, groupId: 10, windowId: 1, active: false, lastAccessed: NOW - 180 * MIN },
    // 11: used 10 minutes ago.
    { id: 110, groupId: 11, windowId: 1, active: false, lastAccessed: NOW - 10 * MIN },
    // 12: holds the active tab of an unfocused window, used long ago.
    { id: 120, groupId: 12, windowId: 2, active: true, lastAccessed: NOW - 1000 * MIN },
    // 13: holds nothing active, used a day ago.
    { id: 130, groupId: 13, windowId: 2, active: false, lastAccessed: NOW - 1440 * MIN },
    // 14: Chrome reports no last-used time.
    { id: 140, groupId: 14, windowId: 1, active: false },
    // 15: the focused window's active tab.
    { id: 150, groupId: 15, windowId: 1, active: true, lastAccessed: NOW },
    // Ungrouped.
    { id: 900, groupId: -1, windowId: 1, active: false, lastAccessed: NOW },
];

const rows = G.summarize(groups, tabs, windows);
const byId = Object.fromEntries(rows.map((r) => [r.id, r]));
const ids = (list) => list.map((g) => g.id).sort((a, b) => a - b);
const reasons = (plan) => Object.fromEntries(plan.skipped.map((s) => [s.group.id, s.reason]));
const throws = (fn, re) => {
    try {
        fn();
        return false;
    } catch (e) {
        return re.test(e.message);
    }
};

const checks = {};
checks['summary counts tabs and keeps their ids'] = byId[10].tabs === 2 && JSON.stringify(byId[10].tabIds) === '[100,101]';
checks['last used is the newest tab'] = byId[10].lastAccessed === NOW - 180 * MIN;
checks['no last-used time is null'] = byId[14].lastAccessed === null && byId[14].title === '';
checks['active in an unfocused window is active, not focused'] = byId[12].active && !byId[12].focused;
checks['active in the focused window is focused'] = byId[15].active && byId[15].focused;
checks['collapsed and color carried'] = byId[11].collapsed === true && byId[13].color === 'blue';

checks['title glob is whole-title and case-insensitive'] =
    G.titleMatches('claude', 'Claude') && G.titleMatches('Claude*', 'claude (2)') && !G.titleMatches('Claude', 'Claude (2)') && G.titleMatches('W?rk', 'Work');
checks['regex characters in a title are literal'] = G.titleMatches('claude (2)', 'Claude (2)') && !G.titleMatches('a.c', 'abc');

checks['a selector is required'] = throws(() => G.select(rows, {}, NOW), /say which groups/);
checks['idle must be positive'] = throws(() => G.select(rows, { idleMs: 0 }, NOW), /more than zero/);

const byTitle = G.select(rows, { title: 'Claude' }, NOW);
checks['exact title picks the matching groups, sparing the active one'] =
    JSON.stringify(ids(byTitle.selected)) === '[10,11]' && reasons(byTitle)[15] === 'active' && !(12 in reasons(byTitle));

const glob = G.select(rows, { title: 'claude*' }, NOW);
checks['a group with an unfocused window’s active tab is spared too'] = JSON.stringify(ids(glob.selected)) === '[10,11]' && reasons(glob)[12] === 'active';

const idle = G.select(rows, { title: 'Claude*', idleMs: 120 * MIN }, NOW);
checks['idle keeps only groups unused that long'] = JSON.stringify(ids(idle.selected)) === '[10]' && reasons(idle)[11] === 'recent';

const idleAlone = G.select(rows, { idleMs: 60 * MIN }, NOW);
checks['idle alone considers every group'] = JSON.stringify(ids(idleAlone.selected)) === '[10,13]';
checks['unknown last use is never idle'] = reasons(idleAlone)[14] === 'unknown';

const explicit = G.select(rows, { ids: [13, 15, 999] }, NOW);
checks['ids pick groups, the active one is still spared, unknown ids are reported'] =
    JSON.stringify(ids(explicit.selected)) === '[13]' && reasons(explicit)[15] === 'active' && JSON.stringify(explicit.missing) === '[999]';

const union = G.select(rows, { ids: [13], title: 'claude (2)' }, NOW);
checks['ids and title add up'] = JSON.stringify(ids(union.selected)) === '[13]' && reasons(union)[12] === 'active';

const all = G.select(rows, { all: true }, NOW);
checks['all takes everything but active groups'] =
    JSON.stringify(ids(all.selected)) === '[10,11,13,14]' && ids(all.skipped.map((s) => s.group)).join() === '12,15';

const untitled = G.select(rows, { title: '' }, NOW);
checks['an empty title matches untitled groups'] = JSON.stringify(ids(untitled.selected)) === '[14]';

const failed = Object.entries(checks).filter(([, ok]) => !ok);
for (const [name, ok] of Object.entries(checks)) console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}`);
if (failed.length) process.exit(1);
