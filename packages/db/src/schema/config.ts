import { integer, sqliteTable, text } from 'drizzle-orm/sqlite-core';
import { sql } from 'drizzle-orm';

export const config = sqliteTable('config', {
    key: text('key').primaryKey(),
    value: text('value', { mode: 'json' }).$type<unknown>().notNull(),
    updatedAt: integer('updated_at', { mode: 'timestamp' }).default(sql`(unixepoch())`).notNull(),
});

export type Config = typeof config.$inferSelect;
export type NewConfig = typeof config.$inferInsert;
