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
> A single Rust binary (`th`) that runs an AI agent stack on your machine. Boots in two modes, has one VM in sandboxed mode, talks to LLMs through a policy-aware proxy, and writes through a deterministic tool surface. No Docker. No nested virtualization. No cloud.

Smooth is part of the [Smoo AI](https://smoo.ai) platform — AI built into every product. This vault is the canonical source of truth for Smooth's architecture, operations, and decisions. Start with [[Start-Here/What-Is-Smooth]], then follow the map below.

---

## The two-mode picture

```
                  th up                          th up direct
                    │                                  │
        ┌───────────▼────────────┐          ┌──────────▼──────────┐
        │  microsandbox microVM  │          │      host shell      │
        │  (default; hardware    │          │   (no sandbox; only  │
        │   isolated)            │          │    trusted envs)     │
        │                        │          │                      │
        │   Big Smooth + Cast    │          │   Big Smooth + Cast  │
        │   + Operatives   │          │   + Operatives │
        └────────────────────────┘          └──────────────────────┘
```

One VM. Same cast. Same dispatch path. Direct mode is the escape hatch when you are already inside a trusted environment (CI runner, dedicated devbox) and want zero overhead. Sandboxed mode is the default.

---

## Start here

| Page                                              | Description                                                 |
| ------------------------------------------------- | ----------------------------------------------------------- |
| [[Start-Here/What-Is-Smooth]]                     | One-pager. What `th up` boots, what gets dispatched, the why |
| [[Start-Here/Glossary]]                           | Cast roles, modes, terms                                    |
| [[Operations/Running-Locally]]                    | `th up`, `th up direct`, `th down`, `th code`               |

---

## Architecture

| Page                                              | Description                                                  |
| ------------------------------------------------- | ------------------------------------------------------------ |
| [[Architecture/Architecture-Overview]]            | Top-level diagram + control flow                             |
| [[Architecture/The-Cast]]                         | Big Smooth, Wonk, Goalie, Narc, Scribe, Archivist, Diver, Groove |
| [[Architecture/Sandboxed-Mode]]                   | The default. microsandbox microVM, what's inside             |
| [[Architecture/Direct-Mode]]                      | Host runtime. When to reach for it                           |
| [[Architecture/Transport]]                        | gRPC over UDS, in-process Arc, HTTP at the edge — the wire story |
| [[Architecture/Dispatch]]                         | How a task flows from `th up` chat to an operator and back  |
| [[Architecture/Operatives]]                        | The agent runtime, the operative binary, tool surface  |
| [[Architecture/Pearls]]                           | The work-item tracker (Dolt-backed)                          |
| [[Architecture/Data-Storage]]                     | Dolt, smooth-dolt, named volumes, sessions, audit            |

---

## Engineering

| Page                                              | Description                                  |
| ------------------------------------------------- | -------------------------------------------- |
| [[Engineering/Build-Workflow]]                    | `cargo`, cross-compile to musl, `pnpm install:th` |
| [[Engineering/Bench-Harness]]                     | `th bench`, scoring, The Line                |

---

## Operations

| Page                                              | Description                                                 |
| ------------------------------------------------- | ----------------------------------------------------------- |
| [[Operations/Running-Locally]]                    | Quickstart, both modes, common knobs                        |
| [[Operations/Troubleshooting]]                    | Known traps, runner missing, port collisions, sandbox stalls |

---

## Decisions

- [[Decisions/ADR-Index]] — Architecture Decision Records

---

## Conventions

- Cast roles are linked by canonical name: [[Architecture/The-Cast#Big-Smooth|Big Smooth]], [[Architecture/The-Cast#Wonk|Wonk]], [[Architecture/The-Cast#Goalie|Goalie]], [[Architecture/The-Cast#Narc|Narc]], [[Architecture/The-Cast#Scribe|Scribe]], [[Architecture/The-Cast#Archivist|Archivist]], [[Architecture/The-Cast#Diver|Diver]], [[Architecture/The-Cast#Groove|Groove]].
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
