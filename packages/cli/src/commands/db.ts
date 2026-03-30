import type { Command } from 'commander';
import { execSync } from 'node:child_process';

export function registerDbCommand(program: Command) {
    const db = program.command('db').description('Database management');

    db.command('backup')
        .description('Backup PostgreSQL data')
        .action(() => {
            console.log('Backing up...');
            execSync('bash docker/postgres/backup.sh', { stdio: 'inherit' });
        });

    db.command('restore <file>')
        .description('Restore from backup')
        .action((file) => {
            execSync(`bash docker/postgres/restore.sh ${file}`, { stdio: 'inherit' });
        });

    db.command('status')
        .description('Show database status')
        .action(() => {
            try {
                execSync('docker compose -f docker/docker-compose.yml exec postgres pg_isready -U smooth', { stdio: 'inherit' });
                console.log('\nDatabase is healthy.');
            } catch {
                console.error('Database is not reachable.');
            }
        });

    db.command('backups')
        .description('List available backups')
        .action(() => {
            try {
                execSync('ls -lh docker/postgres/backups/*.sql.gz 2>/dev/null', { stdio: 'inherit' });
            } catch {
                console.log('No backups found.');
            }
        });
}
