---
'@smooai/smooth': minor
---

Big Smooth daemon: add the `send_file` tool — the agent can deliver a workspace file to the user as a download. It reads a workspace-confined path, base64s it into a `data:` URL, and writes a `send_file` directive (`{type, files:[{name, mimeType, url}]}`) onto the turn's directive sink, which the engine drains onto `eventual_response.directive` for the faces to render. 10 MB cap, multiple sends accumulate into one directive, path-confined like read/write. No engine change (the directive sink already exists at the pinned engine). The SEND half of bidirectional file transfer (EPIC th-2e39fe).
