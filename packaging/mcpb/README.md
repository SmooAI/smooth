# Smooth `.mcpb` Desktop Extension

One-click-install the Smooth MCP server into Claude Desktop. The bundle wraps the
compiled `th` binary and runs `th mcp serve` — a stdio MCP server that exposes
`th`'s work-tracking (pearls) as MCP tools. No env config: the server reads
`~/.smooth/` for auth and pearl state.

## Tools

| Tool | What it does |
| --- | --- |
| `pearls_ready` | List work items ready to work on now — open, unblocked, highest priority first. |
| `pearls_create` | Create a new work item (pearl); returns the new pearl id. |

These act on the pearl store in the **workspace the server is launched in**. The
org tools — the Smooth Operator agent, CRM, knowledge, and analytics — unlock
after you run `th auth login` (`th` caches a JWT under `~/.smooth/auth/`).

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

For all of these, run `th auth login` once to unlock the org tools.
