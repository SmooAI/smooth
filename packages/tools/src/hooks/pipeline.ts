/** HookPipeline — runs hooks around tool execution */

import { createAuditLogger } from '@smooai/smooth-shared/audit-log';

import type { ToolContext } from '../types.js';
import type { Hook, HookContext, HookResult } from './types.js';

const audit = createAuditLogger('hooks');

export class HookPipeline {
    private hooks: Hook[] = [];

    register(hook: Hook): void {
        this.hooks.push(hook);
    }

    /** Run pre-tool hooks. Returns first rejection or allows. */
    async runPreHooks(toolName: string, input: unknown, toolContext: ToolContext): Promise<HookResult> {
        const ctx: HookContext = { toolName, input, toolContext };

        for (const hook of this.hooks) {
            if (hook.event !== 'pre-tool') continue;
            if (hook.tools.length > 0 && !hook.tools.includes(toolName)) continue;

            const result = await hook.handler(ctx);
            if (!result.allow) {
                audit.error(`Hook ${hook.name} blocked ${toolName}: ${result.reason}`, { hook: hook.name, tool: toolName });
                return result;
            }
        }

        return { allow: true };
    }

    /** Run post-tool hooks. Returns first rejection or allows. */
    async runPostHooks(toolName: string, input: unknown, output: unknown, toolContext: ToolContext): Promise<HookResult> {
        const ctx: HookContext = { toolName, input, output, toolContext };

        for (const hook of this.hooks) {
            if (hook.event !== 'post-tool') continue;
            if (hook.tools.length > 0 && !hook.tools.includes(toolName)) continue;

            const result = await hook.handler(ctx);
            if (!result.allow) {
                audit.error(`Post-hook ${hook.name} rejected ${toolName}: ${result.reason}`, { hook: hook.name, tool: toolName });
                return result;
            }
        }

        return { allow: true };
    }
}
