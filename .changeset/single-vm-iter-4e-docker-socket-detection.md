---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 2 iter-4e. `smooth_host_stub::docker_socket`
auto-detects the host's Docker-compatible socket so `th up`
can bind-mount it into the sandbox transparently regardless
of which container runtime the user installed.

Probe order (first match wins):

1. `DOCKER_HOST` env (`unix://` scheme only — `tcp://` is
   rejected with a clear error since it can't be bind-mounted).
2. Colima: `$HOME/.colima/default/docker.sock`.
3. OrbStack: `$HOME/.orbstack/run/docker.sock`.
4. Rancher Desktop: `$HOME/.rd/docker.sock`.
5. Podman (rootless): `$XDG_RUNTIME_DIR/podman/podman.sock`.
6. Docker Desktop default: `/var/run/docker.sock`.

The probe is filesystem-only (no `docker ps` shellout) so it
runs synchronously at startup. Returns a `DetectedSocket`
with the resolved path and a `DockerRuntime` label `th up`
surfaces ("using Colima at …").

`FsProbe` trait abstracts filesystem + env access; tests
inject a `StubProbe` with canned `exists` / `env_var` /
`home_dir` answers, no /tmp scribbling needed.

11 new tests cover every probe branch: DOCKER_HOST happy
path / tcp rejection / unix-missing error; each runtime path
in isolation; ordering preference between Colima and
OrbStack; Podman via XDG_RUNTIME_DIR; Docker Desktop last
resort; total miss → `NotFound`; label rendering.
