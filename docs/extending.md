# Extending Smooth

Smooth ships with a fixed set of built-in tools (read, write, edit,
bash, grep, list, lsp, http_fetch, background processes, pearl
management, remote delegation, port forwarding). Two extension points
let you add more without modifying the binary:

1. **MCP servers** — spawn stdio Model Context Protocol servers
   (Playwright, GitHub, filesystem, database, whatever) and expose
   their tools to the agent as `<server>.<tool>`.
2. **CLI-wrapper plugins** — drop a TOML manifest describing a shell
   command and the runner registers it as a tool named
   `plugin.<name>`. No protocol, no separate process — "render this
   command template and pipe stdout to the agent."

Use MCP when the tool is stateful or ships a typed protocol. Use a
plugin for one-shot CLI wrappers.

All tools — built-ins, MCP, plugins — pass through the same Narc
surveillance path. CliGuard blocks dangerous shell patterns,
detectors screen for prompt injection and secret exfiltration, and
the LLM judge makes an independent call on tool inputs that look
suspicious. The microVM boundary contains anything the agent does
either way.

---

## Configuration scopes

Both MCP servers and plugins are resolved from two locations:

| Scope | Path | Use case |
|---|---|---|
| Global | `$SMOOTH_HOME/` (else `~/.smooth/`) | Personal tools that follow you across projects (your IDE, your credentials) |
| Project | `<repo>/.smooth/` | Tools specific to this repo (a DB MCP pointed at this project, a deploy helper, a team-wide Playwright config) |

On a **name collision, the project entry wins** and the global one
is shadowed. This is the same resolution order used by most tools
(editorconfig, direnv, .gitignore), and it means you can commit
`.smooth/mcp.toml` to a repo to share tools with teammates without
overriding their global preferences.

`th mcp list` and `th plugin list` show entries from both scopes with
`[global]`, `[project]`, and `[shadowed by project]` tags.

---

## MCP servers

