import { createAuthClient as createBetterAuthClient } from 'better-auth/client';

export function createAuthClient(baseURL: string) {
    return createBetterAuthClient({
        baseURL,
    });
}
