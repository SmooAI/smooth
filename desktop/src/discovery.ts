//! Discover Big Smooth daemons on the Tailscale tailnet.
//!
//! A daemon exposes itself over the tailnet with `tailscale serve` (see
//! smooth-daemon/src/tailscale.rs) at `https://<peer>.<tailnet>.ts.net` — on
//! :443 by default, or :8443 where it shares a host that already serves :443
//! (smoo-hub). We enumerate online tailnet peers (`tailscale status --json`) and
//! probe each on those ports for the ungated `/health` (is a daemon here?) and
//! `/api/auth/status` (whose daemon — the signed-in Smoo identity).
//!
//! Connecting needs nothing more than the URL: a remote daemon serves its own
//! SPA with its own token injected, so pointing the window at it authenticates
//! automatically (see daemon.ts / main.ts).

import { execFile } from 'node:child_process';
import { existsSync } from 'node:fs';

export interface RemoteDaemon {
    /** Short tailnet name, e.g. "smoo-hub". */
    name: string;
    /** Full base URL, e.g. "https://smoo-hub.tailnet.ts.net:8443". */
    url: string;
    /** Signed-in Smoo identity of that daemon, if it reports one. */
    identity?: string;
}

// GUI apps get a sparse PATH, so probe the usual install locations before PATH.
const TAILSCALE_CANDIDATES = ['/Applications/Tailscale.app/Contents/MacOS/Tailscale', '/usr/local/bin/tailscale', '/opt/homebrew/bin/tailscale', 'tailscale'];
// serve default (443) + the smoo-hub coexistence port (8443).
const PROBE_PORTS = [443, 8443];
const PROBE_TIMEOUT_MS = 1500;

function tailscaleBin(): string | undefined {
    return TAILSCALE_CANDIDATES.find((p) => p === 'tailscale' || existsSync(p));
}

interface PeerStatus {
    DNSName?: string;
    HostName?: string;
    Online?: boolean;
}

async function statusJson(): Promise<{ Peer?: Record<string, PeerStatus> } | null> {
    const bin = tailscaleBin();
    if (!bin) return null;
    return new Promise((resolve) => {
        execFile(bin, ['status', '--json'], { timeout: 5000 }, (err, stdout) => {
            if (err) return resolve(null);
            try {
                resolve(JSON.parse(stdout) as { Peer?: Record<string, PeerStatus> });
            } catch {
                resolve(null);
            }
        });
    });
}

/** Probe one peer for a Big Smooth daemon; returns it (first responding port) or null. */
async function probePeer(peer: PeerStatus): Promise<RemoteDaemon | null> {
    const dns = (peer.DNSName ?? '').replace(/\.$/, '');
    if (dns === '') return null;
    const name = peer.HostName || dns.split('.')[0];
    for (const port of PROBE_PORTS) {
        const url = `https://${dns}:${port}`;
        try {
            const health = await fetch(`${url}/health`, { signal: AbortSignal.timeout(PROBE_TIMEOUT_MS) });
            if (!health.ok) continue;
            let identity: string | undefined;
            try {
                const status = await fetch(`${url}/api/auth/status`, { signal: AbortSignal.timeout(PROBE_TIMEOUT_MS) });
                if (status.ok) {
                    const j = (await status.json()) as { user?: string; org_id?: string };
                    identity = j.user ?? j.org_id ?? undefined;
                }
            } catch {
                // identity is best-effort; a reachable /health is enough to list it.
            }
            return { name, url, identity };
        } catch {
            // not this port — try the next
        }
    }
    return null;
}

/** Online tailnet peers running a Big Smooth daemon. Excludes this machine (Tailscale
 * lists Self separately from Peer). Empty if Tailscale isn't installed/up. */
export async function discoverDaemons(): Promise<RemoteDaemon[]> {
    const status = await statusJson();
    if (!status?.Peer) return [];
    const online = Object.values(status.Peer).filter((p) => p.Online && p.DNSName);
    const probed = await Promise.all(online.map(probePeer));
    return probed.filter((d): d is RemoteDaemon => d !== null).sort((a, b) => a.name.localeCompare(b.name));
}
