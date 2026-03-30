import { jsonb, pgTable, text, timestamp, uuid } from 'drizzle-orm/pg-core';

export const workerRuns = pgTable('worker_runs', {
    id: uuid('id').primaryKey().defaultRandom(),
    beadId: text('bead_id').notNull(),
    workerId: text('worker_id').notNull(),
    sandboxId: text('sandbox_id'),
    backendType: text('backend_type').notNull().default('local-microsandbox'),
    phase: text('phase').notNull(), // assess, plan, orchestrate, execute, finalize, review
    status: text('status').notNull(), // pending, running, completed, failed, timeout
    startedAt: timestamp('started_at', { withTimezone: true }).defaultNow().notNull(),
    completedAt: timestamp('completed_at', { withTimezone: true }),
    metadata: jsonb('metadata').$type<Record<string, unknown>>().default({}),
});

export type WorkerRun = typeof workerRuns.$inferSelect;
export type NewWorkerRun = typeof workerRuns.$inferInsert;
