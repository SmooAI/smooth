/** Auth middleware — validates Better Auth sessions, API keys, or Tailscale identity headers */

import type { Context, Next } from 'hono';

interface AuthContext {
    userId: string;
    email: string;
    name: string;
    method: 'session' | 'api-key' | 'tailscale';
}

/**
 * Authentication middleware for the leader API.
 *
 * Checks in order:
 * 1. Tailscale identity headers (Tailscale-User-Login) — zero-config on tailnet
 * 2. x-api-key header — Better Auth API key for CLI clients
 * 3. Authorization: Bearer — Better Auth session token
 *
 * In v1, we use a lightweight approach. Full Better Auth validation
 * will be wired in when the auth package is fully integrated.
 */
export async function authMiddleware(c: Context, next: Next) {
    let authCtx: AuthContext | null = null;

    // 1. Tailscale identity headers (injected by Tailscale Serve)
    const tsLogin = c.req.header('Tailscale-User-Login');
    const tsName = c.req.header('Tailscale-User-Name');
    if (tsLogin) {
        authCtx = {
            userId: tsLogin,
            email: tsLogin,
            name: tsName ?? tsLogin,
            method: 'tailscale',
        };
    }

    // 2. API key (x-api-key header)
    if (!authCtx) {
        const apiKey = c.req.header('x-api-key');
        if (apiKey) {
            // TODO: Validate against Better Auth API key store
            // For now, accept any key prefixed with sk_smooth_
            if (apiKey.startsWith('sk_smooth_')) {
                authCtx = {
                    userId: 'api-key-user',
                    email: 'api@smooth.local',
                    name: 'API Key User',
                    method: 'api-key',
                };
            }
        }
    }

    // 3. Bearer token
    if (!authCtx) {
        const authHeader = c.req.header('Authorization');
        if (authHeader?.startsWith('Bearer ')) {
            // TODO: Validate via Better Auth session
            authCtx = {
                userId: 'session-user',
                email: 'user@smooth.local',
                name: 'Session User',
                method: 'session',
            };
        }
    }

    if (!authCtx) {
        // In local dev, allow unauthenticated access
        if (process.env.NODE_ENV !== 'production') {
            authCtx = {
                userId: 'dev-user',
                email: 'dev@smooth.local',
                name: 'Dev User',
                method: 'session',
            };
        } else {
            return c.json({ error: 'Unauthorized', ok: false, statusCode: 401 }, 401);
        }
    }

    c.set('auth', authCtx);
    await next();
}

export type { AuthContext };
