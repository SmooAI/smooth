---
'@smooai/smooth': patch
---

Big Smooth: new `get_location` tool — the Mac's real position from macOS Location Services (CoreLocation), instead of guessing from the IP address. macOS-only, takes no arguments, and named `get_*` so Auto mode treats it as a read and never prompts. `get_weather` now prefers this fix and falls back to the old IP lookup only when Location access is missing — so a VPN no longer moves your weather to another country. Ungranted, the tool answers with how to grant it (and opens the macOS prompt once per session) rather than pretending it has no idea where you are.
