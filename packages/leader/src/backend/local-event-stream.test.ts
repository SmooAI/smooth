import { describe, expect, it, vi } from 'vitest';

import { LocalEventStream } from './local-event-stream.js';
import type { ExecutionEvent } from './types.js';

function makeEvent(type: ExecutionEvent['type'] = 'sandbox_created'): ExecutionEvent {
    return {
        type,
        sandboxId: 'test-sandbox',
        operatorId: 'test-operator',
        data: {},
        timestamp: new Date(),
    };
}

describe('LocalEventStream', () => {
    it('emits events to type-specific listeners', () => {
        const stream = new LocalEventStream();
        const listener = vi.fn();

        stream.on('sandbox_created', listener);
        stream.emit(makeEvent('sandbox_created'));

        expect(listener).toHaveBeenCalledTimes(1);
    });

    it('emits events to wildcard listeners', () => {
        const stream = new LocalEventStream();
        const listener = vi.fn();

        stream.on('*', listener);
        stream.emit(makeEvent('sandbox_created'));
        stream.emit(makeEvent('sandbox_destroyed'));

        expect(listener).toHaveBeenCalledTimes(2);
    });

    it('does not emit to wrong type', () => {
        const stream = new LocalEventStream();
        const listener = vi.fn();

        stream.on('sandbox_destroyed', listener);
        stream.emit(makeEvent('sandbox_created'));

        expect(listener).not.toHaveBeenCalled();
    });

    it('returns unsubscribe function', () => {
        const stream = new LocalEventStream();
        const listener = vi.fn();

        const unsub = stream.on('sandbox_created', listener);
        stream.emit(makeEvent('sandbox_created'));
        expect(listener).toHaveBeenCalledTimes(1);

        unsub();
        stream.emit(makeEvent('sandbox_created'));
        expect(listener).toHaveBeenCalledTimes(1);
    });
});
