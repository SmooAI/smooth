/** Typed wrapper around the bd CLI for Beads operations */

import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

import type { Bead, BeadComment, BeadDetail, BeadStatus, BeadType, Dependency, DependencyType } from '@smooth/shared/beads-types';

const exec = promisify(execFile);

interface BdOptions {
    cwd?: string;
    timeout?: number;
}

const DEFAULT_TIMEOUT = 30_000;

async function bd(args: string[], options: BdOptions = {}): Promise<string> {
    const { stdout } = await exec('bd', args, {
        cwd: options.cwd ?? process.env.BEADS_DIR ?? process.cwd(),
        timeout: options.timeout ?? DEFAULT_TIMEOUT,
    });
    return stdout.trim();
}

async function bdJson<T>(args: string[], options?: BdOptions): Promise<T> {
    const output = await bd([...args, '--json'], options);
    return JSON.parse(output) as T;
}

// ── Issue CRUD ──────────────────────────────────────────────

export async function createBead(opts: {
    title: string;
    description: string;
    type?: BeadType;
    priority?: number;
    labels?: string[];
}): Promise<string> {
    const args = ['create', '--title', opts.title, '--description', opts.description];
    if (opts.type) args.push('--type', opts.type);
    if (opts.priority !== undefined) args.push('--priority', String(opts.priority));
    if (opts.labels?.length) {
        for (const label of opts.labels) {
            args.push('--add-label', label);
        }
    }
    const output = await bd(args);
    // bd create outputs the new bead ID
    const match = output.match(/([A-Z]+-[a-z0-9]+)/);
    return match?.[1] ?? output;
}

export async function getBead(id: string): Promise<BeadDetail> {
    return bdJson<BeadDetail>(['show', id]);
}

export async function listBeads(filters?: {
    status?: BeadStatus;
    type?: string;
    label?: string;
}): Promise<Bead[]> {
    const args = ['list'];
    if (filters?.status) args.push(`--status=${filters.status}`);
    if (filters?.type) args.push(`--type=${filters.type}`);
    if (filters?.label) args.push(`--label=${filters.label}`);
    return bdJson<Bead[]>(args);
}

export async function updateBead(id: string, updates: {
    status?: BeadStatus;
    priority?: number;
    title?: string;
    addLabel?: string;
    removeLabel?: string;
}): Promise<void> {
    const args = ['update', id];
    if (updates.status) args.push(`--status=${updates.status}`);
    if (updates.priority !== undefined) args.push(`--priority=${updates.priority}`);
    if (updates.title) args.push(`--title=${updates.title}`);
    if (updates.addLabel) args.push(`--add-label=${updates.addLabel}`);
    if (updates.removeLabel) args.push(`--remove-label=${updates.removeLabel}`);
    await bd(args);
}

export async function closeBead(id: string, reason?: string): Promise<void> {
    const args = ['close', id];
    if (reason) args.push('--reason', reason);
    await bd(args);
}

// ── Comments / Messages ─────────────────────────────────────

export async function addComment(beadId: string, content: string, author?: string): Promise<void> {
    const args = ['comments', 'add', beadId, content];
    if (author) args.push('--author', author);
    await bd(args);
}

export async function getComments(beadId: string): Promise<BeadComment[]> {
    return bdJson<BeadComment[]>(['comments', beadId]);
}

export async function getThread(beadId: string): Promise<BeadDetail> {
    return bdJson<BeadDetail>(['show', '--thread', beadId]);
}

// ── Dependencies / Graph ────────────────────────────────────

export async function addDependency(issueId: string, dependsOnId: string, type?: DependencyType): Promise<void> {
    const args = ['dep', 'add', issueId, dependsOnId];
    if (type) args.push('--type', type);
    await bd(args);
}

export async function removeDependency(issueId: string, dependsOnId: string): Promise<void> {
    await bd(['dep', 'remove', issueId, dependsOnId]);
}

export async function listDependencies(issueId: string): Promise<Dependency[]> {
    return bdJson<Dependency[]>(['dep', 'list', issueId]);
}

export async function getDependencyTree(issueId: string): Promise<string> {
    return bd(['dep', 'tree', issueId]);
}

// ── Queries ─────────────────────────────────────────────────

export async function getReady(): Promise<Bead[]> {
    return bdJson<Bead[]>(['ready']);
}

export async function getBlocked(): Promise<Bead[]> {
    return bdJson<Bead[]>(['blocked']);
}

export async function search(query: string): Promise<Bead[]> {
    return bdJson<Bead[]>(['search', query]);
}

export async function getGraph(): Promise<string> {
    return bd(['graph', '--compact']);
}

// ── Jira Sync ───────────────────────────────────────────────

export async function jiraSync(direction?: 'pull' | 'push'): Promise<string> {
    const args = ['jira', 'sync'];
    if (direction === 'pull') args.push('--pull');
    if (direction === 'push') args.push('--push');
    return bd(args);
}

// ── Config ──────────────────────────────────────────────────

export async function getBeadsConfig(key: string): Promise<string> {
    return bd(['config', 'get', key]);
}

export async function setBeadsConfig(key: string, value: string): Promise<void> {
    await bd(['config', 'set', key, value]);
}
