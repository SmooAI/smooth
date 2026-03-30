/** SmooAI M2M OAuth client_credentials authentication */

export interface SmooConfig {
    apiUrl: string;
    clientId: string;
    clientSecret: string;
    orgId: string;
}

interface TokenResponse {
    access_token: string;
    token_type: string;
    expires_in: number;
}

export class SmooAuth {
    private config: SmooConfig;
    private token: string | null = null;
    private tokenExpiresAt: number = 0;

    constructor(config: SmooConfig) {
        this.config = config;
    }

    async getToken(): Promise<string> {
        // Return cached token if still valid (with 60s buffer)
        if (this.token && Date.now() < this.tokenExpiresAt - 60_000) {
            return this.token;
        }

        const response = await fetch(`${this.config.apiUrl}/token`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
            body: new URLSearchParams({
                grant_type: 'client_credentials',
                client_id: this.config.clientId,
                client_secret: this.config.clientSecret,
            }),
        });

        if (!response.ok) {
            throw new Error(`SmooAI auth failed: ${response.status} ${response.statusText}`);
        }

        const data = (await response.json()) as TokenResponse;
        this.token = data.access_token;
        this.tokenExpiresAt = Date.now() + data.expires_in * 1000;

        return this.token;
    }

    async authHeaders(): Promise<Record<string, string>> {
        const token = await this.getToken();
        return { Authorization: `Bearer ${token}` };
    }
}
