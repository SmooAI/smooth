---
'@smooai/smooth': patch
---

th msg watch: add `--from` / `--type` filters and a non-consuming `--peek` mode

Watching the agent-mail bus for a specific correspondent used to mean hand-rolling
a `sqlite3` poll over `~/.smooth/mail.db` — the existing `th msg watch` could only
watch your whole inbox and, in continuous mode, acked (consumed) every message it
saw. Two additions close that gap so every harness can use the one tested command:

- `--from <agent>` and `--type <kind>` narrow the stream to one sender or one
  message type (both normal and peek mode).
- `--peek` tracks position by message `seq` instead of read-state and never acks,
  so a machine consumer (a harness responder, the th-mail skill) reacts to each new
  message without marking it read — the owner still decides when it has actually
  been handled. `--since <seq>` pins a durable watermark across restarts.

Backed by new `MailStore::inbox_since` / `max_seq` (and the `Mail` backend wrappers;
cloud emulates via a filtered inbox fetch). Part of the resilient-messaging epic
(th-826c4a).
