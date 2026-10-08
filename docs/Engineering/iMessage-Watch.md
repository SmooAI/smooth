# iMessage: group lookup and watch-a-thread auto-reply

Pearl th-592d67. Two pieces, both chat-driven, so every Big Smooth client (web,
desktop, iOS, Android, `th code`) gets them with no client change.

## Why

Brent sent Big Smooth a screenshot of an iMessage group chat and asked it to
introduce itself, then to read the replies and banter back. The group had no name
in `chat.db` (identifier `chat358836017578106964`, empty `display_name`), and:

- `conversations` returned no participants, so one unnamed group looked like every
  other unnamed group;
- message rows carried phone numbers, while the screenshot showed names;
- `thread` matched names loosely and couldn't read a group by GUID.

Matching the screenshot to a GUID was guesswork. The intro turn took 1m45s. Then
Brent asked for a way to "monitor for responses on a thread and automatically
reply".

## Part 1: finding and reading a group

All of this lives in `crates/smooth-tools/src/imessage.rs` (the read side) and
`crates/smooth-tools/src/contacts.rs` (names).

| Call                                                     | What it returns                                                                                                                                                    |
| -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `{"command":"conversations"}`                            | each chat's `chat` GUID, `name`, `is_group`, and `participants: [{handle, name}]`, with names from the user's Contacts                                            |
| `{"command":"conversations","with":["Alice","Bob"]}`     | only chats that include **every** listed person (name substring, handle, or phone digits), or whose display name matches. When several match, each carries a short `last_message` |
| `{"command":"thread","chat":"iMessage;+;chat…"}`         | exactly that chat, oldest first                                                                                                                                    |
| any message read                                         | rows carry `is_from_me` and `sender_name` next to the raw `handle`                                                                                                 |

The screenshot recipe (it's in the tool description): `conversations` with the
visible names as `with`, pick the GUID, `thread` by `chat`, `send` by `chat`.

Names come from `contacts::resolve_names`, which reads every Address Book source
once per call and matches emails case-insensitively and phones on their last ten
digits. Without Full Disk Access it returns nothing: names are `null` and the raw
handles still work, including `with` by number.

On privacy, the `last_message` preview is capped at 60 characters and only appears
when a `with` search matched more than one chat, which is the one case where the
model needs it to choose.

## Part 2: watching a thread

`crates/smooth-daemon/src/imessage_watch.rs`. There are two tools:

- **`imessage_watch`** starts a watch: `chat` (GUID) or `contact` (exact handle),
  plus `instructions` (the user's words about tone and what to say or avoid), and
  optionally `minutes`, `max_replies` and `max_per_hour`.
- **`imessage_watches`** lists watches (`list`) or stops them (`stop` by `id`,
  `chat`, or `all`).

A poll loop (`spawn_watcher`, every 10s) reads each watched chat past a ROWID
watermark with a single indexed read-only query, and answers settled bursts.

### Guardrails

| Guardrail                    | How                                                                                                                                                                                                           |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Starts only when asked       | The tool is described as user-request-only. It's mutating, so it isn't on Plan mode's allowlist, and it also refuses in a Plan conversation itself. It's absent from the App Store demo clamp. |
| One chat                     | Exact GUID or exact handle. No loose name match, and one watch per chat.                                                                                                                                       |
| Only new messages            | The watermark starts at the chat's newest ROWID, so history is never answered.                                                                                                                                 |
| Time box                     | Default 2h, clamped to 5 min to 24h. An expired watch ends itself and says so.                                                                                                                                 |
| Debounce                     | A burst is answered once, after 45s with no new inbound, or 3 min after it began if it never goes quiet.                                                                                                      |
| Rate limits                  | 90s minimum between replies. At most 6/hour by default (hard cap 12). At most 20 per watch by default (hard cap 50); reaching that cap ends the watch. At most 5 watches at once.                                |
| Never answers "me"           | `is_from_me` rows never trigger a reply. A message the **user** sends counts as answering everything before it, so the watch never talks over them. The watch's own replies are recognised by text and ignored. |
| No other actions             | The draft is ONE model call with **no tools** (`LlmBrain`), and it can only return reply, handoff, or skip as JSON. The daemon sends the text to the watched chat. There's no tool to email, buy, or text another chat. |
| No commitments               | The drafter's rules forbid agreeing to plans, meetings, money, or promises. `vet_reply` refuses a draft with a `$` amount, a phone number or email, a Narc-detected secret, or more than 600 characters; a refused draft becomes a handoff. |
| Sensitive means hand off     | `screen_inbound` hands a batch to the user **without drafting** when it mentions codes, passwords or account numbers, money (Venmo, Zelle, `$20`, …), emergencies, or trips Narc's prompt-injection detector. The model is also told to hand off anything only the user should decide. |
| Visible                      | Every reply, handoff, hourly pause and end is appended to the Big Smooth conversation that started the watch, so every client shows it and the next turn has it in context. Pushes go out for handoffs, the end of a watch, and only the **first** reply. Push bodies never include message text. |
| Stoppable anywhere           | `imessage_watches stop` works from any conversation and stays available in Plan mode. Stopping from a different conversation notes the stop in the original one.                                               |
| Fail closed                  | If the chat vanishes, the watch ends. After three unreadable polls or three failed drafts, it ends. If a send doesn't come back `{"sent":true}`, the watch **ends** instead of retrying into a possible double send. |
| Survives restart             | Watches, including watermark, reply history and counters, persist in `~/.smooth/imessage-watches.db` (`SMOOTH_IMESSAGE_WATCH_DB` overrides it).                                                               |

### Why a tool-less drafter, not a scheduled agent turn

The proactive scheduler (`scheduler.rs`) fires a full agent turn with every tool.
That's right for "summarise my inbox every morning". It's wrong for a turn whose
input is written by other people (anyone in the group can text it) and whose
output goes out as the user. Taking the tools away is the only guarantee that a
prompt-injected message can't make the reply turn do something else. The poll
loop keeps the scheduler's shape (a store, a pure tick, a spawned interval loop),
but its "turn" is a draft.

Narc's tool hooks still see every model-initiated `imessage` send. The auto-reply
send is initiated by the daemon, so Narc's detectors run explicitly instead:
`scan_injection` on inbound text and `scan_secrets` on the draft.

## Tests

- `crates/smooth-tools/src/imessage.rs`: participants, names, the `with`
  filter, previews, GUID `thread`, and the typed reads (`find_chat`,
  `chat_messages_after`, `chat_recent_messages`, `chat_latest_rowid`) against a
  fixture `chat.db` with two unnamed groups.
- `crates/smooth-tools/src/contacts.rs`: batch name resolution, formatting-agnostic
  phones, first-source-wins, and degrading to no names.
- `crates/smooth-daemon/src/imessage_watch_tests.rs`: start, clamps, refusals,
  Plan-mode refusal, debounce, max wait, never answering the user or its own echo,
  gap/hourly/per-watch caps, expiry, unreadable and vanished chats, handoff,
  screening, vetting, skip, draft failures, unconfirmed send, persistence, and
  list/stop. All of it uses fake drafter, sender and reporter; nothing texts a
  real person.
- `crates/smooth-daemon/src/operator.rs`
  (`plan_mode_drops_imessage_watch_but_keeps_the_stop_switch`): Plan drops
  `imessage_watch` and keeps `imessage_watches`.

## Follow-ups

- An "active watches" indicator in the clients. The data exists (`imessage_watches
  list`); the web SPA and the iOS/Android apps don't show it yet.
