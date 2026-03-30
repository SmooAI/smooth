import type { Command } from 'commander';

import { getActiveServerUrl, getApiKey } from '../config.js';
import { LeaderClient } from '../client/leader-client.js';

export function registerRunCommand(program: Command) {
    program
        .command('run <beadId>')
        .description('Trigger work on a bead')
        .action(async (beadId) => {
            const url = getActiveServerUrl();
            const client = new LeaderClient(url, getApiKey(url));
            await client.sendMessage(beadId, `Run requested for bead ${beadId}`, 'human→leader');
            console.log(`Run triggered for bead ${beadId}`);
        });
}
