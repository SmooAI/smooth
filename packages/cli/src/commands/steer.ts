/** th pause/steer/cancel — mid-task operator intervention */

import type { Command } from 'commander';

import { getActiveServerUrl, getApiKey } from '../config.js';

function apiUrl(path: string): string {
    return `${getActiveServerUrl()}/api/steering${path}`;
}

async function post(path: string, body?: Record<string, unknown>): Promise<void> {
    const response = await fetch(apiUrl(path), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${getApiKey(getActiveServerUrl())}` },
        body: body ? JSON.stringify(body) : undefined,
    });

    if (!response.ok) {
        const text = await response.text();
        console.error(`Error: ${response.status} ${text}`);
        process.exit(1);
    }

    const result = (await response.json()) as { ok: boolean; action: string };
    console.log(`${result.action}`);
}

export function registerSteerCommand(program: Command) {
    program
        .command('pause <bead-id>')
        .description('Pause a running Smooth Operator')
        .action(async (beadId) => {
            await post(`/${beadId}/pause`);
            console.log(`Operator on bead ${beadId} paused. Resume: th resume ${beadId}`);
        });

    program
        .command('resume <bead-id>')
        .description('Resume a paused Smooth Operator')
        .action(async (beadId) => {
            await post(`/${beadId}/resume`);
        });

    program
        .command('steer <bead-id> <message>')
        .description('Send guidance to a running Smooth Operator')
        .action(async (beadId, message) => {
            await post(`/${beadId}/steer`, { message });
            console.log(`Guidance sent to operator on bead ${beadId}`);
        });

    program
        .command('cancel <bead-id>')
        .description('Cancel a running Smooth Operator')
        .action(async (beadId) => {
            await post(`/${beadId}/cancel`);
            console.log(`Operator on bead ${beadId} cancelled.`);
        });
}
