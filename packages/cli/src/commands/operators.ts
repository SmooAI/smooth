import type { Command } from 'commander';

import { LeaderClient } from '../client/leader-client.js';
import { getActiveServerUrl, getApiKey } from '../config.js';

export function registerOperatorsCommand(program: Command) {
    const ops = program.command('operators').alias('workers').description('Smooth Operator management');

    ops.command('list')
        .description('List active Smooth Operators')
        .action(async () => {
            const url = getActiveServerUrl();
            const client = new LeaderClient(url, getApiKey(url));
            const result = await client.listOperators();
            if (result.data.length === 0) {
                console.log('No active Smooth Operators.');
                return;
            }
            console.log(`Active Smooth Operators: ${result.data.length}`);
            console.log(JSON.stringify(result.data, null, 2));
        });

    ops.command('kill <id>')
        .description('Kill a Smooth Operator')
        .action(async (id) => {
            const url = getActiveServerUrl();
            const client = new LeaderClient(url, getApiKey(url));
            await client.killOperator(id);
            console.log(`Smooth Operator ${id} killed.`);
        });

    // Default action: list
    ops.action(async () => {
        const url = getActiveServerUrl();
        const client = new LeaderClient(url, getApiKey(url));
        const result = await client.listOperators();
        if (result.data.length === 0) {
            console.log('No active Smooth Operators.');
            return;
        }
        console.log(`Active Smooth Operators: ${result.data.length}`);
        console.log(JSON.stringify(result.data, null, 2));
    });
}
