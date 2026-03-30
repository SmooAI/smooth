/** Client-side API helper for calling the leader through Next.js rewrites */

const BASE = ''; // Proxied through next.config.ts rewrites

export async function api<T>(path: string, options?: RequestInit): Promise<T> {
    const response = await fetch(`${BASE}${path}`, {
        ...options,
        headers: {
            'Content-Type': 'application/json',
            ...options?.headers,
        },
    });

    if (!response.ok) {
        throw new Error(`API error ${response.status}: ${await response.text()}`);
    }

    return response.json() as Promise<T>;
}

export async function apiPost<T>(path: string, body: unknown): Promise<T> {
    return api<T>(path, {
        method: 'POST',
        body: JSON.stringify(body),
    });
}
