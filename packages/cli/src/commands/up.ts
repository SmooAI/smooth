import type { Command } from 'commander';
import { execSync, spawn } from 'node:child_process';

function isMsbInstalled(): boolean {
    try {
        execSync('msb --version', { stdio: 'pipe' });
        return true;
    } catch {
        return false;
    }
}

function isMsbServerRunning(): boolean {
    try {
        execSync('msb server status', { stdio: 'pipe' });
        return true;
    } catch {
        return false;
    }
}

function installMsb(): boolean {
    console.log('  Installing microsandbox...');
    try {
        execSync('curl -sSL https://get.microsandbox.dev | sh', {
            stdio: 'inherit',
            timeout: 60_000,
        });
        return true;
    } catch {
        console.error('  Failed to install microsandbox.');
        console.error('  Manual install: curl -sSL https://get.microsandbox.dev | sh');
        return false;
    }
}

export function registerUpCommand(program: Command) {
    program
        .command('up')
        .description('Start Smooth platform')
        .option('--no-leader', 'Skip starting the leader service')
        .action((opts) => {
            // 1. Check/install Microsandbox
            console.log('Checking Microsandbox...');
            if (!isMsbInstalled()) {
                console.log('  Microsandbox not found. Installing...');
                if (!installMsb()) {
                    process.exit(1);
                }
            } else {
                console.log('  Microsandbox: installed');
            }

            // 2. Start Microsandbox server
            if (!isMsbServerRunning()) {
                console.log('  Starting Microsandbox server...');
                try {
                    execSync('msb server start --dev', { stdio: 'pipe', timeout: 10_000 });
                    console.log('  Microsandbox server: started');
                } catch {
                    console.error('  Failed to start Microsandbox server.');
                    console.error('  Try manually: msb server start --dev');
                }
            } else {
                console.log('  Microsandbox server: running');
            }

            // 3. Start leader natively (SQLite DB auto-creates on first access)
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
                console.log('  Database:    ~/.smooth/smooth.db (SQLite)');
                console.log('  Sandbox:     Microsandbox (local microVMs)');
            } else {
                console.log('');
                console.log('Smooth infrastructure ready (leader skipped):');
                console.log('  Start leader manually: pnpm --filter @smooth/leader dev');
            }
        });
}
