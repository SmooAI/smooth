/** LangGraph state schema for the main orchestration graph */

import { Annotation } from '@langchain/langgraph';

/** Top-level orchestration state */
export const OrchestratorState = Annotation.Root({
    /** Current project being orchestrated */
    projectId: Annotation<string>,

    /** Beads ready for work (from bd ready) */
    readyBeads: Annotation<string[]>({
        reducer: (_prev, next) => next,
        default: () => [],
    }),

    /** Currently dispatched worker assignments: beadId → workerId */
    activeWorkers: Annotation<Record<string, string>>({
        reducer: (_prev, next) => next,
        default: () => ({}),
    }),

    /** Beads waiting for review */
    pendingReviews: Annotation<string[]>({
        reducer: (_prev, next) => next,
        default: () => [],
    }),

    /** Completed beads in this cycle */
    completedBeads: Annotation<string[]>({
        reducer: (prev, next) => [...prev, ...next],
        default: () => [],
    }),

    /** Messages requiring routing */
    pendingMessages: Annotation<Array<{ beadId: string; content: string; direction: string }>>({
        reducer: (_prev, next) => next,
        default: () => [],
    }),

    /** Current orchestration phase */
    phase: Annotation<'idle' | 'scheduling' | 'dispatching' | 'monitoring' | 'reviewing' | 'aggregating'>({
        reducer: (_prev, next) => next,
        default: () => 'idle' as const,
    }),

    /** Human input (from TUI/web chat) */
    humanInput: Annotation<string | null>({
        reducer: (_prev, next) => next,
        default: () => null,
    }),

    /** Leader's response to human */
    leaderResponse: Annotation<string | null>({
        reducer: (_prev, next) => next,
        default: () => null,
    }),

    /** Error state */
    error: Annotation<string | null>({
        reducer: (_prev, next) => next,
        default: () => null,
    }),
});

export type OrchestratorStateType = typeof OrchestratorState.State;
