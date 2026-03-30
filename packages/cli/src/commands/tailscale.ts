import type { Command } from 'commander';
import { execSync } from 'node:child_process';

export function registerTailscaleCommand(program: Command) {
    const ts = program.command('tailscale').alias('ts').description('Tailscale integration');

    ts.command('status')
        .description('Show Tailscale node status')
        .action(() => {
            try {
                execSync('tailscale status', { stdio: 'inherit' });
            } catch {
                console.error('Tailscale is not running or not installed.');
            }
        });

    ts.command('setup')
        .description('Interactive Tailscale setup')
        .action(() => {
            console.log('Tailscale setup for Smooth:');
            console.log('');
            console.log('1. Install Tailscale: https://tailscale.com/download');
            console.log('2. Authenticate: tailscale up');
            console.log('3. The Docker stack will auto-register via TS_AUTHKEY');
            console.log('4. Web will be available at: https://smooth.<tailnet>.ts.net');
            console.log('5. API at: https://smooth-api.<tailnet>.ts.net');
            console.log('');
            console.log('Configure auth key in docker/.env:');
            console.log('  TS_AUTHKEY=tskey-auth-...');
        });

    // Default: status
    ts.action(() => {
        try {
            execSync('tailscale status', { stdio: 'inherit' });
        } catch {
            console.error('Tailscale is not running or not installed.');
        }
    });
}
