/** th smoo — SmooAI platform API + config schema management
 *
 * `th smoo config <cmd>` calls @smooai/config CLI directly as a dependency.
 * No global install needed — it's bundled with th.
 */

import type { Command } from 'commander';
import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';

import { loadConfig } from '../config.js';

/** Resolve the smooai-config binary from node_modules */
function getSmooConfigBin(): string {
    // Try to resolve from the package's bin entry
    try {
        const pkgPath = require.resolve('@smooai/config/package.json');
        const pkg = require(pkgPath);
        const binPath = resolve(pkgPath, '..', pkg.bin['smooai-config']);
        return binPath;
    } catch {
        // Fallback: look in node_modules/.bin
        return 'smooai-config';
    }
}

function runSmooConfig(args: string[]): void {
    const bin = getSmooConfigBin();
    try {
        execFileSync('node', [bin, ...args], { stdio: 'inherit' });
    } catch {
        process.exit(1);
    }
}

export function registerSmooCommand(program: Command) {
    const smoo = program.command('smoo').description('SmooAI platform — API, config schemas, and values');

    // ── th smoo config ─── @smooai/config CLI (bundled) ──────

    const cfg = smoo.command('config').description('Config schema and value management (@smooai/config)');

    cfg.command('init')
        .description('Initialize .smooai-config/ directory with templates')
        .option('--language <lang>', 'Project language (typescript, python, go, rust)')
        .action((opts) => {
            runSmooConfig(['init', ...(opts.language ? ['--language', opts.language] : [])]);
        });

    cfg.command('login')
        .description('Store Smoo AI API credentials')
        .option('--api-key <key>', 'API key')
        .option('--org-id <id>', 'Organization ID')
        .option('--base-url <url>', 'API base URL')
        .action((opts) => {
            const args = ['login'];
            if (opts.apiKey) args.push('--api-key', opts.apiKey);
            if (opts.orgId) args.push('--org-id', opts.orgId);
            if (opts.baseUrl) args.push('--base-url', opts.baseUrl);
            runSmooConfig(args);
        });

    cfg.command('push')
        .description('Push local config schema to Smoo AI platform')
        .option('--schema-name <name>', 'Schema name')
        .option('--description <desc>', 'Change description')
        .option('-y, --yes', 'Skip confirmation')
        .action((opts) => {
            const args = ['push'];
            if (opts.schemaName) args.push('--schema-name', opts.schemaName);
            if (opts.description) args.push('--description', opts.description);
            if (opts.yes) args.push('--yes');
            runSmooConfig(args);
        });

    cfg.command('pull')
        .description('Pull config values from Smoo AI platform')
        .option('-e, --environment <env>', 'Environment name', 'development')
        .action((opts) => {
            runSmooConfig(['pull', '--environment', opts.environment]);
        });

    cfg.command('set <key> <value>')
        .description('Set a config value on Smoo AI platform')
        .option('-e, --environment <env>', 'Environment name', 'development')
        .option('--tier <tier>', 'Config tier (public, secret, feature_flag)')
        .option('--schema-name <name>', 'Schema name')
        .action((key, value, opts) => {
            const args = ['set', key, value, '--environment', opts.environment];
            if (opts.tier) args.push('--tier', opts.tier);
            if (opts.schemaName) args.push('--schema-name', opts.schemaName);
            runSmooConfig(args);
        });

    cfg.command('get <key>')
        .description('Get a config value from Smoo AI platform')
        .option('-e, --environment <env>', 'Environment name', 'development')
        .action((key, opts) => {
            runSmooConfig(['get', key, '--environment', opts.environment]);
        });

    cfg.command('list')
        .description('List all config values for an environment')
        .option('-e, --environment <env>', 'Environment name', 'development')
        .action((opts) => {
            runSmooConfig(['list', '--environment', opts.environment]);
        });

    cfg.command('diff')
        .description('Compare local schema vs remote schema')
        .option('--schema-name <name>', 'Schema name')
        .action((opts) => {
            runSmooConfig(['diff', ...(opts.schemaName ? ['--schema-name', opts.schemaName] : [])]);
        });

    // ── th smoo agents/jobs ─── M2M API commands ─────────────

    smoo.command('agents')
        .description('List SmooAI agents')
        .action(async () => {
            const config = loadConfig();
            if (!config.smoo?.client_id) {
                console.error('SmooAI not configured. Run: th config set smoo.client_id <id>');
                process.exit(1);
            }
            console.log('SmooAI agent listing — requires M2M auth (coming soon)');
        });

    smoo.command('jobs')
        .description('List SmooAI jobs')
        .action(async () => {
            console.log('SmooAI jobs — requires M2M auth (coming soon)');
        });
}
