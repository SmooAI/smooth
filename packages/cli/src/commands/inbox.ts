import type { Command } from 'commander';

import { LeaderClient } from '../client/leader-client.js';
import { getActiveServerUrl, getApiKey } from '../config.js';

export function registerInboxCommand(program: Command) {
    program
        .command('inbox')
        .description('Show messages requiring attention')
        .action(async () => {
            const url = getActiveServerUrl();
            const client = new LeaderClient(url, getApiKey(url));
            const result = await client.getInbox();
            if (result.data.length === 0) {
                console.log('Inbox is empty.');
                return;
            }
            for (const item of result.data) {
                const action = item.requiresAction ? ` [${item.actionType}]` : '';
                console.log(`${item.message.beadId} | ${item.beadTitle}${action}`);
                console.log(`  ${item.message.content}`);
                console.log();
            }
        });
}
