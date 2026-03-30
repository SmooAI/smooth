/** Jira REST API client */

interface JiraConfig {
    url: string;
    email: string;
    apiToken: string;
    project: string;
}

interface JiraIssue {
    id: string;
    key: string;
    fields: {
        summary: string;
        description: unknown;
        status: { name: string; id: string };
        priority: { name: string; id: string };
        issuetype: { name: string };
        labels: string[];
        parent?: { key: string };
    };
}

interface JiraTransition {
    id: string;
    name: string;
    to: { name: string };
}

export class JiraClient {
    private config: JiraConfig;

    constructor(config: JiraConfig) {
        this.config = config;
    }

    private get auth(): string {
        return Buffer.from(`${this.config.email}:${this.config.apiToken}`).toString('base64');
    }

    private async request<T>(path: string, options: RequestInit = {}): Promise<T> {
        const response = await fetch(`${this.config.url}/rest/api/3${path}`, {
            ...options,
            headers: {
                Authorization: `Basic ${this.auth}`,
                'Content-Type': 'application/json',
                Accept: 'application/json',
                ...options.headers,
            },
        });

        if (!response.ok) {
            const body = await response.text().catch(() => '');
            throw new Error(`Jira API error ${response.status}: ${body}`);
        }

        return response.json() as Promise<T>;
    }

    async searchIssues(jql: string, maxResults = 50): Promise<JiraIssue[]> {
        const result = await this.request<{ issues: JiraIssue[] }>(`/search/jql?jql=${encodeURIComponent(jql)}&maxResults=${maxResults}`);
        return result.issues;
    }

    async getIssue(key: string): Promise<JiraIssue> {
        return this.request<JiraIssue>(`/issue/${key}`);
    }

    async createIssue(fields: {
        summary: string;
        description?: string;
        issuetype: string;
        priority?: string;
        labels?: string[];
        parent?: string;
    }): Promise<JiraIssue> {
        const body: Record<string, unknown> = {
            fields: {
                project: { key: this.config.project },
                summary: fields.summary,
                issuetype: { name: fields.issuetype },
            },
        };

        const f = body.fields as Record<string, unknown>;
        if (fields.description) {
            f.description = {
                type: 'doc',
                version: 1,
                content: [{ type: 'paragraph', content: [{ type: 'text', text: fields.description }] }],
            };
        }
        if (fields.priority) f.priority = { name: fields.priority };
        if (fields.labels) f.labels = fields.labels;
        if (fields.parent) f.parent = { key: fields.parent };

        return this.request<JiraIssue>('/issue', {
            method: 'POST',
            body: JSON.stringify(body),
        });
    }

    async updateIssue(key: string, fields: Record<string, unknown>): Promise<void> {
        await this.request(`/issue/${key}`, {
            method: 'PUT',
            body: JSON.stringify({ fields }),
        });
    }

    async getTransitions(key: string): Promise<JiraTransition[]> {
        const result = await this.request<{ transitions: JiraTransition[] }>(`/issue/${key}/transitions`);
        return result.transitions;
    }

    async transitionIssue(key: string, transitionId: string): Promise<void> {
        await this.request(`/issue/${key}/transitions`, {
            method: 'POST',
            body: JSON.stringify({ transition: { id: transitionId } }),
        });
    }

    async addComment(key: string, body: string): Promise<void> {
        await this.request(`/issue/${key}/comment`, {
            method: 'POST',
            body: JSON.stringify({
                body: { type: 'doc', version: 1, content: [{ type: 'paragraph', content: [{ type: 'text', text: body }] }] },
            }),
        });
    }

    async testConnection(): Promise<{ ok: boolean; error?: string }> {
        try {
            await this.searchIssues(`project=${this.config.project}`, 1);
            return { ok: true };
        } catch (error) {
            return { ok: false, error: error instanceof Error ? error.message : String(error) };
        }
    }
}
