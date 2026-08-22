---
'@smooai/smooth': patch
---

Daemon session refresh no longer revokes its own Smoo session (th-c6a542).

The daemon had **two** user-session refreshers — the relay client and the
credential heartbeat — and both ran `load → refresh → save` **without a lock**,
racing each other and any concurrent `th` process. Supabase rotates the refresh
token on every exchange and trips reuse-detection when a rotated token is
re-presented, revoking the whole token family until a full `th auth login`. In
the field this dropped smoo-hub off the relay roughly hourly (`refresh: could not
refresh the Smoo session … refresh_token_not_found`), which in turn made the
mobile client — it reaches Big Smooth through the relay — silently diverge from
desktop.

Both refreshers now go through `refresh_user_session_locked`, which takes the
shared cross-process `credential_lock` (the path-keyed lock th-5c0189 added, so
`th`, the relay, and the heartbeat all contend for one sidecar), **re-reads the
credentials under the lock** so the loser of the race adopts the winner's freshly
rotated token instead of spending its own, and only then refreshes + persists.
th-5c0189 had fixed the `th`/smooth-api-client store but explicitly left the
`smooai-client-shared` store the daemon uses unlocked; this closes that gap.
