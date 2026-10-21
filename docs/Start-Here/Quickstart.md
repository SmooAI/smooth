# Quickstart

#start-here

> [!info] Ten minutes from nothing to a working setup
> Install `th`, choose a model, say hello to Big Smooth, then wire `th` into the coding agents you already use. If you'd rather have your coding agent do all of this, paste the [[Start-Here/Install-Prompt|install prompt]] into it instead.

## 1. Install

```bash
brew install SmooAI/tools/th      # macOS (Apple Silicon) and Linux
th --version
```

No Homebrew? `curl -fsSL https://raw.githubusercontent.com/SmooAI/smooth/main/install.sh | sh`. On Windows, use WSL for now.

If `th --version` prints an older version than you expect, another copy is earlier on your `PATH`. `which -a th` lists all of them.

## 2. Choose your model

Big Smooth and `th code` need a model. You have two options.

### Family plan (hosted models)

$9/month with a 14-day free trial. One key reaches the hosted models through the Smoo AI gateway, so there is nothing to set up per provider. Top up $5 or $10 of AI credits anytime during the trial, with optional auto-reload (`smoo credits buy`). Details: [smoo.ai/big-smooth#plan](https://smoo.ai/big-smooth#plan).

```bash
th model login smooai-gateway
```

### Bring your own key

Use any OpenAI- or Anthropic-compatible endpoint you already pay for.

```bash
th model login anthropic          # or openai, openrouter, google, kimi, ollama, …
th model login                    # no argument: pick from the list

# Any other OpenAI- or Anthropic-compatible server, by URL:
th providers add mylab --url https://llm.example.com/v1 --api-key sk-… --format openai --model my-model
th providers add myproxy --url https://proxy.example.com --api-key … --format anthropic --model my-model
```

`th model status` shows what is configured. The first provider you add with `th providers add` gets every routing slot; `th routing` moves slots later.

## 3. Say hello to Big Smooth

```bash
th up        # start Big Smooth on this machine
```

Then open the chat at <http://localhost:4400> (`th web` prints the address), or stay in the terminal with `th code`. On a Mac you can also use the desktop app (beta): [download Big Smooth for Mac](https://downloads.smoo.ai/bigsmooth/latest-arm64.dmg) (Apple Silicon).

Ask it something that needs a tool, such as "what's on my calendar tomorrow?", "remind me to call Mom at 6" or "what's the weather this weekend?". On macOS, the Calendar, Reminders and Messages tools each need a one-time permission grant:

```bash
th doctor --setup-calendar
th doctor --setup-reminders
th doctor --setup-imessage
```

To keep Big Smooth running across reboots: `th service install`.

## 4. Enable your coding harnesses

```bash
th harness enable all             # or: claude-code | codex | opencode | cursor
th harness status
```

`enable` is idempotent and doubles as the update command. Each harness gets:

- **The `th` MCP server**: pearls, agent mail, SmoothFlow and your Smoo AI org as tools.
- **The smooth-agent plugin or skills**: `/th-handoff`, `/th-clear`, pearls-flow, agent-comms.
- **The context budget.** Sessions auto-compact at 300k tokens instead of running to the model's limit, the agent suggests `/th-clear` at milestones past 150k, and the Claude Code statusline shows a context meter.

```console
$ th harness budget
suggest /th-clear 150k · warn 270k · auto-compact 300k · claude-code · …
```

`th harness budget --report` shows where your own Claude Code input tokens went over the last 30 days, and an upper bound on what the budget would have saved.

Restart running sessions afterwards (or `/reload-plugins` in Claude Code).

## Next

- [[Start-Here/What-Is-Smooth]]: the tour.
- [[Start-Here/Roadmap]]: what's coming next.
- [[Engineering/Using-th-CLI]]: the full command reference.

## Related

- [[Home]]
- [[Start-Here/Install-Prompt]]
- [[Operations/Running-Locally]]
