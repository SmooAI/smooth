import type { Command } from 'commander';
import { copyFileSync, existsSync, readdirSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

const DB_PATH = join(homedir(), '.smooth', 'smooth.db');
const BACKUP_DIR = join(homedir(), '.smooth', 'backups');

export function registerDbCommand(program: Command) {
    const db = program.command('db').description('Database management');

    db.command('path')
        .description('Show database file path')
        .action(() => {
            console.log(DB_PATH);
        });

    db.command('status')
        .description('Show database status')
        .action(() => {
            if (existsSync(DB_PATH)) {
                const stats = statSync(DB_PATH);
                console.log(`Database: ${DB_PATH}`);
                console.log(`Size: ${(stats.size / 1024).toFixed(1)} KB`);
                console.log(`Modified: ${stats.mtime.toISOString()}`);
            } else {
                console.log('Database not created yet. Run `th up` to start.');
            }
        });

    db.command('backup')
        .description('Backup database')
        .action(() => {
            if (!existsSync(DB_PATH)) {
                console.error('No database to backup.');
                return;
            }
            const { mkdirSync } = require('node:fs');
            if (!existsSync(BACKUP_DIR)) mkdirSync(BACKUP_DIR, { recursive: true });

            const timestamp = new Date().toISOString().replace(/[:.]/g, '-');
            const backupPath = join(BACKUP_DIR, `smooth-${timestamp}.db`);
            copyFileSync(DB_PATH, backupPath);
            console.log(`Backup saved to: ${backupPath}`);
        });

    db.command('backups')
        .description('List available backups')
        .action(() => {
            if (!existsSync(BACKUP_DIR)) {
                console.log('No backups found.');
                return;
            }
            const files = readdirSync(BACKUP_DIR)
                .filter((f) => f.endsWith('.db'))
                .sort()
                .reverse();
            if (files.length === 0) {
                console.log('No backups found.');
                return;
            }
            console.log(`Backups (${BACKUP_DIR}):`);
            for (const file of files) {
                const stats = statSync(join(BACKUP_DIR, file));
                console.log(`  ${file}  (${(stats.size / 1024).toFixed(1)} KB)`);
            }
        });

    db.command('restore <file>')
        .description('Restore from backup file')
        .action((file) => {
            const src = existsSync(file) ? file : join(BACKUP_DIR, file);
            if (!existsSync(src)) {
                console.error(`Backup not found: ${file}`);
                return;
            }
            copyFileSync(src, DB_PATH);
            console.log(`Restored from: ${src}`);
        });
}
