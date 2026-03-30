/** OpenCode SDK integration for driving Smooth Operators programmatically */

interface OpenCodeSession {
    id: string;
    title: string;
}

interface OpenCodeMessage {
    role: 'user' | 'assistant' | 'tool';
    content: string;
}

interface OpenCodePromptResult {
    messages: OpenCodeMessage[];
    completed: boolean;
}

/**
 * Driver for communicating with an OpenCode instance running inside a Smooth Operator container.
 *
 * OpenCode runs in server mode (HTTP API at port 4096 inside the container).
 * The leader communicates via the Docker network.
 */
export class OperatorDriver {
    private baseUrl: string;
    private operatorId: string;

    constructor(operatorId: string, containerHost: string, port = 4096) {
        this.operatorId = operatorId;
        this.baseUrl = `http://${containerHost}:${port}`;
    }

    /** Create a new OpenCode session for a task */
    async createSession(title: string): Promise<OpenCodeSession> {
        const response = await this.request<OpenCodeSession>('/session', {
            method: 'POST',
            body: JSON.stringify({ title }),
        });
        console.log(`[driver] Created session ${response.id} for Smooth Operator ${this.operatorId}`);
        return response;
    }

    /** Send a prompt to the Smooth Operator and wait for completion */
    async prompt(sessionId: string, text: string): Promise<OpenCodePromptResult> {
        const response = await this.request<OpenCodePromptResult>(`/session/${sessionId}/prompt`, {
            method: 'POST',
            body: JSON.stringify({
                parts: [{ type: 'text', text }],
            }),
        });
        return response;
    }

    /** Get messages from a session */
    async getMessages(sessionId: string): Promise<OpenCodeMessage[]> {
        return this.request<OpenCodeMessage[]>(`/session/${sessionId}/messages`);
    }

    /** Abort an in-progress prompt */
    async abort(sessionId: string): Promise<void> {
        await this.request(`/session/${sessionId}/abort`, { method: 'POST' });
    }

    /** Check if the OpenCode instance is healthy */
    async healthCheck(): Promise<boolean> {
        try {
            await this.request('/health');
            return true;
        } catch {
            return false;
        }
    }

    /** Wait for the OpenCode instance to be ready (with retries) */
    async waitForReady(maxRetries = 30, intervalMs = 2000): Promise<boolean> {
        for (let i = 0; i < maxRetries; i++) {
            if (await this.healthCheck()) {
                console.log(`[driver] Smooth Operator ${this.operatorId} is ready`);
                return true;
            }
            await new Promise((resolve) => setTimeout(resolve, intervalMs));
        }
        console.error(`[driver] Smooth Operator ${this.operatorId} failed to become ready`);
        return false;
    }

    private async request<T>(path: string, options: RequestInit = {}): Promise<T> {
        const response = await fetch(`${this.baseUrl}${path}`, {
            ...options,
            headers: {
                'Content-Type': 'application/json',
                ...options.headers,
            },
        });

        if (!response.ok) {
            const body = await response.text().catch(() => '');
            throw new Error(`OpenCode API error ${response.status}: ${body}`);
        }

        return response.json() as Promise<T>;
    }
}

/** Create a driver for a Smooth Operator container */
export function createDriver(operatorId: string, containerId: string): OperatorDriver {
    // In Docker network, containers are reachable by name
    const containerHost = `smooth-operator-${operatorId}`;
    return new OperatorDriver(operatorId, containerHost);
}
