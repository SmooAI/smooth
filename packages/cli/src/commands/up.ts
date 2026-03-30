import type { Command } from 'commander';

import { execSync, spawn } from 'node:child_process';

function isInstalled(cmd: string): boolean {
    try {
        execSync(`which ${cmd}`, { stdio: 'pipe' });
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

function brewInstall(pkg: string): boolean {
    console.log(`  Installing ${pkg} via brew...`);
    try {
        execSync(`brew install ${pkg}`, { stdio: 'inherit', timeout: 120_000 });
        return true;
    } catch {
        console.error(`  Failed to install ${pkg}.`);
        console.error(`  Manual install: brew install ${pkg}`);
        return false;
    }
}

function installMsb(): boolean {
    console.log('  Installing microsandbox...');
    try {
        execSync('curl -sSL https://get.microsandbox.dev | sh', { stdio: 'inherit', timeout: 60_000 });
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
            // 1. Check/install OpenCode
            console.log('Checking OpenCode...');
            if (!isInstalled('opencode')) {
                console.log('  OpenCode not found.');
                if (!brewInstall('opencode')) process.exit(1);
            } else {
                try {
                    const ver = execSync('opencode --version', { encoding: 'utf8', stdio: 'pipe' }).trim();
                    console.log(`  OpenCode: ${ver}`);
                } catch {
                    console.log('  OpenCode: installed');
                }
            }

            // 2. Check/install Microsandbox
            console.log('Checking Microsandbox...');
            if (!isInstalled('msb')) {
                console.log('  Microsandbox not found.');
                if (!installMsb()) process.exit(1);
            } else {
                console.log('  Microsandbox: installed');
            }

            // 3. Start Microsandbox server
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

            // 4. Start leader natively (SQLite DB auto-creates on first access)
            if (opts.leader !== false) {
                console.log('Starting leader service...');
                const leader = spawn('pnpm', ['--filter', '@smooai/smooth-leader', 'dev'], {
                    stdio: 'inherit',
                    detached: false,
                });

                leader.on('error', (err) => {
                    console.error('Leader failed to start:', err.message);
                });

                console.log('');
                console.log('Smooth is running:');
                console.log('  Leader:      http://localhost:4400');
                console.log('  WebSocket:   ws://localhost:4400/ws');
                console.log('  Database:    ~/.smooth/smooth.db (SQLite)');
                console.log('  Sandbox:     Microsandbox (local microVMs)');
                console.log('  Operators:   OpenCode Zen');
            } else {
                console.log('');
                console.log('Smooth infrastructure ready (leader skipped):');
                console.log('  Start leader manually: pnpm --filter @smooai/smooth-leader dev');
            }
        });
}
