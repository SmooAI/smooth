---
'@smooai/smooth': minor
---

SEP Phase 6 (smooth side) — relay a dispatched operative's extension `ui/*`
requests to smooth-web.

A dispatched operative runs headless, so its new `HttpUiProvider` relays each
`ui/*` request to Big Smooth over the existing `SMOOTH_NARC_URL` +
`SMOOTH_HOST_TOKEN` callback channel (`POST /api/ui/request` — the same channel
`host_tool` uses). Big Smooth broadcasts a `UiRequest` server event to connected
frontends and, for the interactive kinds (`select`/`confirm`/`input`), blocks the
operative's call until a browser answers via `POST /api/ui/answer`. The new
smooth-web `UiRelay` component renders `select`/`confirm`/`input` as a modal,
`notify` as a toast, and `set_status`/`set_widget`/`set_title` in the chrome;
render blocks (`markdown`/`keyvalue`/`progress`/`table`/`diff`/`stack`, each with
a `text` fallback) render natively.

Unattended (no client connected) an interactive request resolves to
`{cancelled:true}` rather than hang; under `SMOOTH_AUTO_MODE=bypass` a `confirm`
is auto-answered `{confirmed:true}` (audited); otherwise it waits up to
`SMOOTH_UI_TIMEOUT_SECS` (default 120s) then cancels.

Also removes the dangling `smooth-plugin` workspace dependency entry (the crate
was deleted in Phase 5).
