import { z } from 'zod';

/** Zod schemas for API validation and structured output */

// Bead filters for API queries
export const BeadFiltersSchema = z.object({
    status: z.enum(['open', 'in_progress', 'blocked', 'closed', 'deferred']).optional(),
    type: z.enum(['project', 'epic', 'task', 'subtask', 'review', 'decision']).optional(),
    phase: z.enum(['assess', 'plan', 'orchestrate', 'execute', 'finalize']).optional(),
    worker: z.string().optional(),
    label: z.string().optional(),
    priority: z.number().min(0).max(4).optional(),
});
export type BeadFilters = z.infer<typeof BeadFiltersSchema>;

// Message creation
export const SendMessageSchema = z.object({
    beadId: z.string().min(1),
    content: z.string().min(1),
    direction: z.enum(['human→leader', 'leader→worker', 'worker→leader', 'worker→worker', 'leader→human']),
});
export type SendMessage = z.infer<typeof SendMessageSchema>;

// Chat message to leader
export const ChatMessageSchema = z.object({
    content: z.string().min(1),
    attachments: z
        .array(
            z.object({
                type: z.enum(['file', 'bead', 'worker', 'artifact']),
                reference: z.string(),
            }),
        )
        .optional(),
});
export type ChatMessage = z.infer<typeof ChatMessageSchema>;

// Worker plan (structured output from LLM)
export const WorkerPlanSchema = z.object({
    steps: z.array(
        z.object({
            description: z.string(),
            tools: z.array(z.string()),
            expectedOutput: z.string(),
        }),
    ),
    needsSubWorkers: z.boolean(),
    estimatedComplexity: z.enum(['low', 'medium', 'high']),
});
export type WorkerPlan = z.infer<typeof WorkerPlanSchema>;

// Review verdict (structured output from review worker)
export const ReviewVerdictSchema = z.object({
    verdict: z.enum(['approved', 'rework', 'rejected']),
    findings: z.array(
        z.object({
            severity: z.enum(['high', 'medium', 'low']),
            description: z.string(),
        }),
    ),
    suggestions: z.array(z.string()),
    missing: z.array(z.string()),
});
export type ReviewVerdict = z.infer<typeof ReviewVerdictSchema>;

// Project creation
export const CreateProjectSchema = z.object({
    name: z.string().min(1).max(100),
    description: z.string().min(1),
});
export type CreateProject = z.infer<typeof CreateProjectSchema>;

// Config update
export const SetConfigSchema = z.object({
    key: z.string().min(1),
    value: z.string().min(1),
});
export type SetConfig = z.infer<typeof SetConfigSchema>;
