/** Adversarial review subgraph — first-class workflow for reviewing Smooth Operator output */

import { Annotation, END, START, StateGraph } from '@langchain/langgraph';

import { updateBead } from '../beads/client.js';
import { appendProgress, sendMessage } from '../beads/messaging.js';
import { requestOperator } from '../sandbox/pool.js';

/** Review state */
const ReviewState = Annotation.Root({
    /** The bead being reviewed */
    beadId: Annotation<string>,

    /** The review bead (linked via validates) */
    reviewBeadId: Annotation<string | null>({
        reducer: (_prev, next) => next,
        default: () => null,
    }),

    /** Review operator ID */
    reviewOperatorId: Annotation<string | null>({
        reducer: (_prev, next) => next,
        default: () => null,
    }),

    /** Review verdict */
    verdict: Annotation<'pending' | 'approved' | 'rework' | 'rejected'>({
        reducer: (_prev, next) => next,
        default: () => 'pending' as const,
    }),

    /** Review findings */
    findings: Annotation<string[]>({
        reducer: (prev, next) => [...prev, ...next],
        default: () => [],
    }),

    /** Number of rework cycles */
    reworkCount: Annotation<number>({
        reducer: (_prev, next) => next,
        default: () => 0,
    }),

    /** Max rework cycles before escalating to human */
    maxReworks: Annotation<number>({
        reducer: (_prev, next) => next,
        default: () => 3,
    }),

    /** Error state */
    error: Annotation<string | null>({
        reducer: (_prev, next) => next,
        default: () => null,
    }),
});

type ReviewStateType = typeof ReviewState.State;

/** Node: Spawn a review Smooth Operator */
async function spawnReviewerNode(state: ReviewStateType): Promise<Partial<ReviewStateType>> {
    const operatorId = `reviewer-${state.beadId.slice(-6)}`;

    // TODO: Create a review bead linked via 'validates' to the original
    const reviewBeadId = `review-${state.beadId}`;

    // Request a review operator with read-only permissions
    await requestOperator({
        beadId: state.beadId,
        operatorId,
        workspacePath: '/workspace', // Same workspace as the original operator
        permissions: ['beads:read', 'beads:message', 'fs:read'],
        systemPrompt: REVIEW_SYSTEM_PROMPT,
        phase: 'assess', // Review starts with assessment
    });

    await updateBead(state.beadId, { addLabel: 'review:pending' });
    await appendProgress(state.beadId, 'Review Smooth Operator spawned', 'leader');

    return {
        reviewBeadId,
        reviewOperatorId: operatorId,
    };
}

/** Node: Collect review verdict */
async function collectVerdictNode(state: ReviewStateType): Promise<Partial<ReviewStateType>> {
    // In a real implementation, we'd wait for the review operator to complete
    // and parse its structured output. For now, the verdict comes from messages.
    await sendMessage(
        state.beadId,
        'leader→human',
        `Review in progress for bead ${state.beadId}. Awaiting verdict from Smooth Operator ${state.reviewOperatorId}.`,
        'leader',
    );

    // Placeholder — actual verdict comes from the review operator's structured output
    return {
        verdict: 'pending',
    };
}

/** Node: Handle approved verdict */
async function handleApprovedNode(state: ReviewStateType): Promise<Partial<ReviewStateType>> {
    await updateBead(state.beadId, { removeLabel: 'review:pending', addLabel: 'review:approved' });
    await sendMessage(state.beadId, 'leader→human', `Review approved for bead ${state.beadId}`, 'leader');
    return {};
}

/** Node: Handle rework verdict */
async function handleReworkNode(state: ReviewStateType): Promise<Partial<ReviewStateType>> {
    const newCount = state.reworkCount + 1;

    if (newCount >= state.maxReworks) {
        // Escalate to human
        await updateBead(state.beadId, { removeLabel: 'review:pending', addLabel: 'review:escalated' });
        await sendMessage(
            state.beadId,
            'leader→human',
            `Review rework limit reached (${state.maxReworks}) for bead ${state.beadId}. Human review required.`,
            'leader',
        );
        return { reworkCount: newCount, verdict: 'rejected' };
    }

    await updateBead(state.beadId, { removeLabel: 'review:pending', addLabel: 'review:rework' });
    await sendMessage(
        state.beadId,
        'worker→leader',
        `Rework requested (attempt ${newCount}/${state.maxReworks}). Findings: ${state.findings.join('; ')}`,
        'leader',
    );

    return { reworkCount: newCount };
}

/** Route based on verdict */
function routeVerdict(state: ReviewStateType): string {
    switch (state.verdict) {
        case 'approved':
            return 'handle_approved';
        case 'rework':
            return state.reworkCount >= state.maxReworks ? 'handle_approved' : 'handle_rework';
        case 'rejected':
            return 'handle_approved'; // Goes to END after
        default:
            return END; // pending — waiting for human or operator
    }
}

/** Build the review subgraph */
export function buildReviewGraph() {
    return new StateGraph(ReviewState)
        .addNode('spawn_reviewer', spawnReviewerNode)
        .addNode('collect_verdict', collectVerdictNode)
        .addNode('handle_approved', handleApprovedNode)
        .addNode('handle_rework', handleReworkNode)

        .addEdge(START, 'spawn_reviewer')
        .addEdge('spawn_reviewer', 'collect_verdict')
        .addConditionalEdges('collect_verdict', routeVerdict)
        .addEdge('handle_approved', END)
        .addEdge('handle_rework', 'spawn_reviewer'); // Loop back for another review
}

const REVIEW_SYSTEM_PROMPT = `You are an adversarial review Smooth Operator.

Your job is to critically review completed work:

1. **Inspect diffs** — Read all file changes from the execution phase
2. **Inspect test results** — Verify tests pass and cover new code
3. **Inspect artifacts** — Check completeness of deliverables
4. **Challenge assumptions** — Look for edge cases, missing validation, security issues
5. **Check requirements** — Verify the original task description is satisfied

Output your review as structured findings:
- VERDICT: approved | rework | rejected
- FINDING (high|medium|low): description
- SUGGESTION: improvement description
- MISSING: what's missing

Be thorough but fair. Don't reject for style preferences — focus on correctness, completeness, and security.`;
