/** Backend registry — resolves and manages the active ExecutionBackend */

import type { EventStream, ExecutionBackend } from './types.js';

import { LocalEventStream } from './local-event-stream.js';

export type BackendType = 'local-microsandbox' | 'aws-lambda';

let _backend: ExecutionBackend | null = null;
let _events: EventStream | null = null;

/** Get the active execution backend. Throws if not initialized. */
export function getBackend(): ExecutionBackend {
    if (!_backend) {
        throw new Error('Execution backend not initialized. Call initializeBackend() first.');
    }
    return _backend;
}

/** Get the event stream. Creates a local one if not set. */
export function getEventStream(): EventStream {
    if (!_events) {
        _events = new LocalEventStream();
    }
    return _events;
}

/** Initialize the execution backend based on type. Call once at startup. */
export async function initializeBackend(type?: BackendType): Promise<ExecutionBackend> {
    const backendType = type ?? (process.env.SMOOTH_BACKEND as BackendType) ?? 'local-microsandbox';

    switch (backendType) {
        case 'local-microsandbox': {
            const { LocalMicrosandboxBackend } = await import('./local-microsandbox.js');
            _backend = new LocalMicrosandboxBackend();
            break;
        }
        case 'aws-lambda':
            throw new Error('AWS Lambda backend not yet implemented. Coming in a future release.');
        default:
            throw new Error(`Unknown execution backend: ${backendType}`);
    }

    await _backend.initialize();
    console.log(`[backend] Initialized execution backend: ${_backend.name}`);

    return _backend;
}

/** Shutdown the active backend. Call on process exit. */
export async function shutdownBackend(): Promise<void> {
    if (_backend) {
        await _backend.shutdown();
        _backend = null;
    }
}
