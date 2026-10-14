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
| Extension | `crates/smooth-cli/src/chrome/extension/`                         | Embedded in `th`; `th chrome install` writes it to `~/.smooth/chrome/extension/`. The `key` in its manifest pins the ID to `pgjpgmfljenhbbgkgjpmaghndenconlc`. Up-front permissions are only `bookmarks`, `nativeMessaging` and `alarms`.   |
| Host      | `chrome/host.rs` (`th chrome host`, hidden)                       | Chrome launches `~/.smooth/chrome/host`, a wrapper that runs `exec th chrome host`. The host bridges length-prefixed JSON on stdio to one JSON line per request on its socket. There is one host per Chrome profile that has the extension. |
| Manifest  | `…/Google/Chrome/NativeMessagingHosts/ai.smoo.smooth_chrome.json` | `allowed_origins` is limited to the pinned extension ID.                                                                                                                                                                                    |
| Logic     | `chrome/tree.rs`                                                  | Paths, upsert, managed guards and restore are written against an `Api` trait. The same code runs live, as a dry run (`MemTree`) and in tests.                                                                                               |
| History   | `chrome/history.rs`                                               | A git repo at `~/.smooth/chrome/history` holding `bookmarks.json` and `bookmarks.html`.                                                                                                                                                     |
| MCP       | `mcp_chrome.rs`                                                   | 11 `bookmarks_*` and 20 `browse_*` tools on `th mcp serve`.                                                                                                                                                                                 |

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

Browsing also asks for `tabGroups` (no warning): tabs the agent opens go in a purple **Smooth** group, and the extension badge reads `AI` while an action runs.

All page work is one self-contained function, `pageCall` in `background.js`, injected with `chrome.scripting.executeScript` into the extension's isolated world:

- **Snapshot.** A DOM walk, through open shadow roots, and across every frame (`allFrames`; subframe refs look like `f7:e3`). It emits an indented outline: interactive elements with refs, nested under headings, forms, dialogs, landmarks, table rows and list items, so two "Edit" buttons can be told apart by their row. It records state (value, with passwords masked; options; checked, expanded, disabled, required) and flags elements that are offscreen or covered by an overlay (`elementFromPoint`). Elements styled only with `cursor:pointer` count as interactive. The header says how many viewports are above and below.
- **Refs.** These live in a `WeakRef` map on the isolated world's global, so the page can't see them, refs stay stable between snapshots, and numbers never repeat. If the element under a ref is re-rendered, the ref is re-matched to the single element with the same role and name.
- **Actions.** Click sends pointer and mouse down/up at the element's centre, then `click()`. Type uses `execCommand('insertText')`, which React and rich editors both see, and falls back to the native value setter. Select matches an option by its text or value. There are also key, hover, scroll (by viewports, to some text, or inside a panel), fill (several fields at once) and waitFor. Every action waits for a navigation or for the DOM to settle (a MutationObserver quiet for 200ms, capped at 3s). It reports a URL change or a newly opened tab, and the MCP tools return a fresh snapshot.
- **Limits.** Events are synthetic (`isTrusted` is false), so a site that checks it, a canvas editor, or a native file picker won't respond. A JS dialog blocks the page, and the action times out and says so. Trusted input (below) fixes all of these.

Screenshots use `captureVisibleTab`, which first activates the tab. `node scripts/chrome-page-smoke.mjs` runs `pageCall` against a fixture page in headless Chrome and checks every op.

## Trusted input (opt-in)

`debugger` is a second optional permission, granted with the popup's **Enable trusted input** toggle (separate from Enable browsing). It shows Chrome's "Smooth started debugging this browser" bar while attached and carries the strongest permission warning, which is why it is off by default. With it on:

- `trusted: true` on `browse_click`, `browse_type`, `browse_key`, `browse_hover` and `browse_scroll` (CLI `--trusted`) sends real input through CDP: `Input.dispatchMouseEvent`, `Input.dispatchKeyEvent` (with the macOS editing `commands`, without which keys do nothing in a Mac text field) and `Input.insertText`. Targets are a main-frame ref or an `x`/`y` point; `x`/`y` imply trusted. Click also takes `button` (right, middle) and `count` (2 = double click); key takes `repeat`.
- `browse_screenshot` captures through `Page.captureScreenshot` without switching tabs, scaled to CSS pixels, so a point in the image is the `x`/`y` to click.
- New tools: `browse_eval` (`Runtime.evaluate`; `await` and a final `return` work), `browse_console` and `browse_network` (buffers fed by `Runtime.consoleAPICalled`/`exceptionThrown` and `Network.*`; the first call starts capturing; capped at 1000 messages and 500 requests), `browse_dialog` (show/accept/dismiss an alert, confirm or prompt via `Page.handleJavaScriptDialog`) and `browse_upload` (`DOM.setFileInputFiles`; paths must be existing local files).
- Sessions attach per tab (protocol 1.3), single-flight, and detach after 30s idle unless console or network capture is on. Browser pages (`chrome://` and the like) and tabs that DevTools or another extension already debugs are refused. `th chrome status` shows `trusted input on/off`.

