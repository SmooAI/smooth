/** Leader API client — shared between TUI and CLI commands */

import type {
    ApiResponse,
    BeadListResponse,
    BeadResponse,
    ConfigResponse,
    HealthResponse,
    InboxResponse,
    JiraSyncResult,
    JiraSyncStatus,
    ProjectListResponse,
    ProjectResponse,
    ReviewListResponse,
    WorkerListResponse,
    WorkerResponse,
} from '@smooai/smooth-shared/api-types';

export class LeaderClient {
    private baseUrl: string;
    private apiKey: string | null;

    constructor(baseUrl: string, apiKey?: string | null) {
        this.baseUrl = baseUrl;
        this.apiKey = apiKey ?? null;
    }

    private async request<T>(path: string, options: RequestInit = {}): Promise<T> {
        const headers: Record<string, string> = {
            'Content-Type': 'application/json',
        };
        if (this.apiKey) {
            headers['x-api-key'] = this.apiKey;
        }

        const response = await fetch(`${this.baseUrl}${path}`, {
            ...options,
            headers: { ...headers, ...options.headers },
        });

        if (!response.ok) {
            const body = await response.text().catch(() => '');
            throw new Error(`Leader API error ${response.status}: ${body}`);
        }

        return response.json() as Promise<T>;
    }

    // ── Auth ────────────────────────────────────────────────

    async whoami(): Promise<{ user: { email: string; name: string }; server: string }> {
        await this.request<HealthResponse>('/health');
        return { user: { email: 'user@smooth.local', name: 'User' }, server: this.baseUrl };
    }

    // ── Health ──────────────────────────────────────────────

    async getHealth(): Promise<HealthResponse> {
        return this.request<HealthResponse>('/health');
    }

    // ── Projects ────────────────────────────────────────────

    async listProjects(): Promise<ProjectListResponse> {
        return this.request<ProjectListResponse>('/api/projects');
    }

    async getProject(id: string): Promise<ProjectResponse> {
        return this.request<ProjectResponse>(`/api/projects/${id}`);
    }

    async createProject(name: string, description: string): Promise<ProjectResponse> {
        return this.request<ProjectResponse>('/api/projects', {
            method: 'POST',
            body: JSON.stringify({ name, description }),
        });
    }

    // ── Beads ───────────────────────────────────────────────

    async listBeads(filters?: Record<string, string>): Promise<BeadListResponse> {
        const params = filters ? '?' + new URLSearchParams(filters).toString() : '';
        return this.request<BeadListResponse>(`/api/beads${params}`);
    }

    async getBead(id: string): Promise<BeadResponse> {
        return this.request<BeadResponse>(`/api/beads/${id}`);
    }

    async getReadyBeads(): Promise<BeadListResponse> {
        return this.request<BeadListResponse>('/api/beads/ready');
    }

    async searchBeads(query: string): Promise<BeadListResponse> {
        return this.request<BeadListResponse>(`/api/beads/search?q=${encodeURIComponent(query)}`);
    }

    // ── Smooth Operators ────────────────────────────────────

    async listOperators(): Promise<WorkerListResponse> {
        return this.request<WorkerListResponse>('/api/workers');
    }

    async getOperator(id: string): Promise<WorkerResponse> {
        return this.request<WorkerResponse>(`/api/workers/${id}`);
    }

    async killOperator(id: string): Promise<void> {
        await this.request(`/api/workers/${id}`, { method: 'DELETE' });
    }

    // ── Steering ────────────────────────────────────────────

    async pauseOperator(beadId: string): Promise<void> {
        await this.request(`/api/steering/${beadId}/pause`, { method: 'POST' });
    }

    async steerOperator(beadId: string, message: string): Promise<void> {
        await this.request(`/api/steering/${beadId}/steer`, { method: 'POST', body: JSON.stringify({ message }) });
    }

    async resumeOperator(beadId: string): Promise<void> {
        await this.request(`/api/steering/${beadId}/resume`, { method: 'POST' });
    }

    async cancelOperator(beadId: string): Promise<void> {
        await this.request(`/api/steering/${beadId}/cancel`, { method: 'POST' });
    }

    // ── Messages ────────────────────────────────────────────

    async getInbox(): Promise<InboxResponse> {
        return this.request<InboxResponse>('/api/messages/inbox');
    }

    async getMessages(beadId: string): Promise<ApiResponse<unknown[]>> {
        return this.request<ApiResponse<unknown[]>>(`/api/messages/${beadId}`);
    }

    async sendMessage(beadId: string, content: string, direction = 'human→leader'): Promise<void> {
        await this.request('/api/messages', {
            method: 'POST',
            body: JSON.stringify({ beadId, content, direction }),
        });
    }

    // ── Reviews ─────────────────────────────────────────────

    async getPendingReviews(): Promise<ReviewListResponse> {
        return this.request<ReviewListResponse>('/api/reviews');
    }

    async approveReview(beadId: string): Promise<void> {
        await this.request(`/api/reviews/${beadId}/approve`, { method: 'POST' });
    }

    async rejectReview(beadId: string, reason: string): Promise<void> {
        await this.request(`/api/reviews/${beadId}/reject`, {
            method: 'POST',
            body: JSON.stringify({ reason }),
        });
    }

    async requestRework(beadId: string, feedback: string): Promise<void> {
        await this.request(`/api/reviews/${beadId}/rework`, {
            method: 'POST',
            body: JSON.stringify({ feedback }),
        });
    }

    // ── System ──────────────────────────────────────────────

    async getSystemHealth(): Promise<HealthResponse> {
        return this.request<HealthResponse>('/api/system/health');
    }

    async getConfig(): Promise<ConfigResponse> {
        return this.request<ConfigResponse>('/api/system/config');
    }

    async setConfig(key: string, value: unknown): Promise<void> {
        await this.request('/api/system/config', {
            method: 'PUT',
            body: JSON.stringify({ key, value }),
        });
    }

    // ── Jira ────────────────────────────────────────────────

    async jiraSync(direction?: 'pull' | 'push'): Promise<ApiResponse<JiraSyncResult>> {
        return this.request<ApiResponse<JiraSyncResult>>('/api/jira/sync', {
            method: 'POST',
            body: JSON.stringify({ direction }),
        });
    }

    async jiraStatus(): Promise<ApiResponse<JiraSyncStatus>> {
        return this.request<ApiResponse<JiraSyncStatus>>('/api/jira/status');
    }

    // ── Chat ────────────────────────────────────────────────

    async chat(content: string, attachments?: Array<{ type: string; reference: string }>): Promise<Response> {
        const headers: Record<string, string> = { 'Content-Type': 'application/json' };
        if (this.apiKey) headers['x-api-key'] = this.apiKey;

        return fetch(`${this.baseUrl}/api/chat`, {
            method: 'POST',
            headers,
            body: JSON.stringify({ content, attachments }),
        });
    }

    // ── SSE Stream ──────────────────────────────────────────

    subscribe(): EventSource {
        const url = `${this.baseUrl}/api/stream`;
        // EventSource doesn't support custom headers natively.
        // In a real implementation, we'd use a polyfill or fetch-based SSE.
        return new EventSource(url);
    }
}
