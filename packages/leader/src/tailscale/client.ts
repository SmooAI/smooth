/** Tailscale REST API client for service discovery, auth key generation, and health */

import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

const exec = promisify(execFile);

export interface TailscaleStatus {
    connected: boolean;
    hostname?: string;
    tailnet?: string;
    ip?: string;
    version?: string;
}

export interface TailscaleAuthKey {
    key: string;
    expires: string;
}

/**
 * Check if Tailscale is installed and connected.
 * Works by running `tailscale status --json`.
 */
export async function getTailscaleStatus(): Promise<TailscaleStatus> {
    try {
        const { stdout } = await exec('tailscale', ['status', '--json']);
        const status = JSON.parse(stdout);

        return {
            connected: status.BackendState === 'Running',
            hostname: status.Self?.HostName,
            tailnet: status.MagicDNSSuffix,
            ip: status.TailscaleIPs?.[0],
            version: status.Version,
        };
    } catch {
        return { connected: false };
    }
}

/**
 * Get the Tailscale DNS name for a given hostname on the tailnet.
 * Returns null if Tailscale is not available.
 */
export async function resolveTailscaleHost(hostname: string): Promise<string | null> {
    const status = await getTailscaleStatus();
    if (!status.connected || !status.tailnet) return null;
    return `${hostname}.${status.tailnet}`;
}

/**
 * Check if a service is reachable via Tailscale Serve.
 * Looks for smooth-* hostnames on the tailnet.
 */
export async function discoverSmoothServices(): Promise<{ web?: string; api?: string }> {
    const status = await getTailscaleStatus();
    if (!status.connected || !status.tailnet) return {};

    const services: { web?: string; api?: string } = {};

    try {
        const { stdout } = await exec('tailscale', ['status', '--json']);
        const parsed = JSON.parse(stdout);
        const peers = parsed.Peer ?? {};

        for (const peer of Object.values(peers) as any[]) {
            const hostName = peer.HostName?.toLowerCase();
            if (hostName === 'smooth') {
                services.web = `https://smooth.${status.tailnet}`;
            }
            if (hostName === 'smooth-api') {
                services.api = `https://smooth-api.${status.tailnet}`;
            }
        }
    } catch {
        // Tailscale not available
    }

    return services;
}

/**
 * Create an ephemeral auth key for registering a new Smooth Operator node.
 * Requires Tailscale API key configured in environment.
 */
export async function createAuthKey(opts?: { tags?: string[]; ephemeral?: boolean }): Promise<TailscaleAuthKey | null> {
    const apiKey = process.env.TAILSCALE_API_KEY;
    const tailnet = process.env.TAILSCALE_TAILNET;

    if (!apiKey || !tailnet) return null;

    try {
        const response = await fetch(`https://api.tailscale.com/api/v2/tailnet/${tailnet}/keys`, {
            method: 'POST',
            headers: {
                Authorization: `Bearer ${apiKey}`,
                'Content-Type': 'application/json',
            },
            body: JSON.stringify({
                capabilities: {
                    devices: {
                        create: {
                            reusable: false,
                            ephemeral: opts?.ephemeral ?? true,
                            preauthorized: true,
                            tags: opts?.tags ?? ['tag:smooth-worker'],
                        },
                    },
                },
                expirySeconds: 3600,
            }),
        });

        if (!response.ok) return null;

        const data = (await response.json()) as { key: string; expires: string };
        return { key: data.key, expires: data.expires };
    } catch {
        return null;
    }
}

/**
 * Get identity headers from a Tailscale Serve request.
 * These are injected automatically by Tailscale Serve.
 */
export function extractTailscaleIdentity(headers: Record<string, string | undefined>): {
    login?: string;
    name?: string;
    profilePic?: string;
} | null {
    const login = headers['tailscale-user-login'];
    if (!login) return null;

    return {
        login,
        name: headers['tailscale-user-name'],
        profilePic: headers['tailscale-user-profile-pic'],
    };
}
