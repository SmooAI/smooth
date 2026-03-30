import { drizzleAdapter } from '@better-auth/drizzle-adapter';
import { betterAuth } from 'better-auth';

import { db } from '@smooth/db/client';

export const auth = betterAuth({
    database: drizzleAdapter(db, { provider: 'pg' }),
    emailAndPassword: {
        enabled: true,
    },
    trustedOrigins: ['http://localhost:3100', 'https://smooth.*.ts.net'],
});

export type Auth = typeof auth;
