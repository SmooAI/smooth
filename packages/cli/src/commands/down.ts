import type { Command } from 'commander';
import { execSync } from 'node:child_process';

export function registerDownCommand(program: Command) {
    program
        .command('down')
        .description('Stop Smooth platform (preserves data volumes)')
        .action(() => {
            console.log('Stopping Smooth...');

            // Stop PostgreSQL (NEVER pass -v — that destroys the data volume)
            console.log('Stopping PostgreSQL...');
            execSync('docker compose -f docker/docker-compose.yml down', { stdio: 'inherit' });

            console.log('Smooth stopped. Data volumes preserved.');
            console.log('Leader process (if running) should be stopped separately (Ctrl+C).');
        });
}
