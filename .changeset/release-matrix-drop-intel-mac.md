---
"@smooai/smooth": patch
---

Release workflow: drop `x86_64-apple-darwin` from the build matrix
and set `fail-fast: false`.

Intel macOS has been blocking every release since microsandbox was
wired into smooth-bigsmooth: the upstream `msb_krun_utils` v0.1.9
crate references `kvm_bindings::kvm_irq_routing_entry` without a
`cfg(target_os = "linux")` guard, so it fails to compile on any
non-Linux target. On Apple Silicon the build never gets that far
because different HVF code paths are used, but Intel macOS hits the
wall every time.

Until upstream gates that type properly, we ship:

- `aarch64-apple-darwin`
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`

`fail-fast: false` also means a future single-platform regression
won't silently cancel sibling builds, so we can ship the remaining
targets while we fix the broken one.
