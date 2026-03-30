import type { Command } from 'commander';

import { getActiveServerUrl, getApiKey } from '../config.js';
import { LeaderClient } from '../client/leader-client.js';

export function registerApproveCommand(program: Command) {
    program
        .command('approve <beadId>')
        .description('Approve a pending review')
        .action(async (beadId) => {
            const url = getActiveServerUrl();
            const client = new LeaderClient(url, getApiKey(url));
            await client.approveReview(beadId);
            console.log(`Review approved for bead ${beadId}`);
        });

    program
        .command('reject <beadId>')
        .description('Reject a review')
        .requiredOption('-m, --message <msg>', 'Rejection reason')
        .action(async (beadId, opts) => {
            const url = getActiveServerUrl();
            const client = new LeaderClient(url, getApiKey(url));
            await client.rejectReview(beadId, opts.message);
            console.log(`Review rejected for bead ${beadId}`);
        });
}
