/** Bidirectional Jira ↔ Beads sync engine */

import type { BeadStatus } from '@smooai/smooth-shared/beads-types';

import { closeBead, createBead, listBeads, updateBead } from '../beads/client.js';
import { JiraClient } from './client.js';
import { beadStatusToJira, beadTypeToJiraIssueType, jiraPriorityToBeadPriority, jiraStatusToBeadStatus } from './mapper.js';

interface SyncResult {
    pulled: number;
    pushed: number;
    conflicts: number;
}

export class JiraSyncEngine {
    private jira: JiraClient;
    private project: string;

    constructor(jira: JiraClient, project: string) {
        this.jira = jira;
        this.project = project;
    }

    /** Full bidirectional sync */
    async sync(): Promise<SyncResult> {
        const pullResult = await this.pull();
        const pushResult = await this.push();

        return {
            pulled: pullResult.pulled,
            pushed: pushResult.pushed,
            conflicts: pullResult.conflicts + pushResult.conflicts,
        };
    }

    /** Pull: Jira → Beads (import new issues, update status) */
    async pull(): Promise<SyncResult> {
        let pulled = 0;
        let conflicts = 0;

        const jiraIssues = await this.jira.searchIssues(`project=${this.project} AND status!=Done ORDER BY updated DESC`, 100);
        const beads = await listBeads();
        const beadsByRef = new Map(beads.filter((b) => b.externalRef?.startsWith('jira:')).map((b) => [b.externalRef!, b]));

        for (const issue of jiraIssues) {
            const ref = `jira:${issue.key}`;
            const existingBead = beadsByRef.get(ref);

            if (!existingBead) {
                // New Jira issue — create bead
                await createBead({
                    title: `${issue.key}: ${issue.fields.summary}`,
                    description: `Imported from Jira: ${issue.key}`,
                    type: 'task',
                    priority: jiraPriorityToBeadPriority(issue.fields.priority.name),
                    labels: [`jira:${issue.key}`, ...issue.fields.labels],
                });
                pulled++;
            } else {
                // Existing bead — sync status
                const jiraBeadStatus = jiraStatusToBeadStatus(issue.fields.status.name);
                if (jiraBeadStatus && jiraBeadStatus !== existingBead.status) {
                    await updateBead(existingBead.id, { status: jiraBeadStatus });
                    pulled++;
                }
            }
        }

        return { pulled, pushed: 0, conflicts };
    }

    /** Push: Beads → Jira (export new beads, update status) */
    async push(): Promise<SyncResult> {
        let pushed = 0;
        let conflicts = 0;

        const beads = await listBeads();

        for (const bead of beads) {
            // Only push beads that have a Jira reference
            if (!bead.externalRef?.startsWith('jira:')) continue;

            const jiraKey = bead.externalRef.replace('jira:', '');
            const targetStatus = beadStatusToJira(bead.status);

            if (targetStatus) {
                try {
                    const transitions = await this.jira.getTransitions(jiraKey);
                    const transition = transitions.find((t) => t.to.name.toLowerCase() === targetStatus.toLowerCase());

                    if (transition) {
                        await this.jira.transitionIssue(jiraKey, transition.id);
                        pushed++;
                    }
                } catch {
                    conflicts++;
                }
            }
        }

        return { pulled: 0, pushed, conflicts };
    }

    /** Create a Jira issue from a bead */
    async createFromBead(beadId: string, title: string, description: string, type: string = 'task'): Promise<string> {
        const issue = await this.jira.createIssue({
            summary: title,
            description,
            issuetype: beadTypeToJiraIssueType(type),
        });

        // Link bead to Jira issue
        await updateBead(beadId, { addLabel: `jira:${issue.key}` });

        return issue.key;
    }
}
