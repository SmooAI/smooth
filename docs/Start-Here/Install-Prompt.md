# Install Prompt

#start-here

> [!info] Paste this into your coding agent
> The fastest way to set up Smooth is to let the agent you already use do it. Paste the prompt below into Claude Code, Codex or OpenCode. It installs (or updates) `th`, wires `th` into all three harnesses, checks the result and reports back.

`th install-prompt` prints the same text, and `th install-prompt --copy` puts it on your clipboard, which is handy when you set up a second machine. Both read the block below, so this page is the single source.

## The prompt

```text
Install or update th (Smooth, by Smoo AI) to the latest release on this machine and wire it into Claude Code, OpenCode and Codex.

1. Run `brew update && (brew upgrade smooai/tools/th || brew install smooai/tools/th)`. Then run `th --version`. If an older th wins on PATH (check `which -a th`), fix PATH so the Homebrew copy wins.
2. Run `th harness enable claude-code`, `th harness enable opencode` and `th harness enable codex`. They are idempotent: each sets up the plugin, the MCP server, hooks and skills, and for Claude Code it also moves the statusline to the newest plugin.
3. Verify:
   - `th harness status` shows all three harnesses.
   - `th harness budget` prints: suggest /th-clear 150k · warn 270k · auto-compact 300k.
   - ~/.claude/settings.json has "autoCompactWindow": 300000, unless I set my own value.
   - ~/.codex/config.toml has model_auto_compact_token_limit = 300000.
   - If ~/.claude/settings.json has no statusLine entry, run `th doctor --setup-statusline`.
4. Choose a model: run `th model login`. Pick smooai-gateway for the Smoo AI Family plan, or enter my own OpenAI or Anthropic key.
5. Report what changed and anything that failed, with the exact errors. Then tell me to restart running sessions (or run /reload-plugins in Claude Code).
```

## What it does

| Step | Why                                                                                                                                                                                        |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1    | `th` ships through the [SmooAI/homebrew-tools](https://github.com/SmooAI/homebrew-tools) tap. An older copy earlier on `PATH` is the most common install problem.                          |
| 2    | `th harness enable` is the install **and** the update command. It writes only what it owns, and `th harness disable` removes it again.                                                     |
| 3    | The context budget is the part people notice: sessions auto-compact at 300k tokens instead of running to the model's limit. See [[Start-Here/Quickstart#4. Enable your coding harnesses]]. |
| 4    | Big Smooth needs a model. See [[Start-Here/Quickstart#2. Choose your model]] for the Family plan and bring-your-own-key options.                                                           |
| 5    | Running sessions load hooks and plugins at start, so they need a restart to pick the changes up.                                                                                           |

No Homebrew? Use `curl -fsSL https://raw.githubusercontent.com/SmooAI/smooth/main/install.sh | sh` for step 1 and keep the rest.

## Related

- [[Start-Here/Quickstart]]
- [[Start-Here/What-Is-Smooth]]
- [[Engineering/Using-th-CLI]]
