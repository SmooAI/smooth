/** Bead ↔ Jira field mapping */

import type { BeadStatus } from '@smooth/shared/beads-types';

/** Map Jira status names to bead statuses */
export function jiraStatusToBeadStatus(jiraStatus: string): BeadStatus | null {
    const normalized = jiraStatus.toLowerCase();
    const map: Record<string, BeadStatus> = {
        'to do': 'open',
        'open': 'open',
        'backlog': 'open',
        'in progress': 'in_progress',
        'in review': 'in_progress',
        'blocked': 'blocked',
        'done': 'closed',
        'closed': 'closed',
        'resolved': 'closed',
    };
    return map[normalized] ?? null;
}

/** Map bead status to Jira target status name */
export function beadStatusToJira(beadStatus: BeadStatus): string | null {
    const map: Record<BeadStatus, string | null> = {
        open: 'To Do',
        in_progress: 'In Progress',
        blocked: 'Blocked',
        deferred: null, // No direct Jira mapping
        closed: 'Done',
    };
    return map[beadStatus];
}

/** Map Jira priority names to bead priority (0-4) */
export function jiraPriorityToBeadPriority(jiraPriority: string): number {
    const map: Record<string, number> = {
        highest: 0,
        high: 1,
        medium: 2,
        low: 3,
        lowest: 4,
    };
    return map[jiraPriority.toLowerCase()] ?? 2;
}

/** Map bead priority (0-4) to Jira priority name */
export function beadPriorityToJira(priority: number): string {
    const map: Record<number, string> = {
        0: 'Highest',
        1: 'High',
        2: 'Medium',
        3: 'Low',
        4: 'Lowest',
    };
    return map[priority] ?? 'Medium';
}

/** Map bead type to Jira issue type */
export function beadTypeToJiraIssueType(beadType: string): string {
    const map: Record<string, string> = {
        project: 'Epic',
        epic: 'Epic',
        task: 'Task',
        subtask: 'Sub-task',
        review: 'Task',
        decision: 'Task',
    };
    return map[beadType] ?? 'Task';
}
