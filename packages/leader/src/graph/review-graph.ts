/** Adversarial review subgraph — first-class workflow for reviewing Smooth Operator output
 *
 * Includes dedicated security checks as part of every review cycle.
 */

import { Annotation, END, START, StateGraph } from '@langchain/langgraph';
import { createAuditLogger } from '@smooai/smooth-shared/audit-log';

import { updateBead } from '../beads/client.js';
import { appendProgress, sendMessage } from '../beads/messaging.js';
import { requestOperator } from '../sandbox/pool.js';

const audit = createAuditLogger('leader');

/** Review state */
const ReviewState = Annotation.Root({
    beadId: Annotation<string>,

    reviewBeadId: Annotation<string | null>({
        reducer: (_prev, next) => next,
        default: () => null,
    }),

    reviewOperatorId: Annotation<string | null>({
        reducer: (_prev, next) => next,
        default: () => null,
    }),

    verdict: Annotation<'pending' | 'approved' | 'rework' | 'rejected'>({
        reducer: (_prev, next) => next,
        default: () => 'pending' as const,
    }),

    findings: Annotation<string[]>({
        reducer: (prev, next) => [...prev, ...next],
        default: () => [],
    }),

    /** Security-specific findings from the security review */
    securityFindings: Annotation<SecurityFinding[]>({
        reducer: (prev, next) => [...prev, ...next],
        default: () => [],
    }),

    /** Whether security checks passed */
    securityPassed: Annotation<boolean>({
        reducer: (_prev, next) => next,
        default: () => false,
    }),

    reworkCount: Annotation<number>({
        reducer: (_prev, next) => next,
        default: () => 0,
    }),

    maxReworks: Annotation<number>({
        reducer: (_prev, next) => next,
        default: () => 3,
    }),

    error: Annotation<string | null>({
        reducer: (_prev, next) => next,
        default: () => null,
    }),
});

type ReviewStateType = typeof ReviewState.State;

interface SecurityFinding {
    severity: 'critical' | 'high' | 'medium' | 'low';
    category: SecurityCategory;
    description: string;
    file?: string;
    line?: number;
}

type SecurityCategory =
    | 'injection'
    | 'secrets'
    | 'auth'
    | 'xss'
    | 'path_traversal'
    | 'command_injection'
    | 'insecure_dependency'
    | 'permissions'
    | 'data_exposure'
    | 'crypto'
    | 'other';

/** Node: Run security checks before spawning the review operator */
async function securityCheckNode(state: ReviewStateType): Promise<Partial<ReviewStateType>> {
    audit.phaseStarted('security_review', state.beadId);

    // Spawn a security-focused review operator with read-only permissions
    const operatorId = `security-${state.beadId.slice(-6)}`;

    await requestOperator({
        beadId: state.beadId,
        operatorId,
        workspacePath: '/workspace',
        permissions: ['beads:read', 'beads:message', 'fs:read'],
        systemPrompt: SECURITY_REVIEW_PROMPT,
        phase: 'assess',
    });

    await updateBead(state.beadId, { addLabel: 'review:security' });
    await appendProgress(state.beadId, 'Security review started', 'leader');

    // The security operator will produce structured findings
    // For now, mark as pending — actual results come from operator messages
    return {
        securityPassed: false,
    };
}

/** Node: Collect security results and decide whether to proceed */
async function collectSecurityNode(state: ReviewStateType): Promise<Partial<ReviewStateType>> {
    // Parse security operator output (in production, this reads structured messages)
    // Critical/high findings block approval
    const criticalCount = state.securityFindings.filter((f) => f.severity === 'critical' || f.severity === 'high').length;

    if (criticalCount > 0) {
        await sendMessage(
            state.beadId,
            'leader→human',
            `Security review found ${criticalCount} critical/high issue(s). Review blocked until resolved.`,
            'leader',
        );
        audit.reviewVerdict(state.beadId, 'security_blocked', { criticalCount });
        return { securityPassed: false };
    }

    const mediumCount = state.securityFindings.filter((f) => f.severity === 'medium').length;
    if (mediumCount > 0) {
        await appendProgress(state.beadId, `Security review: ${mediumCount} medium finding(s) — proceeding with warnings`, 'leader');
    }

    audit.phaseCompleted('security_review', state.beadId);
    return { securityPassed: true };
}

/** Route after security check */
function routeSecurityResult(state: ReviewStateType): string {
    return state.securityPassed ? 'spawn_reviewer' : 'handle_rework';
}

/** Node: Spawn a review Smooth Operator */
async function spawnReviewerNode(state: ReviewStateType): Promise<Partial<ReviewStateType>> {
    const operatorId = `reviewer-${state.beadId.slice(-6)}`;
    const reviewBeadId = `review-${state.beadId}`;

    await requestOperator({
        beadId: state.beadId,
        operatorId,
        workspacePath: '/workspace',
        permissions: ['beads:read', 'beads:message', 'fs:read'],
        systemPrompt: REVIEW_SYSTEM_PROMPT,
        phase: 'assess',
    });

    await updateBead(state.beadId, { addLabel: 'review:pending' });
    await appendProgress(state.beadId, 'Review Smooth Operator spawned', 'leader');
    audit.reviewRequested(state.beadId);

    return { reviewBeadId, reviewOperatorId: operatorId };
}

/** Node: Collect review verdict */
async function collectVerdictNode(state: ReviewStateType): Promise<Partial<ReviewStateType>> {
    await sendMessage(
        state.beadId,
        'leader→human',
        `Review in progress for bead ${state.beadId}. Awaiting verdict from Smooth Operator ${state.reviewOperatorId}.`,
        'leader',
    );

    return { verdict: 'pending' };
}

