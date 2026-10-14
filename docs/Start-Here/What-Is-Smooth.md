# What Is Smooth

#start-here

> [!arch] One sentence
> Smooth is your always-on AI assistant (Big Smooth) and the CLI that runs your coding agents (`th`), shipped as one Rust binary that runs on your machine.

New here? [[Start-Here/Quickstart]] gets you running, or paste the [[Start-Here/Install-Prompt|install prompt]] into your coding agent.

## Two halves, one binary

| Half                       | What it is                                                                                                                                                                                     |
| -------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Big Smooth**             | A personal assistant that stays running on your Mac or Linux box. It remembers things, works with your calendar, reminders and messages, searches the web, and notifies your devices.          |
| **The `th` agent toolbox** | What `th harness enable` adds to Claude Code, Codex, OpenCode and Cursor: pearls work tracking, agent mail between sessions, handoffs, the context budget, and SmoothFlow for running a fleet. |

The same binary is also `smoo`, the CLI for the [Smoo AI](https://smoo.ai) platform (CRM, config, agents, analytics).

## What `th up` does

1. Starts **Big Smooth** (`smooth-daemon`) as a host process and daemonises it (`--foreground` to stay attached; `th service install` to survive reboots).
2. Binds `127.0.0.1:4400`. The chat web UI and the WebSocket API live there; `th code` is the terminal client.
3. Hosts the `smooth-operator` engine in-process. Each message runs as an agent turn with tools: files and shell in your workspace, web search, memory, and on macOS Calendar, Reminders, Messages, Contacts and Location.

There is no VM, no container and no image pull. See [[Architecture/Daemon-Modules-and-Security]] and [[Operations/Running-Locally]].

## Models

Big Smooth needs a model, and you choose where it comes from:

- **Family plan**: $9/month with a 14-day free trial; hosted models through the Smoo AI gateway with one key. `th model login smooai-gateway`. See [smoo.ai/big-smooth#plan](https://smoo.ai/big-smooth#plan).
- **Bring your own key**: any OpenAI- or Anthropic-compatible endpoint. `th model login <provider>` or `th providers add <id> --url … --format openai|anthropic`.

## Local-first

Everything runs on your machine: conversations, pearls, agent mail and settings live under `~/.smooth/`. The cloud is optional. Signing in to Smoo AI (`smoo auth login`) adds memory that syncs across devices and push notifications to your phone, and the Family plan adds hosted models.

## Security posture

Big Smooth acts as you, on your machine. Three layers sit between the model and your system ([[Architecture/Security-Model]]):

1. A permission gate with deny rules, installed first. Tools that can't be undone (such as cancelling a calendar event) ask before they run.
2. **Narc**: secret, prompt-injection and dangerous-shell detectors on every tool call, plus secret redaction.
3. An opt-in kernel sandbox for shell commands (macOS only; `th settings set sandbox.enabled true`).

Run it where you'd run any trusted coding agent. It is single-tenant: one owner per instance.

## What it isn't

- Not a hosted service. The optional cloud pieces add sync and models; they don't run your agent.
- Not a container platform. No Docker required.
- Not multi-tenant. Family sharing is on the [[Start-Here/Roadmap]].

## Related

- [[Home]]
- [[Start-Here/Quickstart]]
- [[Start-Here/Roadmap]]
- [[Start-Here/Glossary]]
- [[Architecture/Architecture-Overview]]
