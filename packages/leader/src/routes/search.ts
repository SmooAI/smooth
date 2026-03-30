/** Search route — powers @ autocomplete in chat (beads, files, paths) */

import { Hono } from 'hono';
import { execSync } from 'node:child_process';
import { existsSync, readdirSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { glob } from 'tinyglobby';

export const searchRoutes = new Hono();

interface SearchResult {
    type: 'bead' | 'file' | 'path';
    id: string;
    label: string;
    detail?: string;
}

/** Search beads by title/id */
function searchBeads(query: string): SearchResult[] {
    try {
        const output = execSync('bd list --json 2>/dev/null', { encoding: 'utf8', timeout: 5000 });
        const beads = JSON.parse(output) as Array<{ id: string; title: string; status: string; priority: number }>;
        const q = query.toLowerCase();
        return beads
            .filter((b) => b.id.toLowerCase().includes(q) || b.title.toLowerCase().includes(q))
            .slice(0, 10)
            .map((b) => ({ type: 'bead', id: b.id, label: `${b.id}: ${b.title}`, detail: `${b.status} P${b.priority}` }));
    } catch {
        return [];
    }
}

/** Search files using tinyglobby (extremely fast) */
async function searchFiles(query: string, basePath: string): Promise<SearchResult[]> {
    const q = query.toLowerCase().replace(/[^a-z0-9._-]/g, '');
    if (!q) return [];

    const pattern = `**/*${q}*`;
    const files = await glob([pattern], {
        cwd: basePath,
        ignore: ['node_modules/**', '.next/**', 'dist/**', '.git/**', '.beads/**', '.turbo/**', '*.tsbuildinfo'],
        deep: 4,
    });

    return files.slice(0, 15).map((f) => {
        const isDir = f.endsWith('/');
        return { type: 'file', id: f, label: f, detail: isDir ? 'dir' : 'file' };
    });
}

/** Expand path (supports ~) and list entries */
function searchPaths(query: string): SearchResult[] {
    let expanded = query.replace(/^~/, homedir());
    expanded = resolve(expanded);

    if (existsSync(expanded)) {
        try {
            const stat = statSync(expanded);
            if (stat.isDirectory()) {
                return readdirSync(expanded)
                    .filter((e) => !e.startsWith('.'))
                    .slice(0, 15)
                    .map((e) => {
                        const full = join(expanded, e);
                        const isDir = existsSync(full) && statSync(full).isDirectory();
                        return { type: 'path', id: join(query, e), label: isDir ? e + '/' : e, detail: isDir ? 'dir' : 'file' };
                    });
            }
        } catch {
            /* skip */
        }
    }

    const parent = resolve(expanded, '..');
    const partial = basename(expanded).toLowerCase();
    if (existsSync(parent)) {
        try {
            return readdirSync(parent)
                .filter((e) => e.toLowerCase().startsWith(partial) && !e.startsWith('.'))
                .slice(0, 15)
                .map((e) => {
                    const full = join(parent, e);
                    const isDir = existsSync(full) && statSync(full).isDirectory();
                    const parentQuery = query.replace(/[^/]*$/, '');
                    return { type: 'path', id: parentQuery + e, label: isDir ? e + '/' : e, detail: isDir ? 'dir' : 'file' };
                });
        } catch {
            /* skip */
        }
    }

    return [];
}

/** GET /api/search?q=<query>&type=beads|files|paths|all */
searchRoutes.get('/', async (c) => {
    const query = c.req.query('q') ?? '';
    const searchType = c.req.query('type') ?? 'all';

    if (!query) return c.json({ data: [], ok: true });

    // @~ or @/ triggers path search
    if (query.startsWith('~') || query.startsWith('/')) {
        const results = searchPaths(query);
        return c.json({ data: results, ok: true });
    }

    const results: SearchResult[] = [];

    if (searchType === 'beads' || searchType === 'all') {
        results.push(...searchBeads(query));
    }
    if (searchType === 'files' || searchType === 'all') {
        results.push(...(await searchFiles(query, process.cwd())));
    }
    if (searchType === 'paths') {
        results.push(...searchPaths(query));
    }

    return c.json({ data: results.slice(0, 15), ok: true });
});
