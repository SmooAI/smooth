---
'@smooai/smooth': patch
---

The Claude Desktop bundle (`packaging/mcpb`) lists every tool `th mcp serve` exposes. That's 42 tools, the SmoothFlow ones included; it used to list 6. A test pins the list to the server, so a new tool fails CI until the manifest is re-blessed (`SMOOTH_MCPB_BLESS=1 cargo test -p smooai-smooth-cli mcpb_manifest`). `build-mcpb.sh` stamps th's version into the bundle instead of the frozen 0.22.0. `th mcp install --help` now names `claude-desktop`.
