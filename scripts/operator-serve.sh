#!/usr/bin/env bash
# operator-serve.sh — boot any of the 5 polyglot smooth-operator LocalServer
# implementations behind a uniform env contract, so Big Smooth's control surface
# (and the bench harness) can drive each engine over the shared WS protocol.
#
# Pearl th-3f46fd. Dev dogfooding tool — NOT shipped in `th`. The servers live in
# the sibling smooth-operator repo; override with SMOOTH_OPERATOR_REPO.
#
#   scripts/operator-serve.sh <rust|go|ts|python|dotnet> [port]
#   scripts/operator-serve.sh smoke                 # build+boot all 5, report which listen
#
# Uniform env contract (all engines read these; the launcher passes them through):
#   SMOOAI_GATEWAY_URL / SMOOAI_GATEWAY_KEY  LLM gateway (no key → boots, turns error cleanly)
#   SMOOTH_PERSONA                           system prompt (optional)
#   SMOOAI_MODEL                             agent model (default deepseek-v4-flash here)
# Per-engine bind var (the launcher sets the right one from [port]):
#   rust=SMOOTH_ADDR  go=SMOOTH_OPERATOR_BIND  ts=SMOOTH_OPERATOR_HOST/PORT  dotnet=ASPNETCORE_URLS
#   python: bind is hardcoded 127.0.0.1:8787 upstream — [port] is ignored (caveat).
set -euo pipefail

REPO="${SMOOTH_OPERATOR_REPO:-$HOME/dev/smooai/smooth-operator}"
: "${SMOOAI_MODEL:=deepseek-v4-flash}"
export SMOOAI_MODEL
# Pass gateway/persona through only if already set — don't clobber with empties.
for v in SMOOAI_GATEWAY_URL SMOOAI_GATEWAY_KEY SMOOTH_PERSONA; do
    [ -n "${!v:-}" ] && export "$v"
done

serve() {
    local lang="$1" port="${2:-8799}"
    case "$lang" in
    rust)
        # The one runnable Rust LocalServer is the daemon itself (carries the
        # daemon's narc/storage/persona extras — not a bare engine, noted).
        SMOOTH_ADDR="127.0.0.1:$port" exec th daemon
        ;;
    go)
        cd "$REPO/go/server"
        SMOOTH_OPERATOR_BIND="127.0.0.1:$port" exec go run ./cmd/serve
        ;;
    ts)
        cd "$REPO/typescript/server"
        [ -f dist/main.js ] || { pnpm install --silent && pnpm build; }
        SMOOTH_OPERATOR_HOST=127.0.0.1 SMOOTH_OPERATOR_PORT="$port" exec node dist/main.js
        ;;
    python)
        cd "$REPO/python/server"
        uv sync --quiet
        [ "$port" = "8787" ] || echo "note: python bind is hardcoded 127.0.0.1:8787 upstream; ignoring port $port" >&2
        exec uv run python -m smooth_operator_server
        ;;
    dotnet)
        cd "$REPO/dotnet/server/host"
        ASPNETCORE_URLS="http://127.0.0.1:$port" exec dotnet run
        ;;
    *)
        echo "usage: operator-serve.sh <rust|go|ts|python|dotnet> [port]" >&2
        return 2
        ;;
    esac
}

# smoke: build+boot each engine on its own port, PASS if the port accepts TCP
# within the timeout (a listening socket = the engine booted; strict-auth 401 on
# GET is still "listening"). Reports a table; never leaves a server running.
smoke() {
    local timeout="${SMOOTH_SMOKE_TIMEOUT:-300}" port=8791 rc=0
    for lang in rust go ts python dotnet; do
        [ "$lang" = python ] && port=8787
        local logf; logf="$(mktemp)"
        ( serve "$lang" "$port" >"$logf" 2>&1 ) &
        local pid=$!
        local ok=fail waited=0
        while [ "$waited" -lt "$timeout" ]; do
            kill -0 "$pid" 2>/dev/null || { ok="crash"; break; }
            if nc -z 127.0.0.1 "$port" 2>/dev/null; then ok=PASS; break; fi
            sleep 2; waited=$((waited + 2))
        done
        # Kill the server + its whole process group (go run/dotnet spawn children).
        pkill -P "$pid" 2>/dev/null || true; kill "$pid" 2>/dev/null || true
        printf '%-8s %-6s (%ss)\n' "$lang" "$ok" "$waited"
        [ "$ok" = PASS ] || { rc=1; echo "  --- last log lines ($lang) ---"; tail -5 "$logf" | sed 's/^/  /'; }
        rm -f "$logf"; port=$((port + 1))
    done
    return $rc
}

case "${1:-}" in
smoke) smoke ;;
"") echo "usage: operator-serve.sh <rust|go|ts|python|dotnet|smoke> [port]" >&2; exit 2 ;;
*) serve "$@" ;;
esac
