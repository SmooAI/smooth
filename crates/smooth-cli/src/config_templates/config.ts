/**
 * SmooAI configuration schema.
 *
 * Three tiers:
 *   - publicConfigSchema: safe to ship to browsers (URLs, SDK slugs, anon keys)
 *   - secretConfigSchema: server-only (upstream API keys, DB URLs, signing secrets)
 *   - featureFlagSchema:  ops-toggleable booleans / strings
 *
 * Values live on the SmooAI config server (api.smoo.ai). Push schema
 * changes with `th config push`; set values with `th config set`.
 *
 * Key naming: schema keys are camelCase; the package auto-maps to
 * UPPER_SNAKE_CASE on the wire (so `supabaseUrl` reads as `SUPABASE_URL`
 * for anyone poking at raw values via the CLI).
 */
import { BooleanSchema, defineConfig, StringSchema } from '@smooai/config/config';

export default defineConfig({
    publicConfigSchema: {
        // Add public config keys here, e.g.:
        // apiUrl: StringSchema,
    },
    secretConfigSchema: {
        // Add secret config keys here, e.g.:
        // databaseUrl: StringSchema,
    },
    featureFlagSchema: {
        // Add feature flag keys here, e.g.:
        // enableSomething: BooleanSchema,
    },
});
