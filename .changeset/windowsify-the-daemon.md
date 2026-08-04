---
'@smooai/smooth': patch
---

Windowsify Big Smooth — the daemon now compiles, runs, and is CI-tested on Windows (pearl th-a59af5).

`smooth-daemon` was `--exclude`d from the Windows CI lane, so the actual product was never even compiled on a platform we intend to ship to. The blocker was one leaf dependency: `web-push` → `ece` → `openssl-sys`, and a stock Windows host has no OpenSSL. `web-push` is now declared for non-Windows targets only — Windows gives up phone notifications (`/push/*` 503s, `send_to_all` is a no-op) and gains the entire agent engine. The exclude is gone from `pr-checks.yml`.

Runtime fixes behind it, all of which failed silently rather than loudly:

- **`$HOME` is not set on Windows.** `SandboxPolicy::for_workspace` read it directly, so `policy.home` was `None` and *every* credential-deny rule was dropped. Same bug in the push-subscription store (scattered `push-subs.json` into the cwd) and in `th api`'s user-JWT lookup, where both non-override candidates evaporated and a perfectly good session reported "not logged in". All three now resolve through `dirs_next::home_dir()`, and the auth lookup delegates to `smooth_policy::auth_paths` — the single source of truth — instead of re-deriving paths by hand.
- **No `sh` on Windows.** The non-macOS sandbox fallback spawned `sh -c`, so every `bash` tool call failed to spawn. Windows now gets `cmd /C`.
- **No `.exe` suffix.** `th` and `smooth-daemon` were located by bare name, which matches nothing on Windows; the daemon downloader also fetched `…-msvc.exe` and saved it as extensionless `smooth-daemon`, which cannot be executed by full path. Both go through `std::env::consts::EXE_SUFFIX` now.
- **`th service logs` shelled out to `tail`.** Windows uses `Get-Content -Tail`/`-Wait`, and the logon Scheduled Task now redirects stdout/stderr into the same `service.log`/`service.err` launchd and systemd write — previously it discarded them, so the logs were always empty.

Adds `docs/Architecture/Windows-Security-Posture.md` — the loud version of the caveat. The kernel sandbox is macOS-Seatbelt-only (th-08e05a), so on Windows `bash` runs with the operator's own token: credential stores are readable and writable, `.git/hooks` is plantable, and the goalie egress allowlist drops from a boundary to a suggestion. Linked from `Security-Model.md`. Do not ship a Windows build presenting it as sandboxed.

Enabling the lane immediately paid for itself — it caught three real bugs that had shipped unnoticed, all of which were product defects rather than test artifacts: skill discovery built mixed-separator paths (`C:\\ws\\.smooth/skills\\x\\SKILL.md`) that went into the persona for the model to read back; the composer's `@`-mention path completion returned nothing for any Windows absolute path, because the anchor check only accepted `/`, `~`, `./`, `../`; and search results carried native separators, so one file had two spellings depending on host.

A fourth was found by auditing rather than by a failing test, and is the serious one: **`permission::workspace_relative` made Gate-1 path denies silently inert on Windows.** Rules are authored with forward slashes, the match target came back native, the glob never matched, and the deny waved the write through with no error and no log. The existing test passed there by accident because it embedded a `/` in its fixture path. Relative paths reaching the matcher — and the model — now share a `util::to_slash` helper that is Windows-only, so a Unix filename containing a legitimate backslash is no longer corrupted either.

macOS behavior is unchanged: the Seatbelt profile, tool set, and service installer are byte-identical, and `cargo fmt`/`clippy`/`test` are green there. The Windows lane is 1622/1622 passing.
