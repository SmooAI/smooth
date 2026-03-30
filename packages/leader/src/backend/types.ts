/** Backend-agnostic execution interfaces for Smooth Operator sandboxes */

import type { ToolPermission, WorkerPhase } from '@smooai/smooth-shared/worker-types';

/** Opaque handle to a running sandbox. Backend-agnostic. */
export interface SandboxHandle {
    sandboxId: string;
    operatorId: string;
    beadId: string;
    /** Backend-specific metadata (microsandbox VM name, Lambda ARN, etc.) */
    backendRef: Record<string, unknown>;
    createdAt: Date;
    timeoutAt: Date;
}

/** Configuration for creating a new sandbox */
export interface SandboxConfig {
    operatorId: string;
    beadId: string;
    workspacePath: string;
    permissions: ToolPermission[];
    systemPrompt?: string;
    model?: string;
    phase: WorkerPhase;
    env?: Record<string, string>;
    resourceLimits?: {
        cpus?: number;
        memoryMb?: number;
    };
    timeoutSeconds: number;
}

/** Result of prompting an agent inside a sandbox */
export interface PromptResult {
    messages: Array<{ role: 'user' | 'assistant' | 'tool'; content: string }>;
    completed: boolean;
}

/** Status of a running sandbox */
export interface SandboxStatus {
    running: boolean;
    healthy: boolean;
    phase: WorkerPhase;
    uptimeMs: number;
}

/**
 * The primary execution interface.
 *
 * Every execution backend (local microsandbox, future AWS Lambda) implements this.
 * The orchestration layer (LangGraph nodes, pool, lifecycle) calls ONLY these methods.
 * No Docker, no shell sessions, no host-specific details leak through.
 */
export interface ExecutionBackend {
    readonly name: string;

    // Lifecycle
    initialize(): Promise<void>;
    shutdown(): Promise<void>;

    // Sandbox management
    createSandbox(config: SandboxConfig): Promise<SandboxHandle>;
    destroySandbox(sandboxId: string): Promise<void>;
    getSandboxStatus(sandboxId: string): Promise<SandboxStatus>;
    listSandboxes(): Promise<SandboxHandle[]>;

    // Execution
    prompt(sandboxId: string, sessionId: string, text: string): Promise<PromptResult>;
    createSession(sandboxId: string, title: string): Promise<{ id: string; title: string }>;
    abort(sandboxId: string, sessionId: string): Promise<void>;

    // Observability
    getLogs(sandboxId: string, tail?: number): Promise<string>;
    healthCheck(): Promise<{ healthy: string[]; unhealthy: string[] }>;

    // Capacity
    hasCapacity(): boolean;
    activeCount(): number;
    maxConcurrency(): number;

    // Artifacts
    collectArtifacts(sandboxId: string): Promise<string[]>;

    // Timeout enforcement
    enforceTimeouts(): Promise<string[]>;
}

/** Durable artifact storage (local filesystem or S3) */
export interface ArtifactStore {
    put(beadId: string, name: string, content: Buffer | string): Promise<string>;
    get(key: string): Promise<Buffer>;
    list(beadId: string): Promise<string[]>;
    delete(key: string): Promise<void>;
}

/** Execution events for observability and SSE streaming */
export interface ExecutionEvent {
    type:
        | 'sandbox_created'
        | 'sandbox_destroyed'
        | 'phase_started'
        | 'phase_completed'
        | 'prompt_sent'
        | 'prompt_completed'
        | 'timeout'
        | 'error';
    sandboxId: string;
    operatorId: string;
    beadId?: string;
    data: Record<string, unknown>;
    timestamp: Date;
}

export type EventListener = (event: ExecutionEvent) => void;

/** Event stream for execution lifecycle events */
export interface EventStream {
    emit(event: ExecutionEvent): void;
    on(type: ExecutionEvent['type'] | '*', listener: EventListener): () => void;
}
