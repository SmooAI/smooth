---
'smooai-smooth-policy': patch
---

EPIC th-c89c2a (th-515a13): Gate 1 — the deterministic deny/ask/allow permission
rule model. `Decision` (Deny/Ask/Allow), `Matcher` (Claude-Code `Tool(pattern)`
syntax with `:` as a word boundary, e.g. `Bash(rm:*)` ≡ `Bash(rm *)`), and
`PermissionRules` with **deny > ask > allow** precedence and a **fail-safe `Ask`
default** (an unmatched call never silently allows). Pure model + matcher,
exhaustively tested incl. adversarial inputs. The intent layer above the kernel
sandbox; wiring it as a ToolHook on the operator registry is a following slice.
