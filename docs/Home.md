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
> A single Rust binary (`th`) that runs an AI agent stack on your machine. Boots as a host daemon, dispatches operatives as subprocesses under Narc surveillance, and writes through a deterministic tool surface. No Docker. No VMs. No cloud.

Smooth is part of the [Smoo AI](https://smoo.ai) platform — AI built into every product. This vault is the canonical source of truth for Smooth's architecture, operations, and decisions. Start with [[Start-Here/What-Is-Smooth]], then follow the map below.

---

## The picture

```
                       th up
                         │
             ┌───────────▼────────────┐
             │      host daemon       │
             │                        │
             │  Big Smooth (API, WS,  │
             │  web UI, dispatch)     │
             │      │ exec            │
             │      ▼                 │
             │  smooth-operative      │
             │  subprocess + Narc     │
             └────────────────────────┘
```

One host process, operatives as subprocesses, Narc surveillance in-process. The microVM sandboxed mode was removed 2026-07 — see [[Decisions/ADR-004-remove-microvm-sandbox-stack]] for what was lost and why.

---

## Start here

| Page                                              | Description                                                 |
| ------------------------------------------------- | ----------------------------------------------------------- |
| [[Start-Here/What-Is-Smooth]]                     | One-pager. What `th up` boots, what gets dispatched, the why |
| [[Start-Here/Glossary]]                           | Cast roles, terms                                           |
| [[Operations/Running-Locally]]                    | `th up`, `th down`, `th code`                               |

---

## Architecture

| Page                                              | Description                                                  |
| ------------------------------------------------- | ------------------------------------------------------------ |
| [[Architecture/Architecture-Overview]]            | Top-level diagram + control flow                             |
| [[Architecture/The-Cast]]                         | Big Smooth, Narc, Scribe, Archivist, Diver, Groove (Wonk/Goalie removed — historical) |
| [[Architecture/Sandboxed-Mode]]                   | Historical — the removed microVM mode                        |
| [[Architecture/Direct-Mode]]                      | How Smooth runs now (formerly the escape hatch)              |
| [[Architecture/Transport]]                        | gRPC over UDS, in-process Arc, HTTP at the edge — the wire story |
| [[Architecture/Dispatch]]                         | How a task flows from `th up` chat to an operator and back  |
| [[Architecture/Operatives]]                        | The agent runtime, the operative binary, tool surface  |
| [[Architecture/Pearls]]                           | The work-item tracker (Dolt-backed)                          |
| [[Architecture/Data-Storage]]                     | Dolt, smooth-dolt, named volumes, sessions, audit            |

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

- [[Decisions/ADR-Index]] — Architecture Decision Records

---

## Conventions

- Cast roles are linked by canonical name: [[Architecture/The-Cast#Big-Smooth|Big Smooth]], [[Architecture/The-Cast#Narc|Narc]], [[Architecture/The-Cast#Scribe|Scribe]], [[Architecture/The-Cast#Archivist|Archivist]], [[Architecture/The-Cast#Diver|Diver]], [[Architecture/The-Cast#Groove|Groove]].
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
