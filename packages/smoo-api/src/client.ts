/** Type-safe SmooAI API client */

import { SmooAuth, type SmooConfig } from './auth.js';

export class SmooClient {
    private auth: SmooAuth;
    private config: SmooConfig;

    constructor(config: SmooConfig) {
        this.config = config;
        this.auth = new SmooAuth(config);
    }

    private async request<T>(path: string, options: RequestInit = {}): Promise<T> {
        const headers = await this.auth.authHeaders();
        const response = await fetch(`${this.config.apiUrl}${path}`, {
            ...options,
            headers: {
                'Content-Type': 'application/json',
                ...headers,
                ...options.headers,
            },
        });

        if (!response.ok) {
            const body = await response.text().catch(() => '');
            throw new Error(`SmooAI API error ${response.status}: ${body}`);
        }

        return response.json() as Promise<T>;
    }

    // Organizations
    orgs = {
        list: () => this.request<unknown[]>(`/organizations`),
        get: (id: string) => this.request<unknown>(`/organizations/${id}`),
    };

    // Agents
    agents = {
        list: (orgId: string) => this.request<unknown[]>(`/organizations/${orgId}/agents`),
        get: (orgId: string, agentId: string) => this.request<unknown>(`/organizations/${orgId}/agents/${agentId}`),
    };

    // Knowledge
    knowledge = {
        list: (orgId: string) => this.request<unknown[]>(`/organizations/${orgId}/knowledge`),
    };

    // Jobs
    jobs = {
        list: (orgId: string) => this.request<unknown[]>(`/organizations/${orgId}/jobs`),
        get: (orgId: string, jobId: string) => this.request<unknown>(`/organizations/${orgId}/jobs/${jobId}`),
    };

    // Test Cases
    tests = {
        list: (orgId: string) => this.request<unknown[]>(`/organizations/${orgId}/test-cases`),
    };

    // Config
    configValues = {
        list: (orgId: string, envId: string) => this.request<unknown[]>(`/organizations/${orgId}/config-environments/${envId}/config-values`),
    };

    // Integrations
    integrations = {
        list: (orgId: string) => this.request<unknown[]>(`/organizations/${orgId}/integrations`),
    };

    // Auth Clients
    authClients = {
        list: (orgId: string) => this.request<unknown[]>(`/organizations/${orgId}/auth-clients`),
    };

    /** Test connection by listing organizations */
    async testConnection(): Promise<{ ok: boolean; error?: string }> {
        try {
            await this.orgs.list();
            return { ok: true };
        } catch (error) {
            return { ok: false, error: error instanceof Error ? error.message : String(error) };
        }
    }
}
