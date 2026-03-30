import type { Command } from 'commander';

import { execSync } from 'node:child_process';

export function registerDownCommand(program: Command) {
    program
        .command('down')
        .description('Stop Smooth platform')
        .option('--msb', 'Also stop Microsandbox server')
        .action((opts) => {
            console.log('Stopping Smooth...');

            // Leader is a foreground process — user stops it with Ctrl+C
            console.log('  Leader: stop with Ctrl+C in the terminal running it');

            // Optionally stop Microsandbox server
            if (opts.msb) {
                try {
                    execSync('msb server stop', { stdio: 'pipe' });
                    console.log('  Microsandbox server: stopped');
                } catch {
                    console.log('  Microsandbox server: already stopped');
                }
            }

            console.log('');
            console.log('Database preserved at ~/.smooth/smooth.db');
        });
}
