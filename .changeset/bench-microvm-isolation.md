---
'@smooai/smooth': patch
---

`smooth-bench score --isolation microvm` — run each scored task's engine inside a microsandbox microVM instead of as a host process (pearl th-a63c22).

The new `MicroVmBooter` is a second `EngineBooter` impl: per task it allocates a free host port, boots the linux `smooth-daemon` in a uniquely-named `msb` sandbox with the task's scratch dir bind-mounted at `/work`, denies egress by default except the LLM gateway, injects the gateway key as a host-scoped `--secret`, and removes the sandbox on drop so no VMs leak. The linux daemon binary is container-built once (cached in docker volumes — never touching the host `~/.cargo` or `./target`).

`--isolation` defaults to `host`, preserving today's behaviour, and rejects `microvm` for any engine but `rust` (the polyglot engines have no VM-bootable binary and ship no tools — pearl th-82ad57).

Two integration bugs fixed along the way: readiness is now probed at the HTTP layer, because msb's host-side port forwarder accepts TCP before the guest binds (a TCP probe returned instantly and every turn died with "Handshake not finished"); and the model pin is passed as `SMOOTH_AGENT_MODEL` as well as `SMOOAI_MODEL`, because the daemon only reads the former and was silently running the upstream default model. Because attached `msb run` pipes no guest output, the daemon's stdout is redirected into a bind-mounted log dir so failed tasks stay debuggable.
