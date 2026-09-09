---
'@smooai/smooth': patch
---

Add SMOOTH_DEMO mode: a locked-down daemon for the App Store reviewer demo (th-a455be).

Big Smooth iOS can't be reviewed without a paired Mac daemon (its empty state is "Open Big Smooth on your Mac"), so submitting it needs a safe hosted demo a review account auto-connects to over the relay. But relay phones authenticate as the daemon owner (full toolset, Bypass) — Plan mode and family RBAC don't gate them — so env-only lockdown isn't airtight. `SMOOTH_DEMO=1` clamps every turn to a deny-by-default read-only allowlist (`DEMO_SAFE_TOOLS`) applied last and unconditionally, so a reviewer (or anyone with the demo creds) gets chat + safe reads only — never bash / write / th / calendar / imessage / MCP — regardless of mode or principal. Pair with `SMOOTH_WORKSPACE` (throwaway dir) + `SMOOTH_EGRESS_ALLOWLIST`. Runbook: docs/Operations/App-Store-Reviewer-Demo.md.
