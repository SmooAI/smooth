/** th — Smooth CLI entry point */

import { Command } from 'commander';

import { registerApproveCommand } from './commands/approve.js';
import { registerAuditCommand } from './commands/audit.js';
import { registerAuthCommand } from './commands/auth.js';
import { registerConfigCommand } from './commands/config.js';
import { registerDbCommand } from './commands/db.js';
import { registerDownCommand } from './commands/down.js';
import { registerInboxCommand } from './commands/inbox.js';
import { registerJiraCommand } from './commands/jira.js';
import { registerLoginCommand } from './commands/login.js';
import { registerOperatorsCommand } from './commands/operators.js';
import { registerProjectCommand } from './commands/project.js';
import { registerRunCommand } from './commands/run.js';
import { registerSmooCommand } from './commands/smoo.js';
import { registerStatusCommand } from './commands/status.js';
import { registerSteerCommand } from './commands/steer.js';
import { registerTailscaleCommand } from './commands/tailscale.js';
import { registerTuiCommand } from './commands/tui.js';
import { registerUpCommand } from './commands/up.js';
import { registerWebCommand } from './commands/web.js';
import { registerWorktreeCommand } from './commands/worktree.js';

const program = new Command().name('th').description('Smoo AI CLI — agent orchestration, config management, and platform tools').version('0.1.0');

// Register all commands
registerAuditCommand(program);
registerAuthCommand(program);
registerStatusCommand(program);
registerTuiCommand(program);
registerWebCommand(program);
registerProjectCommand(program);
registerRunCommand(program);
registerApproveCommand(program);
registerInboxCommand(program);
registerOperatorsCommand(program);
registerConfigCommand(program);
registerJiraCommand(program);
registerSmooCommand(program);
registerDbCommand(program);
registerUpCommand(program);
registerDownCommand(program);
registerWorktreeCommand(program);
registerTailscaleCommand(program);
registerSteerCommand(program);
registerLoginCommand(program);

program.parse();
