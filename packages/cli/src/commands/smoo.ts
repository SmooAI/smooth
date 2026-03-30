/** th smoo — SmooAI platform API + config schema management
 *
 * `th smoo config <cmd>` wraps the @smooai/config CLI (smooai-config binary)
 * for schema push/pull/set/get/list/diff operations.
 */

import type { Command } from 'commander';
import { execSync } from 'node:child_process';

import { loadConfig } from '../config.js';

function runSmooConfig(args: string): void {
    try {
        execSync(`smooai-config ${args}`, { stdio: 'inherit' });
    } catch (error) {
        if ((error as NodeJS.ErrnoException).code === 'ENOENT') {
            console.error('smooai-config not found.');
            console.error('Install: pnpm add -g @smooai/config');
            process.exit(1);
        }
        process.exit(1);
    }
}

export function registerSmooCommand(program: Command) {
    const smoo = program.command('smoo').description('SmooAI platform — API, config schemas, and values');

    // ── th smoo config ─── wraps @smooai/config CLI ──────────

    const cfg = smoo.command('config').description('Config schema and value management (via @smooai/config)');

    cfg.command('init')
        .description('Initialize .smooai-config/ directory with templates')
        .option('--language <lang>', 'Project language (typescript, python, go, rust)')
        .action((opts) => {
            runSmooConfig(`init${opts.language ? ` --language ${opts.language}` : ''}`);
        });

    cfg.command('login')
        .description('Store Smoo AI API credentials')
        .option('--api-key <key>', 'API key')
        .option('--org-id <id>', 'Organization ID')
        .option('--base-url <url>', 'API base URL')
        .action((opts) => {
            const flags = [opts.apiKey ? `--api-key ${opts.apiKey}` : '', opts.orgId ? `--org-id ${opts.orgId}` : '', opts.baseUrl ? `--base-url ${opts.baseUrl}` : '']
                .filter(Boolean)
                .join(' ');
            runSmooConfig(`login ${flags}`);
        });

    cfg.command('push')
        .description('Push local config schema to Smoo AI platform')
        .option('--schema-name <name>', 'Schema name')
        .option('--description <desc>', 'Change description')
        .option('-y, --yes', 'Skip confirmation')
        .action((opts) => {
            const flags = [opts.schemaName ? `--schema-name ${opts.schemaName}` : '', opts.description ? `--description "${opts.description}"` : '', opts.yes ? '--yes' : '']
                .filter(Boolean)
                .join(' ');
            runSmooConfig(`push ${flags}`);
        });

    cfg.command('pull')
        .description('Pull config values from Smoo AI platform')
        .option('-e, --environment <env>', 'Environment name', 'development')
        .action((opts) => {
            runSmooConfig(`pull --environment ${opts.environment}`);
        });

    cfg.command('set <key> <value>')
        .description('Set a config value on Smoo AI platform')
        .option('-e, --environment <env>', 'Environment name', 'development')
        .option('--tier <tier>', 'Config tier (public, secret, feature_flag)')
        .option('--schema-name <name>', 'Schema name')
        .action((key, value, opts) => {
            const flags = [`--environment ${opts.environment}`, opts.tier ? `--tier ${opts.tier}` : '', opts.schemaName ? `--schema-name ${opts.schemaName}` : '']
                .filter(Boolean)
                .join(' ');
            runSmooConfig(`set "${key}" "${value}" ${flags}`);
        });

    cfg.command('get <key>')
        .description('Get a config value from Smoo AI platform')
        .option('-e, --environment <env>', 'Environment name', 'development')
        .action((key, opts) => {
            runSmooConfig(`get "${key}" --environment ${opts.environment}`);
        });

    cfg.command('list')
        .description('List all config values for an environment')
        .option('-e, --environment <env>', 'Environment name', 'development')
        .action((opts) => {
            runSmooConfig(`list --environment ${opts.environment}`);
        });

    cfg.command('diff')
        .description('Compare local schema vs remote schema')
        .option('--schema-name <name>', 'Schema name')
        .action((opts) => {
            runSmooConfig(`diff${opts.schemaName ? ` --schema-name ${opts.schemaName}` : ''}`);
        });

    // ── th smoo agents/jobs/knowledge ─── M2M API commands ───

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
