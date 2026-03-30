import { defineConfig } from 'drizzle-kit';

export default defineConfig({
    dialect: 'postgresql',
    schema: './src/schema/index.ts',
    out: './src/migrations',
    dbCredentials: {
        url: process.env.DATABASE_URL ?? 'postgresql://smooth:smooth_dev_password@localhost:5433/smooth',
    },
});
