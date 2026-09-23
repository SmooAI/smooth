// Which computer this window drives (pearl th-a49e21). Big Smooth runs on each
// of your Macs; the window always loads from the daemon on THIS computer, and
// can drive another one of yours over the Smoo Relay: the local daemon tunnels
// the operator WebSocket and the few REST routes the SPA reads to it, under
// `/api/relay/peers/<device>`. So "drive smoo-hub" is nothing more than a
// different API base — every `${http}/…` call and the `/ws` socket follow it.
//
// Pure (no React, no DOM beyond what's passed in) so the rules are tested with
// `node --test`: which computer is active after a reload, the fallback when the
// remembered one is unreachable, and what the switcher shows.

/** One of the user's other Big Smooth computers on the relay. */
export interface RelayPeer {
    device: string;
    label: string;
    kind: string;
}

/** `GET /api/relay/peers` (smooth-daemon `relay_peers_route`). */
export interface PeersResponse {
    self: { device: string; label: string };
    relay: { state: string; detail: string; since?: string };
    peers: RelayPeer[];
    error: string | null;
}

/** What the window remembers across reloads: the computer it last picked. */
export interface Remembered {
    device: string;
    label: string;
}

export type ActiveComputer = { kind: 'local' } | { kind: 'remote'; device: string; label: string };

export const COMPUTER_KEY = 'smooth.computer';

/** The relay's device-id grammar. Anything else never reaches a URL. */
const DEVICE_RE = /^[A-Za-z0-9._-]{1,64}$/;

export function validDevice(device: unknown): device is string {
    return typeof device === 'string' && DEVICE_RE.test(device);
}

type KV = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;

/** The remembered computer, or null (this computer). Junk reads as null. */
export function readRemembered(storage: KV): Remembered | null {
    try {
        const raw = storage.getItem(COMPUTER_KEY);
        if (!raw) return null;
        const v = JSON.parse(raw) as Partial<Remembered>;
        if (!validDevice(v.device)) return null;
        return { device: v.device, label: typeof v.label === 'string' && v.label.trim() ? v.label.trim() : v.device };
    } catch {
        return null;
    }
}

/** Remember a pick; `null` = this computer. */
export function writeRemembered(storage: KV, pick: Remembered | null): void {
    try {
        if (pick && validDevice(pick.device)) storage.setItem(COMPUTER_KEY, JSON.stringify({ device: pick.device, label: pick.label }));
        else storage.removeItem(COMPUTER_KEY);
    } catch {
        /* storage unavailable — the pick just doesn't survive a reload */
    }
}

/** Parse a peers response defensively (it crosses a process boundary). */
export function parsePeers(v: unknown): PeersResponse | null {
    if (!v || typeof v !== 'object') return null;
    const o = v as Record<string, unknown>;
    const self = o.self as Record<string, unknown> | undefined;
    const relay = o.relay as Record<string, unknown> | undefined;
    if (!self || typeof self.device !== 'string' || typeof self.label !== 'string') return null;
    if (!relay || typeof relay.state !== 'string') return null;
    const peers = Array.isArray(o.peers)
        ? o.peers.flatMap((p): RelayPeer[] => {
              const r = p as Record<string, unknown>;
              if (!validDevice(r?.device)) return [];
              const label = typeof r.label === 'string' && r.label.trim() ? r.label.trim() : r.device;
              return [{ device: r.device, label, kind: typeof r.kind === 'string' ? r.kind : 'daemon' }];
          })
        : [];
    return {
        self: { device: self.device, label: self.label },
        relay: {
            state: relay.state,
            detail: typeof relay.detail === 'string' ? relay.detail : '',
            since: typeof relay.since === 'string' ? relay.since : undefined,
        },
        peers: peers.filter((p) => p.kind === 'daemon' && p.device !== self.device),
        error: typeof o.error === 'string' ? o.error : null,
    };
}

/** Why remote computers can't be reached right now, in words — or null when
 * the relay is fine. `resp = null` means this computer's Big Smooth couldn't
 * answer at all (too old, or down). */
