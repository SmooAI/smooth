# Architecture Decision Records (ADR) Index

#moc #decision

Architecture Decision Records capture significant technical decisions in Smooth. Each ADR documents context, decision, reasoning, and consequences. ADRs are numbered sequentially and are immutable once accepted — reversal happens by writing a new ADR that supersedes the original.

---

## When to write an ADR

Write one when a decision:

- Affects the system architecture or a major subsystem
- Is difficult or expensive to reverse
- Has been debated and a clear choice was made
- Involves adopting or replacing a significant technology

---

## Decision Records

| ADR                                          | Title                                     | Status   | Date    |
| -------------------------------------------- | ----------------------------------------- | -------- | ------- |
| [[ADR-001-Consolidate-into-one-microVM]]     | Consolidate Boardroom and Operator VMs    | Accepted | 2026-05 |

> [!todo] More to backfill
> Older decisions worth ADRing once we have time: Dolt over SQLite for pearls (2025), microsandbox over Firecracker (2025), single Rust binary over multi-binary CLI (2024), pearls naming over beads/issues (2025), workflow phases as the default agent loop (2026), gRPC + UDS for in-VM cast comms (2026; see `single_process.rs`).

---

## Templates

- [[../_templates/ADR-Template]]

## Related

- [[../Architecture/Architecture-Overview]]
