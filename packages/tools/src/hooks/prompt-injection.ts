/** Prompt injection detection hook
 *
 * Scans file content and tool inputs for patterns that attempt to
 * manipulate the agent's behavior through injected instructions.
 *
 * This is defense-in-depth — not perfect, but catches common patterns.
 */

import type { Hook } from './types.js';

/** Patterns that commonly appear in prompt injection attempts */
const INJECTION_PATTERNS = [
    // Direct instruction overrides
    { name: 'system_prompt_override', pattern: /(?:ignore|disregard|forget)\s+(?:all\s+)?(?:previous|above|prior)\s+(?:instructions|prompts|rules)/i },
    { name: 'new_instructions', pattern: /(?:new|updated|revised)\s+(?:system\s+)?instructions?\s*:/i },
    { name: 'role_hijack', pattern: /you\s+are\s+now\s+(?:a|an)\s+(?:different|new|unrestricted)/i },
    { name: 'jailbreak_dan', pattern: /(?:DAN|do\s+anything\s+now|developer\s+mode|god\s+mode)/i },

    // Hidden instruction markers
    { name: 'hidden_instruction', pattern: /<!--\s*(?:SYSTEM|INSTRUCTION|PROMPT|IMPORTANT)\s*:/i },
    { name: 'base64_instruction', pattern: /(?:decode|execute|eval)\s+(?:this\s+)?base64/i },
    { name: 'invisible_chars', pattern: /[\u200B\u200C\uFEFF\u2060]{3,}/ }, // Zero-width chars used to hide text

    // Tool manipulation
    { name: 'tool_override', pattern: /(?:override|bypass|skip|disable)\s+(?:the\s+)?(?:security|safety|hook|guard|permission|validation)/i },
    { name: 'exfiltration', pattern: /(?:send|post|upload|exfiltrate)\s+(?:this|the|all)\s+(?:the\s+)?(?:data|content|code|secrets?|keys?|tokens?)\s+to/i },

    // Delimiter attacks
    { name: 'fake_system_msg', pattern: /\[(?:SYSTEM|ADMIN|ROOT)\]\s*:/i },
    { name: 'prompt_boundary', pattern: /(?:---+|===+)\s*(?:END|BEGIN)\s+(?:OF\s+)?(?:SYSTEM|USER|CONTEXT)/i },
];

/** Low-confidence patterns — logged but not blocked */
const SUSPICIOUS_PATTERNS = [
    { name: 'instruction_language', pattern: /(?:you\s+must|always\s+respond|never\s+refuse|do\s+not\s+question)/i },
    { name: 'output_manipulation', pattern: /(?:respond\s+only\s+with|output\s+exactly|say\s+nothing\s+except)/i },
];

export interface InjectionScanResult {
    blocked: boolean;
    findings: Array<{ pattern: string; severity: 'high' | 'low' }>;
}

export function scanForInjection(content: string): InjectionScanResult {
    const findings: InjectionScanResult['findings'] = [];

    for (const { name, pattern } of INJECTION_PATTERNS) {
        if (pattern.test(content)) {
            findings.push({ pattern: name, severity: 'high' });
        }
    }

    for (const { name, pattern } of SUSPICIOUS_PATTERNS) {
        if (pattern.test(content)) {
            findings.push({ pattern: name, severity: 'low' });
        }
    }

    return {
        blocked: findings.some((f) => f.severity === 'high'),
        findings,
    };
}

/** Pre-tool hook: scans inputs for prompt injection patterns */
export const promptInjectionHook: Hook = {
    name: 'prompt-injection-guard',
    event: 'pre-tool',
    tools: [], // Apply to all tools
    handler: async (ctx) => {
        // Scan all string values in the input
        const content = typeof ctx.input === 'string' ? ctx.input : JSON.stringify(ctx.input);

        const result = scanForInjection(content);

        if (result.blocked) {
            const patterns = result.findings
                .filter((f) => f.severity === 'high')
                .map((f) => f.pattern)
                .join(', ');
            return {
                allow: false,
                reason: `Blocked: potential prompt injection detected (${patterns}). Input contains patterns that attempt to override agent instructions.`,
            };
        }

        return { allow: true };
    },
};
