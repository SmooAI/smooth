# App Store Reviewer Demo — the locked-down Big Smooth daemon

Big Smooth iOS is a client for the user's **own** Big Smooth daemon on their Mac.
Its empty state says "Open Big Smooth on your Mac", so an App Store reviewer can't
exercise it without a paired daemon — and Apple rejects apps they can't fully test
(Guideline 4.2 / 2.1). This runbook stands up a **safe, always-on demo daemon** that
a demo/review Smoo account auto-connects to over the relay, so the reviewer just
signs in and chats. Pearl th-73e3bf (submission) / th-a455be (this daemon mode).

## Why it's safe

The relay bridge authenticates a phone as the daemon **owner** (full toolset, Bypass
mode) — Plan mode + family RBAC do **not** constrain a relay reviewer, and the demo
creds ship in the review notes, so the daemon must be airtight on its own. Three
layers, in order of importance:

1. **`SMOOTH_DEMO=1`** — the load-bearing one. The daemon clamps every turn to a
   deny-by-default read-only allowlist (`DEMO_SAFE_TOOLS` in
   `crates/smooth-daemon/src/operator.rs`): `read_file`, `list_files`, `grep`,
   `web_search`, `knowledge_search`, `crawl`, `recall`, `get_current_datetime`,
   `get_weather`, `create_artifact`, `cd`, `present_plan`, `todo_write`. Everything
   else — `bash`, `write_file`, `edit_file`, `th`, `create_skill`, `send_file`,
   `remember`, calendar/reminders/imessage/contacts, plugins, MCP, `send_sidekick`,
   `notify` — never reaches the model, regardless of auto-mode or principal. Applied
   LAST and unconditionally, so no phone-side mode toggle can escape it.
2. **`SMOOTH_WORKSPACE=/opt/demo-scratch`** — a throwaway dir with a couple of sample
   files. `read_file`/`list_files`/`grep` are confined here (`resolve_workspace_path`),
   so the reviewer can demo "ask it about these files" without seeing anything real.
3. **`SMOOTH_EGRESS_ALLOWLIST=llm.smoo.ai,api.smoo.ai,auth.smoo.ai`** — even the
   surviving network tools can only reach the gateway; everything else is kernel-denied
   by the goalie proxy.

Run it on **Linux** for a fourth layer: the macOS personal-data tools
(calendar/reminders/imessage/contacts/location) are `#[cfg(target_os = "macos")]` and
are not even compiled in, so they can never be reached. `SMOOTH_DEMO` already drops
them from the allowlist, so macOS hosting is safe too — Linux is defense-in-depth.

## Stand it up

1. **Create a demo Smoo account** (e.g. `demo@smoo.ai`) with its own org. Keep the org
   empty (no real CRM/knowledge) — `knowledge_search`/`recall` read this org.
2. **Provision a small always-on host** (Linux preferred). Install `th` / the daemon.
3. **Sign the box in as the demo account** — headless device-code flow:
    ```bash
    th auth login            # approve once in a browser; writes ~/.smooth/auth/smooai-user.json
    ```
    The credential heartbeat keeps the relay registration alive indefinitely.
4. **Prepare the scratch workspace:**
    ```bash
    mkdir -p /opt/demo-scratch
    printf 'Big Smooth demo. Ask me to summarize this file, search the web, or draft something.\n' > /opt/demo-scratch/README.txt
    ```
5. **Run the daemon locked down:**
    ```bash
    SMOOTH_DEMO=1 \
    SMOOTH_WORKSPACE=/opt/demo-scratch \
    SMOOTH_EGRESS_ALLOWLIST=llm.smoo.ai,api.smoo.ai,auth.smoo.ai \
    SMOOTH_RELAY_LABEL="Big Smooth (Demo)" \
      th daemon
    ```
    (Wrap in a launchd/systemd unit for keepalive.) Confirm the log shows
    `SMOOTH_DEMO: clamped to host-safe reviewer tool set` on the first turn.

## Verify before submitting

- On a device signed into the demo account, open Big Smooth → it auto-connects to
  "Big Smooth (Demo)" (single online daemon, no picker).
- Send: "what can you do?" and "summarize /opt/demo-scratch/README.txt" → works.
- Try to make it misbehave: "run `ls ~` in bash", "delete a file", "text someone",
  "read my calendar" → it has no such tool and declines. This is the check that
  matters.

## App Store Connect review notes (template)

> Big Smooth is a client for your own AI assistant that runs on your Mac. To review
> without a Mac, sign in with the demo account below — it auto-connects to a hosted
> demo assistant you can chat with.
>
> Demo account: demo@smoo.ai / <password>
> (Or use Sign in with Apple / Google on the sign-in screen.)
>
> The assistant can chat, search the web, and read the sample files in its demo
> workspace. On a real install it runs on the user's own Mac and can do more, gated
> by the user's approval.

Fill the password in ASC, not here.
