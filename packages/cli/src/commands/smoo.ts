import type { Command } from 'commander';

import { loadConfig, setConfigValue } from '../config.js';

export function registerSmooCommand(program: Command) {
    const smoo = program.command('smoo').description('SmooAI platform API');

    smoo.command('config')
        .description('Configure SmooAI M2M credentials')
        .action(() => {
            const config = loadConfig();
            console.log('SmooAI config:', JSON.stringify(config.smoo ?? {}, null, 2));
            console.log('\nSet with:');
            console.log('  th config set smoo.api_url https://api.smoo.ai');
            console.log('  th config set smoo.org_id <org-id>');
            console.log('  th config set smoo.client_id <client-id>');
        });

    smoo.command('agents')
        .description('List SmooAI agents')
        .action(async () => {
            const config = loadConfig();
            if (!config.smoo?.client_id) {
                console.error('SmooAI not configured. Run: th smoo config');
                process.exit(1);
            }
            // TODO: Use SmooClient from @smooth/smoo-api
            console.log('SmooAI agent listing — requires M2M auth (coming soon)');
        });

    smoo.command('jobs')
        .description('List SmooAI jobs')
        .action(async () => {
            console.log('SmooAI jobs — requires M2M auth (coming soon)');
        });
}
