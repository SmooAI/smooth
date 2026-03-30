/** Bead graph relationship types for Smooth */

export type BeadType = 'project' | 'epic' | 'task' | 'subtask' | 'review' | 'decision';

export type BeadStatus = 'open' | 'in_progress' | 'blocked' | 'deferred' | 'closed';

export type DependencyType = 'blocks' | 'parent-child' | 'validates' | 'relates-to' | 'discovered-from' | 'supersedes' | 'tracks' | 'caused-by';

export interface Bead {
    id: string;
    title: string;
    description: string;
    status: BeadStatus;
    type: BeadType;
    priority: number; // 0-4 (0 = highest)
    labels: string[];
    owner?: string;
    createdAt: string;
    updatedAt: string;
    closedAt?: string;
    closeReason?: string;
    dependencyCount: number;
    dependentCount: number;
    commentCount: number;
    externalRef?: string; // e.g., "jira:SMOODEV-123"
}

export interface BeadDetail extends Bead {
    notes?: string;
    dependencies: Dependency[];
    comments: BeadComment[];
}

export interface Dependency {
    issueId: string;
    dependsOnId: string;
    type: DependencyType;
    createdAt: string;
}

export interface BeadComment {
    id: string;
    beadId: string;
    content: string;
    author: string;
    createdAt: string;
}

/** Standard label prefixes for dimensional state tracking */
export const LABEL_PREFIXES = {
    worker: 'worker:', // worker:<worker-id>
    phase: 'phase:', // phase:assess|plan|orchestrate|execute|finalize
    review: 'review:', // review:pending|approved|rejected|rework
    run: 'run:', // run:<run-id>
    scope: 'scope:', // scope:code|test|docs|infra|research|analysis|deploy
} as const;
