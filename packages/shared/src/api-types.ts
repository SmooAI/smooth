/** API request/response types (Zod-validated) */

import type { Bead, BeadDetail } from './beads-types.js';
import type { InboxItem, Message } from './message-types.js';
import type { Project, SmoothConfig, SystemHealth, User } from './types.js';
import type { Worker, WorkerDetail } from './worker-types.js';

// Generic API response wrapper
export interface ApiResponse<T> {
    data: T;
    ok: true;
}

export interface ApiError {
    error: string;
    ok: false;
    statusCode: number;
}

// Auth
export interface LoginResponse {
    apiKey: string;
    user: User;
}

export interface WhoamiResponse {
    user: User;
    server: string;
}

// Projects
export type ProjectListResponse = ApiResponse<Project[]>;
export type ProjectResponse = ApiResponse<Project>;

// Beads
export type BeadListResponse = ApiResponse<Bead[]>;
export type BeadResponse = ApiResponse<BeadDetail>;

// Workers
export type WorkerListResponse = ApiResponse<Worker[]>;
export type WorkerResponse = ApiResponse<WorkerDetail>;

// Messages
export type InboxResponse = ApiResponse<InboxItem[]>;
export type MessageListResponse = ApiResponse<Message[]>;

// Reviews
export interface Review {
    beadId: string;
    reviewBeadId: string;
    status: 'pending' | 'approved' | 'rejected' | 'rework';
    taskTitle: string;
    createdAt: string;
}
export type ReviewListResponse = ApiResponse<Review[]>;

// System
export type HealthResponse = ApiResponse<SystemHealth>;
export type ConfigResponse = ApiResponse<SmoothConfig>;

// Jira
export interface JiraSyncResult {
    pulled: number;
    pushed: number;
    conflicts: number;
}
export interface JiraSyncStatus {
    lastSync?: string;
    pendingChanges: number;
    conflicts: number;
    connected: boolean;
}

// Chat (streaming)
export interface ChatStreamEvent {
    type: 'text' | 'tool_call' | 'tool_result' | 'reasoning' | 'plan' | 'done' | 'error';
    content: string;
    metadata?: Record<string, unknown>;
}

// SSE events
export interface SSEEvent {
    type: 'worker_status' | 'bead_update' | 'message' | 'review_complete' | 'system_health';
    data: Record<string, unknown>;
    timestamp: string;
}
