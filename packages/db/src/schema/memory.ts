import { integer, sqliteTable, text } from 'drizzle-orm/sqlite-core';
import { sql } from 'drizzle-orm';

export const memories = sqliteTable('memories', {
    id: text('id').primaryKey().$defaultFn(() => crypto.randomUUID()),
    content: text('content').notNull(),
    metadata: text('metadata', { mode: 'json' }).$type<Record<string, unknown>>().default({}),
    createdAt: integer('created_at', { mode: 'timestamp' }).default(sql`(unixepoch())`).notNull(),
    updatedAt: integer('updated_at', { mode: 'timestamp' }).default(sql`(unixepoch())`).notNull(),
});

export type Memory = typeof memories.$inferSelect;
export type NewMemory = typeof memories.$inferInsert;
