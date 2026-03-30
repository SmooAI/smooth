/** In-memory event stream using EventEmitter */

import { EventEmitter } from 'node:events';

import type { EventListener, EventStream, ExecutionEvent } from './types.js';

export class LocalEventStream implements EventStream {
    private emitter = new EventEmitter();

    emit(event: ExecutionEvent): void {
        this.emitter.emit(event.type, event);
        this.emitter.emit('*', event);
    }

    on(type: ExecutionEvent['type'] | '*', listener: EventListener): () => void {
        this.emitter.on(type, listener);
        return () => {
            this.emitter.off(type, listener);
        };
    }
}
