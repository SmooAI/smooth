---
name: switch-operator-engine
description: Boot any of the 5 polyglot smooth-operator LocalServer implementations (rust / go / ts / python / dotnet) via `th operator serve --lang <x>` so Big Smooth can dogfood each engine over the shared WS protocol. Use when the user asks to "switch operator engines", "run the go/ts/python/dotnet server", "dogfood the polyglot operator", "compare engines", or when you need a LocalServer of a specific language up to drive with the bench harness.
---

# switch-operator-engine — dogfood each polyglot smooth-operator server

The smooth-operator LocalServer has five implementations that all speak the same
WS protocol: **rust, go, ts, python, dotnet**. `th operator serve` boots any one
of them behind a uniform env contract, so Big Smooth (and the bench harness) can
drive each engine identically and compare behavior. Pearl th-3f46fd.

The servers live in the sibling **`smooth-operator`** repo (default
`~/dev/smooai/smooth-operator`; override with `SMOOTH_OPERATOR_REPO`). This is a
dev/dogfooding tool — it spawns a server process in the foreground and blocks.

## The command

```bash
th operator serve --lang <rust|go|ts|python|dotnet> [--port <n>]
th operator serve --help          # per-engine caveats, self-documenting
```

- **Default port** is `8799`.
- Every engine inherits the shared env contract from your shell:
  `SMOOAI_GATEWAY_URL`, `SMOOAI_GATEWAY_KEY`, `SMOOTH_PERSONA`, and
  `SMOOAI_MODEL` (defaults to `deepseek-v4-flash`). No gateway key → it still
  boots and turns errors cleanly.
- The launcher sets the correct per-engine bind var from `--port` for you.

## Per-engine notes (the ones that bite)

| `--lang` | What runs                                 | Caveat                                                                                                                           |
| -------- | ----------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `rust`   | `th daemon`                               | The only runnable Rust LocalServer — it's the daemon, so it carries the daemon's narc/storage/persona extras, not a bare engine. |
| `go`     | `go run ./cmd/serve`                      | Needs Go toolchain.                                                                                                              |
| `ts`     | `node dist/main.js`                       | Auto-builds (`pnpm install && pnpm build`) on first run if `dist/main.js` is missing.                                            |
| `python` | `uv run python -m smooth_operator_server` | Bind is **hardcoded 127.0.0.1:8787 upstream** — `--port` is ignored (the CLI warns you). Runs `uv sync` first.                   |
| `dotnet` | `dotnet run`                              | Needs .NET SDK.                                                                                                                  |

## How to run one (it blocks)

`th operator serve` runs in the foreground. To keep working while it serves,
launch it in the background / a separate pane and drive it over WS on the bound
port, e.g.:

```bash
# boot the Go engine on a spare port, in the background
th operator serve --lang go --port 8801 &
# ...drive ws://127.0.0.1:8801 with the bench harness / a WS client...
# stop it when done
kill %1
```

For a build-and-boot smoke of **all five** at once (each on its own port, PASS =
the port accepts TCP), the repo's `scripts/operator-serve.sh smoke` still does
that — `th operator serve` is the single-engine, discoverable front door.

## When NOT to use this

- To run the production/daemon Big Smooth for real work, use `th up` / `th
daemon` directly — this skill is for _comparing engines_, not daily driving.
- If the sibling `smooth-operator` repo isn't checked out, `th operator serve`
  errors with the expected path — clone it or set `SMOOTH_OPERATOR_REPO`.
