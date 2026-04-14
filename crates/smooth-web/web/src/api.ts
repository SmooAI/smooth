const BASE = '';

export async function api<T>(path: string, init?: RequestInit): Promise<T> {
    const opts: RequestInit = init ?? {};
    if (opts.body && !opts.headers) {
        opts.headers = { 'Content-Type': 'application/json' };
    }
    const resp = await fetch(`${BASE}${path}`, opts);
    if (!resp.ok) throw new Error(`API ${resp.status}`);
    // DELETE + other empty responses: return the raw Response's json
    // only if there's something to parse.
    const text = await resp.text();
    if (!text) return undefined as unknown as T;
    return JSON.parse(text) as T;
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
