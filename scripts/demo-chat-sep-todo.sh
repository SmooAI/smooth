#!/usr/bin/env bash
# Manual acceptance demo for pearl th-6d8606: Big Smooth's own chat loop hosts
# SEP extensions. Proves the `todo` demo extension fires from the chat loop, its
# widget renders in smooth-web via the in-process UiRelay, and it survives
# `th ext reload` — the three acceptance criteria that can't run in CI (they need
# a live browser and a running daemon).
#
# The deterministic setup (install, trust, start daemon, poke the HTTP surface)
# is scripted. The browser-observation steps are printed for you to eyeball —
# there is no reliable headless way to assert "the widget rendered in the chat
# pane", so this stays a runbook, not a Playwright test.
#
# Usage:  scripts/demo-chat-sep-todo.sh
# Env:    SMOOTH_BIGSMOOTH_URL (default http://127.0.0.1:4400)
#         TODO_EXT_DIR (default ~/dev/smooai/smooth-operator/typescript/extension-sdk/examples)
set -euo pipefail

URL="${SMOOTH_BIGSMOOTH_URL:-http://127.0.0.1:4400}"
TODO_SRC="${TODO_EXT_DIR:-$HOME/dev/smooai/smooth-operator/typescript/extension-sdk/examples}/todo.ts"
EXT_HOME="$HOME/.smooth/extensions/todo"

step() { printf '\n\033[1;36m▶ %s\033[0m\n' "$1"; }
manual() { printf '   \033[1;33m[eyeball]\033[0m %s\n' "$1"; }

step "1. Install the todo demo extension (pre-trusted — the daemon never prompts)"
if [[ ! -f "$TODO_SRC" ]]; then
    echo "   todo.ts not found at $TODO_SRC — set TODO_EXT_DIR to the extension-sdk/examples dir." >&2
    exit 1
fi
mkdir -p "$EXT_HOME"
cat > "$EXT_HOME/extension.toml" <<EOF
name = "todo"
version = "0.1.0"
protocol = 1

[run]
command = "tsx"
args = ["$TODO_SRC"]

[capabilities]
tools = true
commands = true
ui = true
EOF
th ext trust todo || true   # pre-trust so the headless daemon loads it
echo "   installed + trusted at $EXT_HOME"

step "2. Start (or restart) the Big Smooth daemon so it loads the host at startup"
echo "   Run in another shell:  th up   (or restart if already running — new"
echo "   installs need a daemon start; reload only refreshes already-loaded exts)"
read -r -p "   Press enter once the daemon is up at $URL ... " _

step "3. Confirm the daemon loaded todo + its commands"
curl -fsS "$URL/api/ext" | (jq . 2>/dev/null || cat)
manual "the 'todo' extension and its add/done/clear tools should be listed"

step "4. Drive it from the chat loop in smooth-web"
echo "   Open smooth-web, start a chat with Big Smooth, and send:"
echo "       add buy milk to my todos"
manual "the LLM calls todo.add; a Todos widget renders in the chat pane (UiRelay set_widget)"
echo "   Then send:   mark todo 1 done"
manual "the widget updates in place — ✓ buy milk"
echo "   Then send:   /add walk the dog     (extension slash command, bypasses the LLM)"
manual "the item is added directly — no model turn"

step "5. Prove it survives a hot reload"
curl -fsS -X POST "$URL/api/ext/reload" -H 'content-type: application/json' -d '{"name":"todo"}'
echo
manual "reload returns ok; the NEXT chat turn still resolves todo.* tools (fresh subprocess, epoch-fenced)"

step "Done. All three acceptance criteria observed: fires from chat, renders via UiRelay, survives reload."
