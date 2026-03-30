/** Smooth Operator lifecycle state machine: assess → plan → orchestrate → execute → finalize
 *
 * Uses the ExecutionBackend interface — no Docker, no driver, no container details.
 */

import type { WorkerPhase } from '@smooth/shared/worker-types';

import { getBackend } from '../backend/registry.js';
import { updateBead } from '../beads/client.js';
import { appendProgress, sendMessage } from '../beads/messaging.js';

/** Phase transition rules */
const PHASE_TRANSITIONS: Record<WorkerPhase, WorkerPhase | 'done'> = {
    assess: 'plan',
    plan: 'orchestrate',
    orchestrate: 'execute',
    execute: 'finalize',
    finalize: 'done',
};

/** System prompts for each phase */
const PHASE_PROMPTS: Record<WorkerPhase, string> = {
    assess: `You are a Smooth Operator in the ASSESS phase.

Your job is to inspect the current task context:
1. Read the task bead description and requirements
2. Inspect any graph neighbors and related work (use beads_context tool)
3. Check previous messages, progress updates, and artifacts
4. Inspect the workspace/repo state
5. Write a context summary as a progress update

When done, report your assessment findings.`,

    plan: `You are a Smooth Operator in the PLAN phase.

Based on your assessment, define the next steps:
1. Define concrete, bounded steps (max 5-7)
2. Identify required tools for each step
3. Define expected outputs (files, tests, artifacts)
4. Determine if sub-workers are needed
5. Write your plan as a progress update

When done, report your plan.`,

    orchestrate: `You are a Smooth Operator in the ORCHESTRATE phase.

Coordinate the work:
1. If work should be split, create child beads (use spawn_subtask tool)
2. If sub-workers are needed, request them from the leader
3. Ensure dependency links are correct
4. Preserve graph integrity

When done, confirm orchestration is complete.`,

    execute: `You are a Smooth Operator in the EXECUTE phase.

Perform the actual work:
1. Follow your plan step by step
2. Write code, run tests, create artifacts
3. Report progress regularly (use progress_append tool)
4. Keep work observable — no silent steps

When done, report completion with a summary of what was done.`,

    finalize: `You are a Smooth Operator in the FINALIZE phase.

Wrap up the work:
1. Summarize what was completed (use artifact_write tool)
2. Update bead graph relationships
3. Link all artifacts
4. Identify newly unlocked work
5. Recommend next actions back to the leader
6. Request a review (use review_request tool)

When done, mark the task as finalized.`,
};

/** Run a single phase of the Smooth Operator lifecycle */
export async function runPhase(
    sandboxId: string,
    beadId: string,
    phase: WorkerPhase,
): Promise<{ completed: boolean; nextPhase: WorkerPhase | 'done'; output: string }> {
    const backend = getBackend();

    // Update bead with current phase
    await updateBead(beadId, { addLabel: `phase:${phase}` });
    await appendProgress(beadId, `Starting ${phase} phase`, sandboxId);

    // Create session and run phase prompt via backend
    const session = await backend.createSession(sandboxId, `${beadId}-${phase}`);
    const prompt = PHASE_PROMPTS[phase];
    const result = await backend.prompt(sandboxId, session.id, prompt);

    // Extract output from assistant messages
    const output = result.messages
        .filter((m) => m.role === 'assistant')
        .map((m) => m.content)
        .join('\n');

    // Record progress
    await appendProgress(beadId, `Completed ${phase} phase`, sandboxId);
    await sendMessage(beadId, 'worker→leader', `Phase ${phase} complete: ${output.slice(0, 200)}`, sandboxId);

    const nextPhase = PHASE_TRANSITIONS[phase];

    return { completed: true, nextPhase, output };
}

/** Run the full lifecycle for a Smooth Operator */
export async function runFullLifecycle(
    sandboxId: string,
    beadId: string,
): Promise<{ success: boolean; phasesCompleted: WorkerPhase[]; error?: string }> {
    const phases: WorkerPhase[] = ['assess', 'plan', 'orchestrate', 'execute', 'finalize'];
    const completed: WorkerPhase[] = [];

    for (const phase of phases) {
        try {
            const result = await runPhase(sandboxId, beadId, phase);

            if (!result.completed) {
                return { success: false, phasesCompleted: completed, error: `Phase ${phase} failed: ${result.output}` };
            }

            completed.push(phase);
            if (result.nextPhase === 'done') break;
        } catch (error) {
            return { success: false, phasesCompleted: completed, error: `Phase ${phase} threw: ${error instanceof Error ? error.message : String(error)}` };
        }
    }

    return { success: true, phasesCompleted: completed };
}
