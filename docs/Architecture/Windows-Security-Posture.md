# Windows Security Posture

**Status: Big Smooth on Windows runs with NO kernel sandbox.** Pearl th-a59af5
(engine half of cross-platform), sandbox gap tracked as th-08e05a.

This page exists so nobody ships a Windows build believing it has the same
containment as the macOS one. It does not. Read this before enabling a Windows
release, and link it from anything that offers a Windows download.

## The three security layers, per platform

[[Security-Model]] describes three layers a tool call passes through. Only two
of the three exist on Windows.

| Layer                                                                | macOS       | Linux   | Windows     |
| -------------------------------------------------------------------- | ----------- | ------- | ----------- |
| 1. Permission gate (`smooth-policy` auto-mode + `DenyPolicy`)        | ✅          | ✅      | ✅          |
| 2. Narc surveillance (regex detectors + LLM judge, secret redaction) | ✅          | ✅      | ✅          |
| 3. **Kernel OS sandbox** (`smooth-tools/src/sandbox.rs`)             | ✅ Seatbelt | ❌ TODO | ❌ **TODO** |
| Secret-env scrubbing at the spawn point                              | ✅          | ✅      | ✅          |
| Egress boundary (goalie exact-host allowlist, kernel-enforced)       | ✅          | ❌      | ❌          |

Layers 1 and 2 are **userspace**. They are worth having, and they are not the
load-bearing boundary — the whole premise of the security model is that a
reasoning agent can talk its way past an intent-level check but cannot talk its
way past the kernel. On Windows there is currently no kernel-level check to
fail.

## What is actually unprotected on Windows

The `bash` tool runs `cmd /C <command>` as a plain subprocess with the
operator's own access token. Concretely, a hijacked shell — via prompt
injection, a malicious repo's postinstall script, whatever — can do everything
the logged-in user can:

- **Read any credential store.** `%USERPROFILE%\.ssh`, `.aws`, `.kube`,
  `.docker`, `.gnupg`, `_netrc`, `AppData\...\gh` — and Big Smooth's _own_
  secrets in `%USERPROFILE%\.smooth` (`providers.json`'s LLM key, the `auth/`
  JWT). On macOS every one of these is a kernel read-deny. Here, nothing stops
  it.
- **Overwrite those same stores**, and plant persistence (Run key, Startup
  folder, a Scheduled Task) that later executes _outside_ any agent context.
- **Write `.git/hooks/*` and `.git/config` in any repo on the machine**, which
  re-enters execution outside the agent on the next git operation. Kernel-denied
  on macOS in _every_ repo, precisely because this is the cheapest escape.
- **Reach the network directly.** The goalie egress allowlist is only
  load-bearing because the macOS sandbox kernel-denies non-loopback outbound, so
  a tool that ignores `HTTP_PROXY` simply cannot connect. On Windows the proxy
  env vars are still set, but ignoring them works — the allowlist becomes a
  suggestion. This is the third leg of the lethal trifecta (private-data access
    - untrusted content + egress) left open.

### Layer 1 was silently inert for path rules — fixed in th-a59af5

Worth recording because it is the exact failure mode this page exists to warn
about. `permission::workspace_relative` built the string the Gate-1 matcher runs
rules against using the platform's native separator, while rules are authored
with forward slashes (`Write(.git/hooks/**)`). On Windows the target came out as
`.git\hooks\pre-commit`, the glob never matched, and **the deny silently waved
the write through** — no error, no log, an operator believing a path was
protected when it was wide open. The crate's own test passed on Windows by
accident because it built its fixture path with an embedded `/`.

Fixed (targets are normalized to `/` before matching, with a test that builds
the path component-by-component). Called out here because the same shape —
a Unix-separator assumption that turns an _enforcement_ check into a no-op
rather than an error — is the thing to look for in any future Windows work.

### Layer 1 is only _partly_ Windows-shaped

Separately from the missing kernel layer: the daemon's embedded `DenyPolicy`
path list (`DENY_POLICY_TOML` in `smooth-daemon/src/operator.rs`) was written
for Unix. The home-relative globs still bite on Windows —

