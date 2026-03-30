/** th auth — provider authentication for Smooth
 *
 * Supports the same providers as OpenCode: Anthropic, OpenAI, OpenRouter,
 * Groq, OpenCode Zen, and any OpenAI-compatible endpoint.
 * Credentials stored at ~/.smooth/providers.json, shared between leader + operators.
 */

import type { Command } from 'commander';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

import { getActiveServerUrl, getApiKey, loadConfig } from '../config.js';

const SMOOTH_DIR = join(homedir(), '.smooth');
const PROVIDERS_PATH = join(SMOOTH_DIR, 'providers.json');

export interface ProviderConfig {
    name: string;
    apiKey: string;
    baseUrl?: string;
    model?: string;
    enabled: boolean;
}

interface ProvidersFile {
    default: string;
    providers: Record<string, ProviderConfig>;
}

const KNOWN_PROVIDERS: Record<string, { name: string; envVar: string; baseUrl?: string; defaultModel: string }> = {
    'opencode-zen': { name: 'OpenCode Zen', envVar: 'OPENCODE_API_KEY', defaultModel: 'opencode/zen' },
    anthropic: { name: 'Anthropic', envVar: 'ANTHROPIC_API_KEY', defaultModel: 'claude-sonnet-4-20250514' },
    openai: { name: 'OpenAI', envVar: 'OPENAI_API_KEY', defaultModel: 'gpt-4o' },
    openrouter: { name: 'OpenRouter', envVar: 'OPENROUTER_API_KEY', baseUrl: 'https://openrouter.ai/api/v1', defaultModel: 'anthropic/claude-sonnet-4' },
    groq: { name: 'Groq', envVar: 'GROQ_API_KEY', baseUrl: 'https://api.groq.com/openai/v1', defaultModel: 'llama-3.3-70b-versatile' },
    google: { name: 'Google AI', envVar: 'GOOGLE_API_KEY', defaultModel: 'gemini-2.0-flash' },
    custom: { name: 'Custom (OpenAI-compatible)', envVar: '', defaultModel: '' },
};

function ensureDir(): void {
    if (!existsSync(SMOOTH_DIR)) mkdirSync(SMOOTH_DIR, { recursive: true });
}

export function loadProviders(): ProvidersFile {
    if (!existsSync(PROVIDERS_PATH)) {
        return { default: '', providers: {} };
    }
    return JSON.parse(readFileSync(PROVIDERS_PATH, 'utf8'));
}

function saveProviders(data: ProvidersFile): void {
    ensureDir();
    writeFileSync(PROVIDERS_PATH, JSON.stringify(data, null, 4) + '\n', { mode: 0o600 });
}

