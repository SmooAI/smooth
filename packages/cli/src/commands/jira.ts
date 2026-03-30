import type { Command } from 'commander';

import { getActiveServerUrl, getApiKey } from '../config.js';
import { LeaderClient } from '../client/leader-client.js';

export function registerJiraCommand(program: Command) {
    const jira = program.command('jira').description('Jira integration');

    jira.command('sync')
        .description('Bidirectional Jira ↔ Beads sync')
        .option('--pull', 'Pull only (Jira → Beads)')
        .option('--push', 'Push only (Beads → Jira)')
        .action(async (opts) => {
            const url = getActiveServerUrl();
            const client = new LeaderClient(url, getApiKey(url));
            const direction = opts.pull ? 'pull' : opts.push ? 'push' : undefined;
            const result = await client.jiraSync(direction);
            console.log('Jira sync complete:', JSON.stringify(result.data, null, 2));
        });

    jira.command('status')
        .description('Show Jira sync status')
        .action(async () => {
            const url = getActiveServerUrl();
            const client = new LeaderClient(url, getApiKey(url));
            const result = await client.jiraStatus();
            console.log(JSON.stringify(result.data, null, 2));
        });
}
