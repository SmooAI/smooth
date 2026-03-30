/** th audit — view audit logs */

import type { Command } from 'commander';

import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

const AUDIT_DIR = join(homedir(), '.smooth', 'audit');

export function registerAuditCommand(program: Command) {
    const audit = program.command('audit').description('View tool usage audit logs');

    audit
        .command('tail')
        .description('Show recent audit log entries')
        .argument('[actor]', 'Actor name (leader, operator-xxx)', 'leader')
        .option('-n, --lines <n>', 'Number of lines', '50')
        .action((actor, opts) => {
            const logFile = join(AUDIT_DIR, `${actor}.log`);
            if (!existsSync(logFile)) {
                console.log(`No audit log for ${actor}`);
                console.log(`Available: ${listActors().join(', ') || 'none'}`);
                return;
            }

            const content = readFileSync(logFile, 'utf8');
            const lines = content.trim().split('\n');
            const tail = lines.slice(-parseInt(opts.lines));
            console.log(tail.join('\n'));
        });

    audit
        .command('list')
        .description('List actors with audit logs')
        .action(() => {
            const actors = listActors();
            if (actors.length === 0) {
                console.log('No audit logs yet.');
                return;
            }

            console.log('Audit Logs');
            console.log('==========');
            for (const actor of actors) {
                const logFile = join(AUDIT_DIR, `${actor}.log`);
                const stats = statSync(logFile);
                console.log(`  ${actor.padEnd(24)} ${(stats.size / 1024).toFixed(1)} KB  ${stats.mtime.toISOString().slice(0, 19)}`);
            }
        });

    audit
        .command('path')
        .description('Show audit log directory')
        .action(() => {
            console.log(AUDIT_DIR);
        });

    // Default: list actors
    audit.action(() => {
        const actors = listActors();
        if (actors.length === 0) {
            console.log('No audit logs yet. Logs appear at ~/.smooth/audit/ when operators run.');
            return;
        }

        console.log(`${actors.length} actor(s) with audit logs in ${AUDIT_DIR}`);
        for (const actor of actors) {
            console.log(`  ${actor}`);
        }
        console.log('');
        console.log('View: th audit tail <actor>');
    });
}

function listActors(): string[] {
    if (!existsSync(AUDIT_DIR)) return [];
    return readdirSync(AUDIT_DIR)
        .filter((f) => f.endsWith('.log'))
        .map((f) => f.replace('.log', ''))
        .sort();
}