export function registerAuthCommand(program: Command) {
    const auth = program.command('auth').description('Provider authentication (shared by leader + Smooth Operators)');

    auth.command('login')
        .description('Add or update a provider')
        .argument('[provider]', 'Provider: opencode-zen, anthropic, openai, openrouter, groq, google, custom')
        .option('--api-key <key>', 'API key')
        .option('--base-url <url>', 'Base URL (for custom/openrouter)')
        .option('--model <model>', 'Default model')
        .option('--default', 'Set as default provider')
        .action((providerArg, opts) => {
            const providerId = providerArg ?? 'opencode-zen';
            const known = KNOWN_PROVIDERS[providerId];

            if (!known) {
                console.error(`Unknown provider: ${providerId}`);
                console.error(`Available: ${Object.keys(KNOWN_PROVIDERS).join(', ')}`);
                process.exit(1);
            }

            let apiKey = opts.apiKey;
            if (!apiKey && known.envVar) {
                apiKey = process.env[known.envVar];
                if (apiKey) console.log(`Using ${known.envVar} from environment.`);
            }
            if (!apiKey) {
                console.error(`API key required for ${known.name}.`);
                console.error(`  --api-key <key>`);
                if (known.envVar) console.error(`  or set ${known.envVar} environment variable`);
                process.exit(1);
            }

            const data = loadProviders();
            data.providers[providerId] = {
                name: known.name,
                apiKey,
                baseUrl: opts.baseUrl ?? known.baseUrl,
                model: opts.model ?? known.defaultModel,
                enabled: true,
            };

            if (opts.default || !data.default) data.default = providerId;
            saveProviders(data);

            console.log(`${known.name}: authenticated`);
            console.log(`  Model: ${opts.model ?? known.defaultModel}`);
            if (data.default === providerId) console.log('  (default provider)');
            console.log('');
            console.log('This auth is shared between the leader and all Smooth Operators.');
        });

    auth.command('providers')
        .description('List configured providers')
        .action(() => {
            const data = loadProviders();
            const providers = Object.entries(data.providers);

            if (providers.length === 0) {
                console.log('No providers configured. Run: th auth login <provider>');
                console.log('');
                console.log('Available providers:');
                for (const [id, info] of Object.entries(KNOWN_PROVIDERS)) {
                    console.log(`  ${id.padEnd(16)} ${info.name}`);
                }
                return;
            }

            console.log('Configured Providers');
            console.log('====================');
            for (const [id, provider] of providers) {
                const isDefault = data.default === id ? ' (default)' : '';
                const keyPreview = provider.apiKey.slice(0, 8) + '...' + provider.apiKey.slice(-4);
                console.log(`  ${id.padEnd(16)} ${provider.name}${isDefault}`);
                console.log(`${''.padEnd(18)} key: ${keyPreview}  model: ${provider.model ?? 'default'}`);
            }
        });

    auth.command('default [provider]')
        .description('Get or set the default provider')
        .action((providerId) => {
            const data = loadProviders();
            if (!providerId) {
                console.log(data.default || 'No default provider set.');
                return;
            }
            if (!data.providers[providerId]) {
                console.error(`Provider ${providerId} not configured. Run: th auth login ${providerId}`);
                process.exit(1);
            }
            data.default = providerId;
            saveProviders(data);
            console.log(`Default provider: ${providerId}`);
        });

    auth.command('remove <provider>')
        .description('Remove a provider')
        .action((providerId) => {
            const data = loadProviders();
            if (!data.providers[providerId]) {
                console.error(`Provider ${providerId} not found.`);
                process.exit(1);
            }
            delete data.providers[providerId];
            if (data.default === providerId) data.default = Object.keys(data.providers)[0] ?? '';
            saveProviders(data);
            console.log(`Removed: ${providerId}`);
        });

    auth.command('status')
        .description('Show authentication status for all services')
        .action(() => {
            console.log('Authentication Status');
            console.log('====================');
            console.log('');

            const data = loadProviders();
            const providerCount = Object.keys(data.providers).length;
            if (providerCount > 0) {
                console.log(`LLM Providers: ${providerCount} configured (default: ${data.default || 'none'})`);
                for (const [id, p] of Object.entries(data.providers)) {
                    console.log(`  ${id}: ${p.name}${data.default === id ? ' *' : ''}`);
                }
            } else {
                console.log('LLM Providers: none — run: th auth login <provider>');
            }
            console.log('');

            const serverUrl = getActiveServerUrl();
            const apiKey = getApiKey(serverUrl);
            console.log(`Leader:        ${apiKey ? `authenticated (${serverUrl})` : 'not authenticated — run: th login'}`);

            const config = loadConfig();
            console.log(`SmooAI:        ${config.smoo?.client_id ? `configured (org: ${config.smoo.org_id ?? 'unknown'})` : 'not configured'}`);

            const smooCredPath = join(homedir(), '.smooai', 'credentials.json');
            console.log(`Smoo Config:   ${existsSync(smooCredPath) ? 'authenticated' : 'not authenticated — run: th smoo config login'}`);
            console.log('');
        });
}

/** Load the default provider config (used by leader and operators) */
export function getDefaultProvider(): ProviderConfig | null {
    const data = loadProviders();
    if (!data.default) return null;
    return data.providers[data.default] ?? null;
}

/** Get all enabled providers (for OpenCode config generation) */
export function getEnabledProviders(): Record<string, ProviderConfig> {
    const data = loadProviders();
    const enabled: Record<string, ProviderConfig> = {};
    for (const [id, p] of Object.entries(data.providers)) {
        if (p.enabled) enabled[id] = p;
    }
    return enabled;
}