The builders live in `extension/cdp.js` (pure functions, no `chrome.*`). `node scripts/chrome-cdp-smoke.mjs` replays them against headless Chrome over `--remote-debugging-port` and checks the page sees `isTrusted` clicks (left, right, double), hover, keys, typed Unicode, Enter submitting, select-all, wheel scrolling and a CSS-pixel screenshot. `pnpm test:chrome-page` runs both smoke scripts.

### Parity with Claude in Chrome

| Claude in Chrome                                              | Smooth                                                                       |
| ------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| `tabs_context`                                                | `browse_tabs`                                                                |
| `tabs_create` / `tabs_close`                                  | `browse_open` / `browse_close`                                               |
| `navigate`                                                    | `browse_open` with `tab`, `browse_nav` (back, forward, reload)               |
| `computer` (click, type, key, scroll, hover)                  | `browse_click`/`type`/`key`/`scroll`/`hover`, by ref or (trusted) by `x`/`y` |
| `computer` (screenshot)                                       | `browse_screenshot`                                                          |
| `read_page`                                                   | `browse_snapshot`                                                            |
| `find`                                                        | `browse_text` with `find`, `browse_scroll` with `text`                       |
| `form_input`                                                  | `browse_fill`, `browse_select`                                               |
| `get_page_text`                                               | `browse_text`                                                                |
| `javascript_tool`                                             | `browse_eval`                                                                |
| `read_console_messages`                                       | `browse_console`                                                             |
| `read_network_requests`                                       | `browse_network`                                                             |
| `file_upload`                                                 | `browse_upload`                                                              |
| —                                                             | `browse_dialog`, `browse_wait_for` (Claude in Chrome has no equivalent tool) |
| `resize_window`, `gif_creator`, `upload_image`, `shortcuts_*` | Not done                                                                     |

## Setup

```bash
th chrome install                  # host manifest (every installed Chromium browser; --browser for one) + wrapper + unpacked extension
# chrome://extensions → Developer mode → Load unpacked → ~/.smooth/chrome/extension
th chrome status                   # connected instances, browsing on/off; with none, checks the setup (alias: doctor)
th chrome bm tree --depth 1
th chrome bm upsert --path "Bookmarks bar/Brokerage Test" --title "Brokerage Test - Trader" --url https://website.test.topstepbrokerage.com
```

Folder paths match titles exactly, then fall back to a single case-insensitive match, so `brokerage test` reuses `Brokerage Test` rather than creating a second folder. The up-front `alarms` permission (no install warning) wakes the worker each minute, so it reconnects after a host crash or a `th` upgrade.

After upgrading `th`, run `th chrome install` again and reload the extension. Several profiles can be connected at once; pick one with `--instance <pid>` (from `th chrome status`).

Managed (Workspace) profiles can block unpacked extensions through `ExtensionInstallBlocklist` or `DeveloperToolsAvailability`, and native hosts through `NativeMessagingBlocklist`. Check `chrome://policy` if loading or connecting fails.

## Prior art

Reviewed against this design, with clones in `~/dev/refs`:

- **Bookmarks.** [mcp-chrome](https://github.com/hangwin/mcp-chrome), the closest match (an extension, a native host and MCP), finds the bar by its English title, adds a duplicate when asked twice, and deletes every URL match without asking. Ours selects by `folderType` and `syncing`, upserts idempotently, and gates deletes behind `--yes` plus a git snapshot. [Floccus](https://github.com/floccusaddon/floccus) is the prior art for syncing bookmarks through git.
- **Browsing.** These shaped `pageCall` and the tool surface: [browser-use](https://github.com/browser-use/browser-use), [chrome-devtools-mcp](https://github.com/ChromeDevTools/chrome-devtools-mcp), [Playwright MCP](https://github.com/microsoft/playwright-mcp), [Stagehand](https://github.com/browserbase/stagehand), [nanobrowser](https://github.com/nanobrowser/nanobrowser) and [BrowserMCP](https://github.com/BrowserMCP/mcp). We borrowed:
    - the indented outline with state and viewport hints;
    - waiting for the DOM to settle after each action, and returning a snapshot with the result;
    - `fill`, `select`, `key`, `scroll` and `wait_for`;
    - shadow DOM and frame traversal, the `cursor:pointer` and covered-element checks, and re-resolving stale refs.
- **Trusted input.** Most of these attach `chrome.debugger` for real input, dialogs and console/network logs, and so does Claude in Chrome. We do the same, but only as the opt-in tier above, because of the debugging bar and the install warning. From Claude in Chrome we took per-tab attach, the Mac editing commands, focus emulation plus a throwaway capture so background tabs paint, and idle detach.
