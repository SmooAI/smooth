---
status: Accepted
date: 2026-07
deciders: Brent
supersedes: ADR-001, ADR-002, ADR-003
superseded-by: None
tags: [decision, security, runtime]
---

# ADR-004: Remove the microVM sandbox stack — dispatch as a host subprocess

#decision

**Date**: 2026-07
**Status**: Accepted
**Pearl**: th-f4a801 (implementation, PR #124); th-7b0bed (docs)

## Context

Smooth's original pitch was hardware isolation: `th up` booted a microsandbox microVM ("the Safehouse") hosting Big Smooth plus a security cast — **Wonk** (access-control authority), **Goalie** (network/filesystem proxy), **Bootstrap Bill** (VM lifecycle broker) — and every dispatched operative ran inside that kernel boundary with policy-enforced egress and filesystem deny patterns. [[ADR-001-Consolidate-into-one-microVM|ADR-001]] consolidated the earlier two-VM topology into one VM; [[ADR-002-microsandbox-0.4.6-and-remove-docker-backend|ADR-002]] and [[ADR-003-rename-boardroom-to-safehouse|ADR-003]] refined it.

In practice the VM stack was the platform's biggest source of friction:

- ~30s cold boots (vs ~0.3s direct) and run-to-run variance that hurt bench scores.
- A permanent "direct mode" escape hatch (`th up direct`) that everyone — CI, bench harness, daily dev — actually used, leaving the sandboxed path under-exercised.
- Nested-virtualization gaps (Apple HVF) that broke operator dispatch from inside the Safehouse.
- A cross-compile toolchain (musl + zig), OCI image publishing, image-cache staleness, and runner-bin mirroring, all just to get a binary into the guest.
- The smooth-daemon rewrite (EPIC th-c89c2a) targets an always-on personal-agent daemon where a VM-per-platform model doesn't fit.

## Decision

Remove the microVM sandbox stack entirely (2026-07, pearl th-f4a801, PR #124):

- `th up` boots Big Smooth **directly on the host** and daemonizes; `th up direct` is gone because there is nothing to be "direct" relative to.
- Dispatch execs the native `smooth-operative` binary as a **host subprocess**; tool calls pass through **in-process Narc surveillance** (CliGuard, secret/prompt-injection detectors, LLM judge).
- Deleted crates: `smooth-wonk`, `smooth-goalie`, `smooth-bootstrap-bill`, `smooth-host-stub`, `smooth-credential-helper`.
- The `aarch64-unknown-linux-musl` cross-compile, the Safehouse OCI image, and the `microsandbox` dependency are gone; `smooth-operative` is a plain native build.

## Consequences

**Lost:**

- **Hardware isolation.** No kernel boundary around the agent — tools execute against the host filesystem with the user's privileges.
- **Wonk/Goalie enforcement.** No policy-gated network egress, no filesystem deny patterns, no kernel-enforced proxy. Narc remains, but it is surveillance and pattern-blocking, not an enforcement boundary.
- The security white paper's headline claims (`docs/white-paper-security-architecture.md`) now describe a removed architecture; it is retained as a historical record.

**Gained:**

- Sub-second boot, no image pulls, no cross-compile toolchain, one dispatch path that is the tested path.
- A dramatically smaller workspace and simpler `pnpm install:th`.

**Path to resurrect:** the full implementation lives in git history at PR #124's parent commit. Isolation is expected to return in a different shape via the smooth-daemon rewrite (EPIC th-c89c2a) and the sandboxing follow-up pearl th-515a13, rather than by reviving the Wonk/Goalie/microsandbox stack as-was.

## Related

- [[ADR-001-Consolidate-into-one-microVM]] — superseded
- [[ADR-002-microsandbox-0.4.6-and-remove-docker-backend]] — superseded
- [[ADR-003-rename-boardroom-to-safehouse]] — superseded
- [[../Architecture/Dispatch]]
- [[../Operations/Running-Locally]]
