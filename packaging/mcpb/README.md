# Smooth `.mcpb` Desktop Extension

One-click-install the Smooth MCP server into Claude Desktop. The bundle wraps the
compiled `th` binary and runs `th mcp serve`, a stdio MCP server that exposes
`th` as MCP tools. There's no env config: the server reads `~/.smooth/` for
pearls, mail and SmoothFlow, and `~/.config/smooth/auth/` for Smoo sign-in.

Claude Desktop without the bundle: `th mcp install --harness claude-desktop`
writes the same server into Claude Desktop's config on macOS, Windows or Linux.

## Tools

`manifest.json` lists every tool the server exposes. A test in
`crates/smooth-cli/src/mcp_serve.rs` (`mcpb_manifest_lists_every_tool`) pins that
list to the server, so a new tool fails CI until you re-bless it:
`SMOOTH_MCPB_BLESS=1 cargo test -p smooai-smooth-cli mcpb_manifest && pnpm format`.

| Group             | Tools                                                                                                                                                                                                                              | Needs                                              |
| ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------- |
| **SmoothFlow**    | `flow_list`, `flow_snapshot`, `flow_handoff`, `flow_harnesses`, `flow_repos`, `flow_infer` (read)<br>`flow_new`, `flow_send`, `flow_prompt_wait`, `flow_approve`, `flow_kill`, `flow_close`, `flow_fanout_new`, `flow_fanout_pick` | a running flow engine: SmoothFlow open, or `th up` |
| **Pearls, notes** | `pearls_ready`, `pearls_create`, `remember`, `recall`                                                                                                                                                                              | nothing                                            |
| **Agent mail**    | `agent_identity`, `agent_status`, `agent_list`, `mail_inbox`, `mail_send`, `mail_ack`                                                                                                                                              | nothing                                            |
| **Your business** | `ask_business`, `knowledge_search`, `operator_tools`, `operator_tools_set`, `observability_*`, `email_*`                                                                                                                           | `smoo auth login`                                  |

Pearls and notes act on the store of the **workspace the server is launched in**.
Every write tool is annotated, and `SMOOTH_MCP_ALLOW_WRITE=0` hides them all.

## Build the bundle

You need the compiled `th` binary and Node.js 18+ (for the `npx @anthropic-ai/mcpb`
bundler).

```bash
# get th if you don't have it
pnpm install:th                     # from a smooth checkout, or:
brew install SmooAI/tools/th

# build smooth.mcpb (defaults: ~/.cargo/bin/th → ./smooth.mcpb)
./build-mcpb.sh

# or point at a specific binary / output
./build-mcpb.sh /path/to/th ./smooth.mcpb
```

The script stages `th` at `server/th` beside `manifest.json`, then runs
`mcpb pack`. On Windows the manifest's `platform_overrides` points the launcher
at `server/th.exe`; darwin/linux use `server/th`.

### Icon (optional)

The committed `manifest.json` ships **without** an icon so it validates with no
assets. To brand the extension, drop a **512×512 `icon.png`** into this directory
and re-run `./build-mcpb.sh` — it copies the file into the bundle and wires the
manifest `icon` key automatically. (No icon is committed here on purpose; add the
real Smooth `th` mark.)

## Install in Claude Desktop

Double-click the produced `smooth.mcpb`. Claude Desktop shows the extension's
name, tools, and permissions, then installs it. That's it — no JSON editing.

## Manual config for other MCP clients

The bundle is just a convenience wrapper. Any MCP client can run the server
directly with `command: th, args: [mcp, serve]` (assuming `th` is on `PATH`).

**Cursor / Windsurf** — `~/.cursor/mcp.json` (or Windsurf's equivalent):

```json
{
    "mcpServers": {
        "smooth": {
            "command": "th",
            "args": ["mcp", "serve"]
        }
    }
}
```

**VS Code** — note VS Code uses `"servers"`, not `"mcpServers"` (`.vscode/mcp.json`
or user `settings.json` under `"mcp"`):

```json
{
    "servers": {
        "smooth": {
            "command": "th",
            "args": ["mcp", "serve"]
        }
    }
}
```

For all of these, run `smoo auth login` once to unlock the org tools.
