import type { Command } from 'commander';
import { execSync } from 'node:child_process';

export function registerUpCommand(program: Command) {
    program
        .command('up')
        .description('Start full Docker stack')
        .action(() => {
            console.log('Starting Smooth stack...');
            execSync('docker compose -f docker/docker-compose.yml up -d', { stdio: 'inherit' });
            console.log('\nSmooth is running.');
            console.log('  Leader:  http://localhost:4400');
            console.log('  Web:     http://localhost:3100');
        });
}
