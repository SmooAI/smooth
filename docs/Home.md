---
cssclasses:
    - home-page
---

<p align="center">
  <a href="https://smoo.ai"><img src="https://smoo.ai/images/logo/logo.svg" alt="Smoo AI" width="200" /></a>
</p>

# Smooth Documentation

#moc

> [!arch] About Smooth
> Your always-on AI assistant (Big Smooth) and the CLI that runs your coding agents (`th`), in one Rust binary. `th up` starts Big Smooth on your machine, with Narc surveillance on every tool call. No Docker. No nested virtualization. Local-first, with an optional cloud: sign in for synced memory and phone notifications, or take the Family plan for hosted models.

Smooth is part of the [Smoo AI](https://smoo.ai) platform — AI built into every product. This vault is the canonical source of truth for Smooth's architecture, operations, and decisions. New? Start with [[Start-Here/Quickstart]] (or paste the [[Start-Here/Install-Prompt|install prompt]] into your coding agent), then [[Start-Here/What-Is-Smooth]] and the map below.

---

## The picture

```
   th up                         th harness enable all
     │                                   │
     ▼                                   ▼
┌──────────────────────────────┐   ┌──────────────────────────────┐
│ Big Smooth  (127.0.0.1:4400) │   │ Claude Code · Codex ·        │
│ smooth-operator engine,      │◀──│ OpenCode · Cursor            │
│ in-process · tools · memory  │   │ + th MCP server, hooks,      │
│ permission gate → Narc       │   │   skills, context budget     │
└──────────────┬───────────────┘   └──────────────────────────────┘
               │
   web chat · th code · SmoothFlow app · phone (relay)
```

One process on your host, no per-task VM or worker. The microVM sandbox stack was removed in July 2026 ([[Decisions/ADR-004-remove-microvm-sandbox-stack]]); [[Architecture/Daemon-Modules-and-Security]] is the current module map.

---

## Start here

| Page                           | Description                                                     |
| ------------------------------ | --------------------------------------------------------------- |
| [[Start-Here/Quickstart]]      | Install, choose a model, first chat, enable your harnesses      |
| [[Start-Here/Install-Prompt]]  | The prompt that sets `th` up from inside your coding agent      |
| [[Start-Here/What-Is-Smooth]]  | One-pager. Big Smooth, the toolbox, models, the security model  |
| [[Start-Here/Roadmap]]         | Coming soon, and the personal-assistant gaps we're closing next |
| [[Start-Here/Glossary]]        | Cast roles, work model, terms                                   |
| [[Operations/Running-Locally]] | `th up`, `th down`, `th code`                                   |

---

## Architecture

| Page                                         | Description                                                   |
| -------------------------------------------- | ------------------------------------------------------------- |
| [[Architecture/Architecture-Overview]]       | Top-level diagram + control flow                              |
| [[Architecture/The-Cast]]                    | Big Smooth, Operative, Engine, Narc, Scribe, Archivist, Diver |
| [[Architecture/Dispatch]]                    | How a task flows from chat to an operative and back           |
| [[Architecture/Operatives]]                  | The agent runtime, the operative binary, tool surface         |
| [[Architecture/Security-Model]]              | Narc surveillance today; auto-mode + kernel sandbox planned   |
| [[Architecture/Pearls]]                      | The work-item tracker (one SQLite db, all projects)           |
| [[Architecture/Data-Storage]]                | pearls.db, ~/.smooth, audit                                   |
| [[Architecture/Extension-System]]            | SEP — the planned extension protocol                          |
| [[Architecture/Daemon-Direction]]            | Where Big Smooth is headed (epic `th-c89c2a`)                 |
| [[Architecture/Workspace-Crates]]            | What each workspace crate owns                                |
| [[Architecture/Daemon-Modules-and-Security]] | smooth-daemon modules, dispatch, security layers              |
| [[Architecture/Data-Layout]]                 | On-disk layout + pearl/Jira reference                         |
| [[Architecture/Onboarding]]                  | First-run model setup: Family plan or your own key            |

---

## Engineering

| Page                                 | Description                                            |
| ------------------------------------ | ------------------------------------------------------ |
| [[Engineering/Build-Workflow]]       | `cargo`, `pnpm install:th`                             |
| [[Engineering/Bench-Harness]]        | `th bench`, scoring, The Line                          |
| [[Engineering/th-CLI-Daily-Driver]]  | `th`/`smoo` daily reference, adding a subcommand       |
| [[Engineering/Chrome-Extension]]     | `th chrome`: synced bookmarks, history, tab control    |
| [[Engineering/Dev-Loop-and-Landing]] | Build/test/style, oxfmt, changesets, landing checklist |

---

## Operations

| Page                           | Description                                  |
| ------------------------------ | -------------------------------------------- |
| [[Operations/Running-Locally]] | Quickstart, common knobs                     |
| [[Operations/Troubleshooting]] | Known traps, runner missing, port collisions |

---

## Decisions

- [[Decisions/ADR-Index]] — Architecture Decision Records. Note: ADR-001/002/003 record the microVM consolidation, superseded by the July 2026 teardown (`th-f4a801`, [[Decisions/ADR-004-remove-microvm-sandbox-stack]]) — kept as history.

---

## Conventions

- Cast roles are linked by canonical name: [[Architecture/The-Cast#Big-Smooth|Big Smooth]], [[Architecture/The-Cast#Narc|Narc]], [[Architecture/The-Cast#Scribe|Scribe]], [[Architecture/The-Cast#Archivist|Archivist]], [[Architecture/The-Cast#Diver|Diver]].
- ASCII diagrams over Mermaid (renders identically in Obsidian, GitHub, and editor preview).
- Each page opens with a tagline + a `[!arch]` or `[!info]` callout. Bullets over paragraphs.

---

## Related

- [[Start-Here/What-Is-Smooth]]
- [[Architecture/Architecture-Overview]]
- [[_meta/How-to-Update-These-Docs]]

---

<p align="center">
  Built by <a href="https://smoo.ai"><strong>Smoo AI</strong></a> — AI built into every product.
</p>
