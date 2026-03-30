import type { Command } from 'commander';

import { getActiveServerUrl, getApiKey } from '../config.js';
import { LeaderClient } from '../client/leader-client.js';

export function registerStatusCommand(program: Command) {
    program
        .command('status')
        .description('Show system health')
        .option('--server <url>', 'Leader server URL')
        .action(async (opts) => {
            const url = opts.server ?? getActiveServerUrl();
            const client = new LeaderClient(url, getApiKey(url));

            try {
                const health = await client.getHealth();
                console.log(`Smooth Leader: ${url}`);
                console.log(JSON.stringify(health, null, 2));
            } catch (error) {
                console.error(`Cannot reach leader at ${url}:`, (error as Error).message);
                process.exit(1);
            }
        });
}
