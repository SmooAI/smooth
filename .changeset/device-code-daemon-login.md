---
"@smooai/smooth": minor
---

Big Smooth daemon: sign in to your Smoo org from the tailnet chat UI via the
OAuth 2.0 Device Authorization Grant (RFC 8628, th-ea7b54).

The existing browser redirect flow (`/auth/login` → `/auth/callback`) needs a
`redirect_uri` smoo.ai allowlists, which the tailnet host isn't. The device
flow needs none: the UI POSTs `/auth/device/start`, the daemon fetches a
device+user code from `smoo.ai/api/device/code` (public client
`bigsmooth-daemon`), shows the user the code + approval link, and polls the
token endpoint in the background until approval — then persists the user
session to `~/.smooth/auth/smooai-user.json`. The existing `/api/auth/status`
poll flips the UI to logged-in automatically; the `device_code` never reaches
the browser. The redirect flow stays for loopback/localhost. Endpoints +
client id are env-overridable (`SMOOAI_CLI_DEVICE_URL`,
`SMOOAI_DEVICE_CLIENT_ID`).
