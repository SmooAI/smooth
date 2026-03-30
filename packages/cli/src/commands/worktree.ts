import type { Command } from 'commander';
import { execSync } from 'node:child_process';

export function registerWorktreeCommand(program: Command) {
    const wt = program.command('worktree').description('Git worktree management');

    wt.command('create <ticket> [description]')
        .description('Create feature worktree + branch')
        .action((ticket, description) => {
            const desc = description ? `-${description}` : '';
            const branch = `${ticket}${desc}`;
            const path = `../smooth-${branch}`;

            console.log(`Creating worktree: ${path}`);
            execSync(`git worktree add ${path} -b ${branch} main`, { stdio: 'inherit' });
            console.log(`\nWorktree created. Switch to it:`);
            console.log(`  cd ${path}`);
            console.log(`  pnpm install`);
        });

    wt.command('list')
        .description('List active worktrees')
        .action(() => {
            execSync('git worktree list', { stdio: 'inherit' });
        });

    wt.command('remove <ticket>')
        .description('Remove worktree + branch')
        .action((ticket) => {
            // Find worktree path matching ticket
            const list = execSync('git worktree list --porcelain', { encoding: 'utf8' });
            const lines = list.split('\n');
            for (const line of lines) {
                if (line.startsWith('worktree ') && line.includes(ticket)) {
                    const path = line.replace('worktree ', '');
                    console.log(`Removing worktree: ${path}`);
                    execSync(`git worktree remove ${path}`, { stdio: 'inherit' });
                    break;
                }
            }
            // Delete branch
            try {
                execSync(`git branch -d ${ticket} 2>/dev/null`, { stdio: 'inherit' });
            } catch {
                // Branch may not exist or may need -D
            }
        });

    wt.command('merge <ticket>')
        .description('Merge feature branch to main')
        .action((ticket) => {
            console.log(`Merging ${ticket} to main...`);
            execSync('git checkout main && git pull --rebase', { stdio: 'inherit' });
            execSync(`git merge ${ticket} --no-ff`, { stdio: 'inherit' });
            console.log(`\nMerged. Don't forget to push: git push`);
        });
}
