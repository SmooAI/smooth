---
'@smooai/smooth': patch
---

`th ci-queue shim install` now shims `swift` and `xcrun` too (th-cb3c66).

On 2026-09-30, at load 300–900, 32 `swift-frontend` processes were running outside the build queue. They came from agents' `swift test` / `swift build` on SwiftPM packages, which no shim covered, and from `xcrun xcodebuild …`, which runs Xcode's binary by absolute path past the `xcodebuild` shim.

- `swift build`, `swift test`, `swift run` and a bare script queue. The REPL, `swift package …`, `swift format`, `swift sdk` and `--version` don't.
- `xcrun` queues only `xcodebuild` and `swift`, judged by that tool's own light list, so `xcrun xcodebuild -list` doesn't queue. Everything else it runs (`simctl`, `xcresulttool`, `--show-sdk-path`) never queues. It skips `--sdk` / `--toolchain` values to find the tool.
- Run `th ci-queue shim install` after upgrading to add the new shims.
