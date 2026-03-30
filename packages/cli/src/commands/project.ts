import type { Command } from 'commander';

import { getActiveServerUrl, getApiKey } from '../config.js';
import { LeaderClient } from '../client/leader-client.js';

export function registerProjectCommand(program: Command) {
    const project = program.command('project').description('Project management');

    project
        .command('list')
        .description('List projects')
        .action(async () => {
            const url = getActiveServerUrl();
            const client = new LeaderClient(url, getApiKey(url));
            const result = await client.listProjects();
            console.log(JSON.stringify(result.data, null, 2));
        });

    project
        .command('create <name>')
        .description('Create a project')
        .option('-d, --description <desc>', 'Project description', '')
        .action(async (name, opts) => {
            const url = getActiveServerUrl();
            const client = new LeaderClient(url, getApiKey(url));
            const result = await client.createProject(name, opts.description || name);
            console.log(`Project created:`, JSON.stringify(result.data, null, 2));
        });
}
