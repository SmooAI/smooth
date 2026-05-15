# smooth-host-stub

Credential broker bridging the single sandbox VM to host CLIs.

Pearl th-893801 Phase 2 iter-4a.

Runs on the macOS host. The sandbox sees a UDS bind-mounted at
`/run/smooth/host.sock` and dials this server when an in-VM
tool needs a credential.
