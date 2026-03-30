import { jsonb, pgTable, text, timestamp } from 'drizzle-orm/pg-core';

export const config = pgTable('config', {
    key: text('key').primaryKey(),
    value: jsonb('value').$type<unknown>().notNull(),
    updatedAt: timestamp('updated_at', { withTimezone: true }).defaultNow().notNull(),
});

export type Config = typeof config.$inferSelect;
export type NewConfig = typeof config.$inferInsert;
