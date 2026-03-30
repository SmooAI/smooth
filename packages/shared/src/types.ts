/** Core Smooth types */

export interface Project {
    id: string;
    name: string;
    description: string;
    status: 'active' | 'paused' | 'completed';
    createdAt: string;
    updatedAt: string;
}

export interface SystemHealth {
    leader: { status: 'healthy' | 'degraded' | 'down'; uptime: number };
    postgres: { status: 'healthy' | 'degraded' | 'down'; connectionCount: number };
    sandbox: { status: 'healthy' | 'degraded' | 'down'; backend: string; activeSandboxes: number; maxConcurrency: number };
    tailscale: { status: 'connected' | 'disconnected'; hostname?: string };
    beads: { status: 'healthy' | 'degraded' | 'down'; openIssues: number };
}

export interface SmoothConfig {
    jira?: {
        url: string;
        project: string;
        email: string;
    };
    smoo?: {
        apiUrl: string;
        orgId: string;
        clientId: string;
    };
    tailscale?: {
        tailnet: string;
        hostname: string;
    };
    providers?: {
        default: string;
    };
}

export interface User {
    id: string;
    email: string;
    name: string;
}
