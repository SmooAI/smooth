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
> A single Rust binary (`th`) that runs an AI agent stack on your machine. `th up` starts Big Smooth — an orchestrator + API on the host — which dispatches operatives to do real work, with Narc surveillance on the tool surface. No Docker. No nested virtualization. No cloud.

Smooth is part of the [Smoo AI](https://smoo.ai) platform — AI built into every product. This vault is the canonical source of truth for Smooth's architecture, operations, and decisions. Start with [[Start-Here/What-Is-Smooth]], then follow the map below.

---

## The picture

```
                        th up
                          │
                          ▼
        ┌─────────────────────────────────────┐
        │  Big Smooth  (host process, :4400)   │
        │  API · pearl store · dispatch        │
        └──────────────────┬──────────────────┘
                           │ spawn subprocess per task
                           ▼
        ┌─────────────────────────────────────┐
        │  smooth-operative  (agent loop)      │
        │  tools + Narc surveillance           │
        └─────────────────────────────────────┘
```

One process, one operative per task. The microVM sandbox stack was removed in July 2026 (pearl `th-f4a801`; see [[Decisions/ADR-004-remove-microvm-sandbox-stack]] for what was lost and why); the [[Architecture/Daemon-Direction|daemon direction]] is where the always-on personal-agent story continues.

---

## Start here

| Page                                              | Description                                                 |
| ------------------------------------------------- | ----------------------------------------------------------- |
| [[Start-Here/What-Is-Smooth]]                     | One-pager. What `th up` boots, what gets dispatched, the why |
| [[Start-Here/Glossary]]                           | Cast roles, work model, terms                               |
| [[Operations/Running-Locally]]                    | `th up`, `th down`, `th code`                               |

---

## Architecture

| Page                                              | Description                                                  |
| ------------------------------------------------- | ------------------------------------------------------------ |
| [[Architecture/Architecture-Overview]]            | Top-level diagram + control flow                             |
| [[Architecture/The-Cast]]                         | Big Smooth, Operative, Engine, Narc, Scribe, Archivist, Diver |
| [[Architecture/Dispatch]]                         | How a task flows from chat to an operative and back          |
| [[Architecture/Operatives]]                       | The agent runtime, the operative binary, tool surface        |
| [[Architecture/Security-Model]]                   | Narc surveillance today; auto-mode + kernel sandbox planned  |
| [[Architecture/Pearls]]                           | The work-item tracker (Dolt-backed)                          |
| [[Architecture/Data-Storage]]                     | Dolt, smooth-dolt, sessions, audit                           |
| [[Architecture/Extension-System]]                 | SEP — the planned extension protocol                         |
| [[Architecture/Daemon-Direction]]                 | Where Big Smooth is headed (epic `th-c89c2a`)                |

---

## Engineering

| Page                                              | Description                                  |
| ------------------------------------------------- | -------------------------------------------- |
| [[Engineering/Build-Workflow]]                    | `cargo`, `pnpm install:th`                   |
| [[Engineering/Bench-Harness]]                     | `th bench`, scoring, The Line                |

---

## Operations

| Page                                              | Description                                                 |
| ------------------------------------------------- | ----------------------------------------------------------- |
| [[Operations/Running-Locally]]                    | Quickstart, common knobs                                    |
| [[Operations/Troubleshooting]]                    | Known traps, runner missing, port collisions                |

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
