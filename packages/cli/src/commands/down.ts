import type { Command } from 'commander';
import { execSync } from 'node:child_process';

export function registerDownCommand(program: Command) {
    program
        .command('down')
        .description('Stop Docker stack (preserves data volumes)')
        .action(() => {
            console.log('Stopping Smooth stack (data preserved)...');
            // NEVER pass -v — that would destroy the PostgreSQL volume
            execSync('docker compose -f docker/docker-compose.yml down', { stdio: 'inherit' });
            console.log('Smooth stopped. Data volumes preserved.');
        });
}
