import { jsonb, pgTable, text, timestamp, uuid } from 'drizzle-orm/pg-core';

export const memories = pgTable('memories', {
    id: uuid('id').primaryKey().defaultRandom(),
    content: text('content').notNull(),
    metadata: jsonb('metadata').$type<Record<string, unknown>>().default({}),
    createdAt: timestamp('created_at', { withTimezone: true }).defaultNow().notNull(),
    updatedAt: timestamp('updated_at', { withTimezone: true }).defaultNow().notNull(),
});

export type Memory = typeof memories.$inferSelect;
export type NewMemory = typeof memories.$inferInsert;
