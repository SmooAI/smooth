import { drizzle } from 'drizzle-orm/postgres-js';
import postgres from 'postgres';

import * as schema from './schema/index.js';

const connectionString = process.env.DATABASE_URL ?? 'postgresql://smooth:smooth_dev_password@localhost:5433/smooth';

const queryClient = postgres(connectionString);

export const db = drizzle(queryClient, { schema });

export type Database = typeof db;
