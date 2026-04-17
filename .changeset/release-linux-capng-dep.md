---
"@smooai/smooth": patch
---

Release workflow: add `libcap-ng-dev` to the Linux runner deps.

After `libdbus-1-dev` unblocked compilation, the link step failed with
`rust-lld: error: unable to find library -lcap-ng` on both Linux
targets. `microsandbox`'s Linux-only `msb_krun_devices` uses libcap-ng
for CAP_* capability management in the VM host shim, so the headers
need to be present at link time.
