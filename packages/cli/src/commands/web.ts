import type { Command } from 'commander';

import { execSync } from 'node:child_process';

export function registerWebCommand(program: Command) {
    program
        .command('web')
        .description('Open Smooth web interface in browser')
        .action(async () => {
            // Try Tailscale first
            let url: string | null = null;
            try {
                const status = execSync('tailscale status --json 2>/dev/null', { encoding: 'utf8' });
                const parsed = JSON.parse(status);
                const selfDns = parsed.Self?.DNSName;
                if (selfDns) {
                    // Look for smooth hostname on the tailnet
                    const tailnet = selfDns.split('.').slice(1).join('.');
                    url = `https://smooth.${tailnet}`;
                }
            } catch {
                // Tailscale not available
            }

            if (!url) {
                url = 'http://localhost:3100';
            }

            console.log(`Opening ${url} ...`);

            const openMod = await import('open');
            await openMod.default(url);
        });
}