/** Node: Handle approved verdict */
async function handleApprovedNode(state: ReviewStateType): Promise<Partial<ReviewStateType>> {
    await updateBead(state.beadId, { removeLabel: 'review:pending', addLabel: 'review:approved' });
    await sendMessage(state.beadId, 'leader→human', `Review approved for bead ${state.beadId}`, 'leader');
    audit.reviewVerdict(state.beadId, 'approved', { findings: state.findings.length, securityFindings: state.securityFindings.length });
    return {};
}

/** Node: Handle rework verdict */
async function handleReworkNode(state: ReviewStateType): Promise<Partial<ReviewStateType>> {
    const newCount = state.reworkCount + 1;

    if (newCount >= state.maxReworks) {
        await updateBead(state.beadId, { removeLabel: 'review:pending', addLabel: 'review:escalated' });
        await sendMessage(
            state.beadId,
            'leader→human',
            `Review rework limit reached (${state.maxReworks}) for bead ${state.beadId}. Human review required.`,
            'leader',
        );
        audit.reviewVerdict(state.beadId, 'escalated', { reworkCount: newCount });
        return { reworkCount: newCount, verdict: 'rejected' };
    }

    const allFindings = [...state.findings, ...state.securityFindings.map((f) => `[SECURITY/${f.severity}] ${f.category}: ${f.description}`)];

    await updateBead(state.beadId, { removeLabel: 'review:pending', addLabel: 'review:rework' });
    await sendMessage(
        state.beadId,
        'worker→leader',
        `Rework requested (attempt ${newCount}/${state.maxReworks}). Findings: ${allFindings.join('; ')}`,
        'leader',
    );
    audit.reviewVerdict(state.beadId, 'rework', { reworkCount: newCount, findings: allFindings.length });

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
            return 'handle_approved';
        default:
            return END;
    }
}

/** Build the review subgraph */
export function buildReviewGraph() {
    return new StateGraph(ReviewState)
        .addNode('security_check', securityCheckNode)
        .addNode('collect_security', collectSecurityNode)
        .addNode('spawn_reviewer', spawnReviewerNode)
        .addNode('collect_verdict', collectVerdictNode)
        .addNode('handle_approved', handleApprovedNode)
        .addNode('handle_rework', handleReworkNode)

        .addEdge(START, 'security_check')
        .addEdge('security_check', 'collect_security')
        .addConditionalEdges('collect_security', routeSecurityResult)
        .addEdge('spawn_reviewer', 'collect_verdict')
        .addConditionalEdges('collect_verdict', routeVerdict)
        .addEdge('handle_approved', END)
        .addEdge('handle_rework', 'security_check'); // Re-run security on rework too
}

const SECURITY_REVIEW_PROMPT = `You are a security review Smooth Operator.

Your ONLY job is to find security vulnerabilities in the code changes. You are not reviewing for style, correctness, or completeness — only security.

Check for these categories:

**CRITICAL — must block approval:**
- **Secrets exposure**: API keys, passwords, tokens in code or config (not .env)
- **SQL/NoSQL injection**: unsanitized user input in queries
- **Command injection**: user input in exec/spawn/shell commands
- **Path traversal**: user input in file paths without sanitization
- **Auth bypass**: missing or broken authentication/authorization checks
- **Deserialization**: untrusted data in eval(), new Function(), JSON.parse of user input passed to code execution

**HIGH — should block approval:**
- **XSS**: unsanitized output in HTML/templates, dangerouslySetInnerHTML with user data
- **SSRF**: user-controlled URLs in server-side fetch/request
- **Insecure crypto**: weak algorithms (MD5, SHA1 for security), hardcoded IVs/salts
- **Permission escalation**: operations that bypass intended access controls
- **Data exposure**: PII or sensitive data in logs, error messages, or API responses

**MEDIUM — flag but don't block:**
- **Missing input validation**: API endpoints without schema validation at boundaries
- **Overly permissive CORS**: wildcard origins in production config
- **Dependency vulnerabilities**: known CVEs in added/updated packages
- **Insecure defaults**: debug mode, verbose errors, stack traces in production

**LOW — informational:**
- **Missing security headers**: CSP, HSTS, X-Frame-Options
- **Timing attacks**: non-constant-time comparisons on secrets
- **Error information leakage**: detailed error messages in production

Output your findings as structured data:
- SECURITY_FINDING (critical|high|medium|low) [category]: description
  - file: path/to/file.ts:line
  - fix: suggested remediation

If NO security issues found, output:
- SECURITY_PASSED: No security issues found in the changes.

Be thorough. Assume the code handles untrusted input unless proven otherwise.`;

const REVIEW_SYSTEM_PROMPT = `You are an adversarial review Smooth Operator.

Your job is to critically review completed work. A separate security review has already been performed — focus on correctness, completeness, and quality.

1. **Inspect diffs** — Read all file changes from the execution phase
2. **Inspect test results** — Verify tests pass and cover new code paths
3. **Inspect artifacts** — Check completeness of deliverables
4. **Challenge assumptions** — Look for edge cases, missing validation, logic errors
5. **Check requirements** — Verify the original task description is satisfied
6. **Review test quality** — Tests that always pass or mock everything are worth flagging

Output your review as structured findings:
- VERDICT: approved | rework | rejected
- FINDING (high|medium|low): description
- SUGGESTION: improvement description
- MISSING: what's missing

Be thorough but fair. Don't reject for style preferences — focus on correctness and completeness.
Security issues have already been reviewed separately — note if you spot any the security review missed, but it's not your primary focus.`;
