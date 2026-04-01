const BASE = '';

export async function api<T>(path: string): Promise<T> {
    const resp = await fetch(`${BASE}${path}`);
    if (!resp.ok) throw new Error(`API ${resp.status}`);
    return resp.json() as Promise<T>;
}

export async function apiPost<T>(path: string, body: unknown): Promise<T> {
    const resp = await fetch(`${BASE}${path}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
    });
    if (!resp.ok) throw new Error(`API ${resp.status}`);
    return resp.json() as Promise<T>;
}