- `**/.ssh/**`, `**/.aws/**`, `**/.smooth/auth/**`, `**/.smooth/operator-token`,
  `**/.smooth/operator-storage.db*`, `**/.smooth/schedules.db*` ✅

— but every absolute-rooted entry matches nothing there:

- `/etc/**`, `/System/**`, `/usr/**`, `/bin/**`, `/sbin/**`, `/Library/**` ❌
- and there are **no** entries for `C:\Windows\**`, `C:\Program Files\**`, the
  per-user Startup folder, or the `Run` registry key — the Windows equivalents
  of exactly what those globs exist to protect.

The command denylist has the same shape problem: it names `launchctl`,
`kextload`, `nvram`, `diskutil`, `crontab` and friends, with no `reg.exe`,
`schtasks`, `vssadmin`, `bcdedit`, or `Set-MpPreference`.

Adding them is not a one-liner to do blind: `globset` treats `\` as an escape
character by default, so a naively-added `C:\Windows\**` compiles to something
that never matches and reads as protection while providing none. It needs
`GlobBuilder::backslash_escape(false)` (or forward-slash normalization at the
call site) plus a real Windows host to verify against. Tracked with th-08e05a;
do not add Windows path denies without testing them on Windows.

What _does_ still hold on Windows:

- **Secret-named env vars are scrubbed** from the child at the single spawn
  point (`scrub_secret_env`), so `set` / `env` cannot dump the daemon's own
  credentials out of its process environment.
- The **permission gate runs first** and short-circuits denies before the tool
  executes, and **Narc** still redacts detected secrets out of tool results.
- The daemon still binds **loopback only** by default.

## Consequences for a Windows release

1. **Single trusted operator, trusted workspaces only.** Do not point a Windows
   Big Smooth at a repo you would not run `npm install` in unattended.
2. **Do not present Windows as sandboxed** in any UI, README, or release note.
   `SandboxPolicy::is_enforced()` returns `false` there — surface that, don't
   paper over it.
3. Prefer a stricter permission posture: `SMOOTH_AUTO_MODE=ask` rather than the
   default `Bypass`, since the userspace gate is the _only_ gate.

## The fix (th-08e05a)

The Windows equivalent of Seatbelt is a **restricted token + Job Object**, or an
**AppContainer** with an explicit capability set. Either confines a child
process without requiring an elevated parent. The shape mirrors macOS: build the
restriction, apply it to the `bash` child only, never to the daemon itself.

Both approaches need Win32 calls that the workspace's `unsafe_code = "forbid"`
lint disallows, so the implementation belongs behind a vetted crate (e.g.
`windows-rs` wrappers) or in the existing quarantine-crate pattern used for
`smooth-menubar`.

Until that lands, this page is the honest answer to "is Windows safe?" — the
userspace layers are, the kernel layer isn't there yet.

## Other Windows gaps (not security)

- **Web push is unavailable.** `web-push` → `ece` hard-depends on `openssl`, and
  a stock Windows host has none. The dependency is declared for non-Windows
  targets only; `/push/*` routes 503 and `send_to_all` is a no-op. This is what
  unblocked compiling the daemon on Windows at all.
- **macOS-native tools are absent by design** — `calendar`, `calendar_delete`,
  `reminders`, `imessage` and the menu bar are `#[cfg(target_os = "macos")]`, so
  the Windows tool registry simply doesn't contain them. Windows-native
  equivalents (Outlook / Microsoft Graph calendar + tasks) are a follow-up, not
  a regression.
- **Pearl-store tests are skipped** on the Windows CI lane: the `smooth-dolt`
  server transport is a Unix domain socket (TCP transport is th-5f35a5). The
  `smooth-dolt` binary itself still compiles there.

## See also

- [[Security-Model]] — the full three-layer model and the macOS enforcement detail
- `crates/smooth-tools/src/sandbox.rs` — where all of this is implemented
- [[Windows-Build-Box-Runbook]] — building and testing on a real Windows host
