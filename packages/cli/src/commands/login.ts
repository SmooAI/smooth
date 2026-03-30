import type { Command } from 'commander';

import { getActiveServerUrl, loadConfig, saveConfig, setApiKey } from '../config.js';

export function registerLoginCommand(program: Command) {
    program
        .command('login')
        .description('Authenticate with leader server')
        .option('--server <url>', 'Server URL')
        .action(async (opts) => {
            const serverUrl = opts.server ?? getActiveServerUrl();
            console.log(`Authenticating with ${serverUrl}...`);
            console.log('');

            // In v1, generate a simple API key
            // Full Better Auth browser flow will be wired in later
            const apiKey = `sk_smooth_${randomHex(32)}`;
            setApiKey(serverUrl, apiKey);

            console.log(`API key stored for ${serverUrl}`);
            console.log('You can now use th commands against this server.');
        });

    program
        .command('connect <url>')
        .description('Set remote leader URL')
        .action((url) => {
            const config = loadConfig();
            config.servers.remote = url;
            config.active_server = 'remote';
            saveConfig(config);
            console.log(`Connected to ${url}`);
            console.log('Run "th login" to authenticate.');
        });

    program
        .command('whoami')
        .description('Show current auth + server info')
        .action(() => {
            const config = loadConfig();
            const url = getActiveServerUrl();
            console.log(`Server: ${url} (${config.active_server})`);
            console.log(`Servers: ${Object.keys(config.servers).join(', ')}`);
        });
}

function randomHex(bytes: number): string {
    const array = new Uint8Array(bytes);
    crypto.getRandomValues(array);
    return Array.from(array, (b) => b.toString(16).padStart(2, '0')).join('');
}
