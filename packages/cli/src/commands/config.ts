import type { Command } from 'commander';

import { loadConfig, setConfigValue } from '../config.js';

export function registerConfigCommand(program: Command) {
    const cfg = program.command('config').description('View/set configuration');

    cfg.command('show')
        .description('Show current configuration')
        .action(() => {
            const config = loadConfig();
            console.log(JSON.stringify(config, null, 2));
        });

    cfg.command('set <key> <value>')
        .description('Set a config value (e.g., jira.url, smoo.api_url)')
        .action((key, value) => {
            setConfigValue(key, value);
            console.log(`Set ${key} = ${value}`);
        });

    // Default: show config
    cfg.action(() => {
        const config = loadConfig();
        console.log(JSON.stringify(config, null, 2));
    });
}
