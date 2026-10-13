# Chrome Extension — bookmarks and tab control for agents

#engineering

`th chrome` lets the CLI and coding agents (over MCP) read and write the user's Chrome bookmarks, **account-synced ones included**. Once the user turns browsing on, they can also drive tabs. SMOODEV-3887.

## Why an extension

- Synced ("account") bookmarks are stored encrypted on disk (`EncryptedAccountBookmarks2`), so editing the `Bookmarks` JSON file misses them.
- AppleScript sees only local bookmarks.
- There is no public Chrome Sync API, and the DevTools Protocol has no bookmark commands.
- `chrome.bookmarks`, running inside the browser, is the only supported writer. Changes made through it go through Chrome's own model, so Sync carries them to the user's other devices.

## Pieces

```
th chrome bm … / MCP bookmarks_* ──┐
                                   ▼
            ~/.smooth/chrome/socks/<pid>.sock   (Unix socket, 0600 in a 0700 dir; no TCP)
                                   ▲
                th chrome host  (launched by Chrome, stdio native messaging)
                                   ▲
       Smooth extension (MV3) — chrome.bookmarks, optional chrome.tabs/scripting
```

| Piece     | Where                                                             | Notes                                                                                                                                                                                                                                       |
| --------- | ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Extension | `crates/smooth-cli/src/chrome/extension/`                         | Embedded in `th`; `th chrome install` writes it to `~/.smooth/chrome/extension/`. The `key` in its manifest pins the ID to `pgjpgmfljenhbbgkgjpmaghndenconlc`. Up-front permissions are only `bookmarks` and `nativeMessaging`.             |
| Host      | `chrome/host.rs` (`th chrome host`, hidden)                       | Chrome launches `~/.smooth/chrome/host`, a wrapper that runs `exec th chrome host`. The host bridges length-prefixed JSON on stdio to one JSON line per request on its socket. There is one host per Chrome profile that has the extension. |
| Manifest  | `…/Google/Chrome/NativeMessagingHosts/ai.smoo.smooth_chrome.json` | `allowed_origins` is limited to the pinned extension ID.                                                                                                                                                                                    |
| Logic     | `chrome/tree.rs`                                                  | Paths, upsert, managed guards and restore are written against an `Api` trait. The same code runs live, as a dry run (`MemTree`) and in tests.                                                                                               |
| History   | `chrome/history.rs`                                               | A git repo at `~/.smooth/chrome/history` holding `bookmarks.json` and `bookmarks.html`.                                                                                                                                                     |
| MCP       | `mcp_chrome.rs`                                                   | 11 `bookmarks_*` and 8 `browse_*` tools on `th mcp serve`.                                                                                                                                                                                  |

The extension's service worker connects to the host when it starts, on `onStartup` and on `onInstalled`. While that native port is open, Chrome keeps the worker alive. On disconnect it reconnects with exponential backoff, capped at 60s.

## Synced vs local

Chrome 134+ can return **two of each permanent folder** from `getTree()`, a local one and an account one. They are told apart only by `folderType` (`bookmarks-bar` / `other` / `mobile` / `managed`) and `syncing`. Never use the folder ID or title.

- Writes default to the folder with the matching `folderType` and `syncing: true`. `--local` / `local: true` selects the unsynced folder, and the output says **LOCAL ONLY**.
- Paths look like `Bookmarks bar/Work/Jira`. A path that doesn't start with a root name is placed under the bar. Write `\/` for a `/` inside a title.
- If there's no synced bar (Chrome not signed in), the command errors and points at `--local` instead of silently writing locally.
- Nodes under `folderType: "managed"` or with `unmodifiable` set are never written. Neither are the permanent folders.

## Safety and history

- `rm`, moving a folder, and `restore` return a dry run unless you pass `--yes` (CLI) or `confirm: true` (MCP).
- Before every destructive op, the live tree is committed (`before remove …`). Any write that changes something is committed afterwards.
- `restore <rev>` reconciles each non-managed permanent folder to the snapshot. It matches folders by title and bookmarks by title + URL, creates missing items, removes extras and fixes the order. The plan comes from a dry run against an in-memory copy.
- `history remote <url> [--auto-push]` adds a remote. Each machine pushes to its own branch (its short hostname), so several machines can share one repo. `history fetch` followed by `restore origin/<machine>` copies another machine's bookmarks.

## Browsing (opt-in)

`tabs`, `scripting` and `<all_urls>` are **optional** permissions. The user grants them with the popup's **Enable browsing** button (`chrome.permissions.request` needs a user gesture) and can revoke them the same way. Until then, every `browse_*` call fails with a message explaining how to turn it on.

`page.snapshot` tags each visible interactive element with `data-smooth-ref` (`e1`, `e2`, …), and `click`/`type` use those refs. Screenshots use `captureVisibleTab`, which first activates the tab.

## Setup

```bash
th chrome install                  # host manifest + wrapper + unpacked extension
# chrome://extensions → Developer mode → Load unpacked → ~/.smooth/chrome/extension
th chrome status                   # connected instances, browsing on/off
th chrome bm tree --depth 1
th chrome bm upsert --path "Bookmarks bar/Brokerage Test" --title "Brokerage Test - Trader" --url https://website.test.topstepbrokerage.com
```

After upgrading `th`, run `th chrome install` again and reload the extension. Several profiles can be connected at once; pick one with `--instance <pid>` (from `th chrome status`).

Managed (Workspace) profiles can block unpacked extensions through `ExtensionInstallBlocklist` or `DeveloperToolsAvailability`, and native hosts through `NativeMessagingBlocklist`. Check `chrome://policy` if loading or connecting fails.
