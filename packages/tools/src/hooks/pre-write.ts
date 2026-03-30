/** Pre-write hook — blocks secrets in code and path traversal */

import { resolve } from 'node:path';

import type { Hook } from './types.js';

/** Regex patterns that match common secret formats */
const SECRET_PATTERNS = [
    { name: 'AWS Access Key', pattern: /AKIA[0-9A-Z]{16}/ },
    { name: 'AWS Secret Key', pattern: /[0-9a-zA-Z/+]{40}/ },
    { name: 'OpenAI API Key', pattern: /sk-[a-zA-Z0-9]{48,}/ },
    { name: 'Anthropic API Key', pattern: /sk-ant-[a-zA-Z0-9-]{80,}/ },
    { name: 'GitHub Token', pattern: /ghp_[a-zA-Z0-9]{36}/ },
    { name: 'GitHub OAuth', pattern: /gho_[a-zA-Z0-9]{36}/ },
    { name: 'Slack Token', pattern: /xox[baprs]-[0-9a-zA-Z-]+/ },
    { name: 'Private Key', pattern: /-----BEGIN\s*(RSA|EC|DSA|OPENSSH)?\s*PRIVATE KEY-----/ },
    { name: 'Generic Secret Assignment', pattern: /(?:password|secret|token|api_key|apikey)\s*[=:]\s*['"][^'"]{8,}['"]/i },
    { name: 'Bearer Token', pattern: /Bearer\s+[a-zA-Z0-9._~+/=-]{20,}/ },
    { name: 'Connection String', pattern: /(?:postgres|mysql|mongodb|redis):\/\/[^:]+:[^@]+@/ },
];

const WORKSPACE_ROOT = '/workspace';

function checkSecrets(content: string): { found: boolean; matches: string[] } {
    const matches: string[] = [];
    for (const { name, pattern } of SECRET_PATTERNS) {
        if (pattern.test(content)) {
            matches.push(name);
        }
    }
    return { found: matches.length > 0, matches };
}

function checkPathTraversal(filePath: string): { safe: boolean; reason?: string } {
    const resolved = resolve(WORKSPACE_ROOT, filePath);
    if (!resolved.startsWith(WORKSPACE_ROOT)) {
        return { safe: false, reason: `Path escapes workspace: ${filePath} resolves to ${resolved}` };
    }
    if (filePath.includes('..')) {
        return { safe: false, reason: `Path contains traversal: ${filePath}` };
    }
    return { safe: true };
}

/** Pre-write hook: blocks secrets and path traversal in file write operations */
export const preWriteHook: Hook = {
    name: 'pre-write-guard',
    event: 'pre-tool',
    tools: ['artifact_write', 'beads_message', 'progress_append'],
    handler: async (ctx) => {
        const input = ctx.input as Record<string, unknown>;

        // Check for secrets in any string value
        const content = typeof input.content === 'string' ? input.content : typeof input.text === 'string' ? input.text : JSON.stringify(input);

        const secrets = checkSecrets(content);
        if (secrets.found) {
            return {
                allow: false,
                reason: `Blocked: potential secrets detected (${secrets.matches.join(', ')}). Never write secrets into code or messages. Use environment variables instead.`,
            };
        }

        // Check path traversal for file-related tools
        const filePath = typeof input.path === 'string' ? input.path : typeof input.name === 'string' ? input.name : null;
        if (filePath) {
            const pathCheck = checkPathTraversal(filePath);
            if (!pathCheck.safe) {
                return { allow: false, reason: `Blocked: ${pathCheck.reason}` };
            }
        }

        return { allow: true };
    },
};

/** Export the secret patterns for testing */
export { SECRET_PATTERNS };