export function relayReason(resp: PeersResponse | null): string | null {
    if (!resp) return "This computer's Big Smooth can't list your other computers — it may need an update.";
    if (resp.relay.state === 'online' && !resp.error) return null;
    switch (resp.relay.state) {
        case 'signed_out':
            return 'Sign in to Smoo on this computer to reach your other computers.';
        case 'session_expired':
            return 'Your Smoo session expired — sign in again to reach your other computers.';
        case 'disabled':
            return 'The Smoo Relay is turned off on this computer.';
        case 'connecting':
        case 'authenticating':
            return 'Connecting to the Smoo Relay…';
        default:
            return resp.error || resp.relay.detail || "The Smoo Relay isn't reachable right now.";
    }
}

/** Decide which computer the window drives on load. A remembered remote wins
 * only when the relay lists it as online right now; otherwise the window comes
 * up on THIS computer — it always works — with a line saying why. */
export function resolveComputer(remembered: Remembered | null, resp: PeersResponse | null): { active: ActiveComputer; fallback: string | null } {
    if (!remembered) return { active: { kind: 'local' }, fallback: null };
    const peer = resp?.peers.find((p) => p.device === remembered.device);
    if (peer) return { active: { kind: 'remote', device: peer.device, label: peer.label }, fallback: null };
    const why = relayReason(resp) ?? 'it is offline, asleep, or signed out of Smoo.';
    return { active: { kind: 'local' }, fallback: `${remembered.label} isn't reachable — ${why} Showing this computer.` };
}

/** The API base for the active computer: this daemon, or this daemon's relay
 * tunnel to the remote one. */
export function apiBase(localBase: string, active: ActiveComputer): string {
    const base = localBase.replace(/\/$/, '');
    if (active.kind === 'local' || !validDevice(active.device)) return base;
    return `${base}/api/relay/peers/${encodeURIComponent(active.device)}`;
}

/** One row of the switcher. `device = null` is this computer. */
export interface ComputerRow {
    device: string | null;
    name: string;
    detail: string;
    online: boolean;
    active: boolean;
}

/** The switcher's rows: this computer first, then every online remote, then
 * the remembered one if it has dropped off the list (shown offline, so its
 * absence is explained rather than silent). */
export function computerRows(resp: PeersResponse | null, active: ActiveComputer, remembered: Remembered | null): ComputerRow[] {
    const rows: ComputerRow[] = [
        {
            device: null,
            name: resp?.self.label || 'This computer',
            detail: 'This computer',
            online: true,
            active: active.kind === 'local',
        },
    ];
    for (const p of resp?.peers ?? []) {
        rows.push({
            device: p.device,
            name: p.label,
            detail: 'Online · via Smoo Relay',
            online: true,
            active: active.kind === 'remote' && active.device === p.device,
        });
    }
    const missing = [active.kind === 'remote' ? { device: active.device, label: active.label } : null, remembered].filter(
        (r): r is Remembered => !!r && !rows.some((row) => row.device === r.device),
    );
    for (const r of missing) {
        if (rows.some((row) => row.device === r.device)) continue;
        rows.push({
            device: r.device,
            name: r.label,
            detail: relayReason(resp) ?? 'Offline',
            online: false,
            active: active.kind === 'remote' && active.device === r.device,
        });
    }
    return rows;
}

/** The window title for the active computer. */
export function windowTitle(base: string, active: ActiveComputer): string {
    return active.kind === 'remote' ? `Big Smooth — ${active.label}` : base;
}

// ── the window's current computer (set once at boot, read by resolveTarget) ──

let current: ActiveComputer = { kind: 'local' };
let bootNotice: string | null = null;

export function setActiveComputer(active: ActiveComputer, notice: string | null = null): void {
    current = active;
    bootNotice = notice;
}

export function activeComputer(): ActiveComputer {
    return current;
}

/** Why the window didn't come up on the remembered computer, if it didn't. */
export function computerNotice(): string | null {
    return bootNotice;
}

/** Ask THIS computer's daemon for the switcher's data. Null on any failure. */
export async function fetchPeers(localBase: string, token: string, timeoutMs = 4000): Promise<PeersResponse | null> {
    try {
        const r = await fetch(`${localBase.replace(/\/$/, '')}/api/relay/peers`, {
            headers: token ? { authorization: `Bearer ${token}` } : {},
            signal: AbortSignal.timeout(timeoutMs),
        });
        if (!r.ok) return null;
        return parsePeers(await r.json());
    } catch {
        return null;
    }
}
