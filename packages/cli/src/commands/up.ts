import type { Command } from 'commander';
import { execSync, spawn } from 'node:child_process';

export function registerUpCommand(program: Command) {
    program
        .command('up')
        .description('Start Smooth platform (Colima + PostgreSQL + Microsandbox + Leader)')
        .option('--no-leader', 'Skip starting the leader service')
        .action((opts) => {
            // 1. Validate Docker runtime (Colima preferred, Docker Desktop also works)
            console.log('Checking Docker runtime...');
            try {
                execSync('docker info', { stdio: 'pipe' });
                console.log('  Docker runtime: available');
            } catch {
                console.error('  Docker runtime: not found');
                console.error('');
                console.error('Install Colima (preferred):');
                console.error('  brew install colima');
                console.error('  colima start --cpu 4 --memory 8');
                console.error('');
                console.error('Or use Docker Desktop: https://docker.com/products/docker-desktop/');
                process.exit(1);
            }

            // 2. Start PostgreSQL
            console.log('Starting PostgreSQL...');
            execSync('docker compose -f docker/docker-compose.yml up -d postgres', { stdio: 'inherit' });

            // 3. Validate/start Microsandbox server
            console.log('Checking Microsandbox server...');
            try {
                execSync('msb server status', { stdio: 'pipe' });
                console.log('  Microsandbox: running');
            } catch {
                console.log('  Microsandbox: starting...');
                try {
                    execSync('msb server start --dev', { stdio: 'pipe', timeout: 10_000 });
                    console.log('  Microsandbox: started');
                } catch {
                    console.error('  Microsandbox: failed to start');
                    console.error('  Install: curl -sSL https://get.microsandbox.dev | sh');
                    console.error('  Then: msb server start --dev');
                    // Non-fatal — leader can start without sandbox server
                }
            }

            // 4. Start leader natively
            if (opts.leader !== false) {
                console.log('Starting leader service...');
                const leader = spawn('pnpm', ['--filter', '@smooth/leader', 'dev'], {
                    stdio: 'inherit',
                    detached: false,
                });

                leader.on('error', (err) => {
                    console.error('Leader failed to start:', err.message);
                });

                console.log('');
                console.log('Smooth is running:');
                console.log('  Leader:      http://localhost:4400');
                console.log('  PostgreSQL:  localhost:5433');
                console.log('  Sandbox:     Microsandbox (local)');
            } else {
                console.log('');
                console.log('Smooth infrastructure is running (leader skipped):');
                console.log('  PostgreSQL:  localhost:5433');
                console.log('  Start leader manually: pnpm --filter @smooth/leader dev');
            }
        });
}
