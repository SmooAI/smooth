import { homedir } from 'node:os';
import { join } from 'node:path';

import { defineConfig } from 'drizzle-kit';

export default defineConfig({
    dialect: 'sqlite',
    schema: './src/schema/index.ts',
    out: './src/migrations',
    dbCredentials: {
        url: process.env.SMOOTH_DB_PATH ?? join(homedir(), '.smooth', 'smooth.db'),
    },
});
