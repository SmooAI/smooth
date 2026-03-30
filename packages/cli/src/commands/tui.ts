import type { Command } from 'commander';

export function registerTuiCommand(program: Command) {
    program
        .command('tui')
        .description('Launch full terminal UI')
        .option('--server <url>', 'Leader server URL')
        .action(async (opts) => {
            // Dynamic import to avoid loading React/Ink for non-TUI commands
            const { launchTui } = await import('../tui/launch.js');
            await launchTui(opts.server);
        });
}
