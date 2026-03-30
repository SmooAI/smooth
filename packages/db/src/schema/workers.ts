import { integer, sqliteTable, text } from 'drizzle-orm/sqlite-core';
import { sql } from 'drizzle-orm';

export const workerRuns = sqliteTable('worker_runs', {
    id: text('id').primaryKey().$defaultFn(() => crypto.randomUUID()),
    beadId: text('bead_id').notNull(),
    workerId: text('worker_id').notNull(),
    sandboxId: text('sandbox_id'),
    backendType: text('backend_type').notNull().default('local-microsandbox'),
    phase: text('phase').notNull(),
    status: text('status').notNull(),
    startedAt: integer('started_at', { mode: 'timestamp' }).default(sql`(unixepoch())`).notNull(),
    completedAt: integer('completed_at', { mode: 'timestamp' }),
    metadata: text('metadata', { mode: 'json' }).$type<Record<string, unknown>>().default({}),
});

export type WorkerRun = typeof workerRuns.$inferSelect;
export type NewWorkerRun = typeof workerRuns.$inferInsert;