MCP is the Model Context Protocol — a JSON-RPC-over-stdio protocol
for AI tool use. Smooth uses the official [rmcp Rust
SDK](https://crates.io/crates/rmcp) as a client. Any tool that
speaks MCP over stdio works.

### Add a server

```bash
# Playwright browser automation (global)
th mcp add playwright npx @playwright/mcp@latest

# GitHub server with a token from env (global)
th mcp add -e GITHUB_PERSONAL_ACCESS_TOKEN='${env:GITHUB_TOKEN}' github \
    docker run -i --rm ghcr.io/github/github-mcp-server

# A project-specific filesystem server scoped to this repo
th mcp add --project repo-fs npx @modelcontextprotocol/server-filesystem /workspace

# Register but don't start yet
th mcp add --disabled experimental-server /opt/bin/some-mcp-server
```

Flags must come **before positional arguments** (`name command args...`)
because args use clap's `trailing_var_arg`. Hyphenated args like
`docker run -i --rm ghcr.io/...` work because of that.

### Per-server env and secrets

Keep credentials out of the config file — reference them from the
runner's environment using `${env:VAR}`:

```toml
[[servers]]
name = "github"
command = "docker"
args = ["run", "-i", "--rm", "ghcr.io/github/github-mcp-server"]

[servers.env]
GITHUB_PERSONAL_ACCESS_TOKEN = "${env:GITHUB_TOKEN}"
```

The runner resolves `${env:GITHUB_TOKEN}` at spawn time; unset
variables expand to empty strings. Nothing sensitive lives in the
TOML itself, so it's safe to commit the project config.

### List, test, remove

```bash
th mcp list                        # Both scopes, shadowing marked
th mcp test playwright             # Spawn the command, 1s health probe
th mcp remove playwright           # Searches project first, then global
th mcp remove --project repo-fs    # Project only
th mcp path                        # Print global config path
th mcp path --project              # Print project config path
```

`test` spawns the configured command and watches for it to exit
within 1 second. A healthy stdio MCP server stays alive waiting for
JSON-RPC; an early exit (or a binary that isn't on PATH) is
reported with stderr. `test` does **not** do the full MCP
handshake — the runner does that on `th up`, and you'll see each
server's tool count in the startup logs.

### Tool naming

Server tools land in the agent's registry as `<server>.<tool>`:

```
playwright.browser_navigate
playwright.browser_snapshot
github.create_issue
github.list_pull_requests
repo-fs.read_file
```

This keeps MCP tools visually distinct from built-ins
(`read_file` is native; `repo-fs.read_file` is MCP) and prevents
name collisions across servers.

---

## CLI-wrapper plugins

A plugin is a TOML manifest that turns a shell command into a tool
the agent can call. Scaffold one with `th plugin init`:

```bash
# Global plugin that jq-filters JSON
th plugin init jq \
    --command 'jq {{filter}} <<< {{json}}' \
    --description 'Run a jq filter over JSON input.'

# Project-only deploy helper
th plugin init --project deploy \
    --command 'scripts/deploy.sh {{env}}' \
    --description 'Deploy this project to the given environment.'
```

`init` extracts `{{placeholder}}` names from the command and seeds
the JSON Schema's `required` list and per-property stubs so the
generated manifest is callable out of the box:

```toml
name = "jq"
description = "Run a jq filter over JSON input."
prompt_hint = ""
command = "jq {{filter}} <<< {{json}}"

[env]

[parameters]
type = "object"
required = ["filter", "json"]

[parameters.properties.filter]
type = "string"
description = "TODO: describe `filter` for the LLM."

[parameters.properties.json]
type = "string"
description = "TODO: describe `json` for the LLM."
```

Edit the manifest — especially those TODO descriptions, since the
LLM reads them when deciding when to call the tool — and the runner
will load it on the next `th up`.

### Placeholder substitution

`{{key}}` placeholders in `command` are substituted from the agent's
tool args. String values are inserted raw (no quoting); non-strings
are JSON-stringified. Missing keys expand to empty strings. Values
containing literal `{{x}}` can't trigger recursion — substitution
is single-pass.

The rendered command runs via `bash -lc`, so you get shell features
(pipes, here-docs, variable expansion) at the cost of shell-quoting
concerns for your placeholder values. For anything with complex
arguments or stateful sessions, prefer MCP.

### `prompt_hint` vs `description`

The agent sees the concatenation of `description` + `prompt_hint`
as the tool's docstring. Use `description` for *what it does* and
`prompt_hint` for *when to reach for it*:

```toml
description = "Render a Mermaid diagram to a PNG."
prompt_hint = "Use this when the user asks for a diagram; prefer it over writing SVG by hand."
```

### List, path, remove

```bash
th plugin list                   # Both scopes, shadowing marked
th plugin path jq                # Print the manifest path
th plugin path --project deploy  # Project-scoped path
th plugin remove jq              # Searches project first, then global
th plugin remove --project jq    # Project only
```

---

## Security model

> *Users do their thing on their machines. The framework is secure
> forward, but doesn't stop them from doing what they want.*

Loading MCP configs and plugins from disk is deliberately
frictionless — no trust prompts, no sandboxing at install time. It's
the same trust model as `npm install`, `.zshrc`, or cloning a repo
and running `pnpm dev`: the user decides what code to install.

The defensive layers kick in at **call time**, not install time:

1. **CliGuard** (Narc) — regex-based ban on dangerous shell patterns
   (`rm -rf /`, `curl ... | sh`, `:(){:|:&};:`, etc.) applied to
   every bash invocation, including the ones a plugin's command
   renders.
2. **Prompt-injection detectors** (Narc) — scan tool inputs for
   system-prompt overrides, role redefinitions, and obvious
   exfiltration patterns.
3. **Secret detectors** (Narc) — block tool inputs and outputs that
   look like AWS keys, GitHub tokens, private keys, etc.
4. **LLM judge** (Narc) — for borderline cases, an independent
   model reviews the tool call and can escalate.
5. **Goalie + Wonk** — every network request and filesystem write
   passes through the Goalie proxy, gated by Wonk policy. An
   MCP server that tries to `curl evil.com` hits the same wall as
   a native tool would.
6. **Microsandbox** — the whole agent loop runs in a hardware-
   isolated microVM. Whatever the agent's tools do, they do it to
   the sandbox, not your host.

A malicious `plugin.toml` committed to a repo can't escape this
chain any more than a malicious `npm` package can. If a Narc layer
would reject a call from a built-in tool, it'll reject the same
call from an MCP server or plugin.

### What's *not* defended against

- Running `th` in a repo whose `.smooth/` contains a plugin with a
  command you wouldn't otherwise run. The sandbox contains it, but
  it still runs. If that matters for your workflow, review
  `.smooth/` before the first `th up` in a new repo.
- MCP servers that read your files legitimately (filesystem server
  scoped to `/workspace`) and then exfiltrate on the same tool call
  that was expected to be benign. Goalie + Wonk narrow this to
  explicitly-allowed destinations per Wonk policy.

See `SECURITY.md` for the full threat model.
