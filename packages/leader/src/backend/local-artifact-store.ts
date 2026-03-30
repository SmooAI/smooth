/** Filesystem-backed artifact store for local development */

import { existsSync, mkdirSync, readdirSync, readFileSync, unlinkSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

import type { ArtifactStore } from './types.js';

export class LocalArtifactStore implements ArtifactStore {
    private baseDir: string;

    constructor(baseDir?: string) {
        this.baseDir = baseDir ?? process.env.SMOOTH_ARTIFACTS_DIR ?? join(homedir(), '.smooth', 'artifacts');
    }

    private beadDir(beadId: string): string {
        const dir = join(this.baseDir, beadId);
        if (!existsSync(dir)) {
            mkdirSync(dir, { recursive: true });
        }
        return dir;
    }

    async put(beadId: string, name: string, content: Buffer | string): Promise<string> {
        const dir = this.beadDir(beadId);
        const key = `${beadId}/${name}`;
        writeFileSync(join(dir, name), content);
        return key;
    }

    async get(key: string): Promise<Buffer> {
        const filePath = join(this.baseDir, key);
        return readFileSync(filePath);
    }

    async list(beadId: string): Promise<string[]> {
        const dir = join(this.baseDir, beadId);
        if (!existsSync(dir)) return [];
        return readdirSync(dir).map((name) => `${beadId}/${name}`);
    }

    async delete(key: string): Promise<void> {
        const filePath = join(this.baseDir, key);
        if (existsSync(filePath)) {
            unlinkSync(filePath);
        }
    }
}
