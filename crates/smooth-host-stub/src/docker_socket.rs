//! Host-side Docker socket auto-detection.
//!
//! Pearl th-893801 Phase 2 iter-4e. `th up` calls
//! [`detect`] to figure out where to bind-mount the host's
//! Docker socket so in-sandbox `docker` commands work
//! transparently regardless of which container runtime the
//! user installed (Colima, OrbStack, Rancher Desktop,
//! Podman, vanilla Docker Desktop).
//!
//! Probe order (first non-empty wins):
//!
//! 1. `DOCKER_HOST` env var — when set to a `unix://`
//!    scheme, the path is resolved. (`tcp://` is rejected
//!    with a clear error; we'd need TCP plumbing inside the
//!    sandbox.)
//! 2. Colima: `$HOME/.colima/default/docker.sock`.
//! 3. OrbStack: `$HOME/.orbstack/run/docker.sock`.
//! 4. Rancher Desktop: `$HOME/.rd/docker.sock`.
//! 5. Podman (rootless): `$XDG_RUNTIME_DIR/podman/podman.sock`.
//! 6. Default: `/var/run/docker.sock`.
//!
//! The probe is filesystem-only — no `docker ps` shellout —
//! so it's safe to call from synchronous startup code.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Description of a detected socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetectedSocket {
    /// Resolved filesystem path of the socket.
    pub path: PathBuf,
    /// Which probe matched — surfaced by `th up` so the
    /// user sees "using Colima at …".
    pub runtime: DockerRuntime,
}

/// Probe identifier — surfaced to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DockerRuntime {
    DockerHostEnv,
    Colima,
    OrbStack,
    RancherDesktop,
    Podman,
    DockerDesktop,
}

impl DockerRuntime {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DockerHostEnv => "DOCKER_HOST env",
            Self::Colima => "Colima",
            Self::OrbStack => "OrbStack",
            Self::RancherDesktop => "Rancher Desktop",
            Self::Podman => "Podman",
            Self::DockerDesktop => "Docker Desktop",
        }
    }
}

/// Errors returned by [`detect`].
#[derive(Debug, thiserror::Error)]
pub enum DetectError {
    #[error("DOCKER_HOST={0:?} uses an unsupported scheme; only unix:// is bind-mountable")]
    UnsupportedDockerHost(String),
    #[error("DOCKER_HOST={0:?} points at {1}, which does not exist")]
    DockerHostMissing(String, PathBuf),
    #[error("no Docker socket found; tried Colima, OrbStack, Rancher, Podman, and /var/run/docker.sock")]
    NotFound,
}

/// Trait for filesystem access. The default
/// [`HostFsProbe`] hits the real filesystem; tests provide a
/// stub with canned `exists` answers.
pub trait FsProbe: Send + Sync {
    fn exists(&self, path: &Path) -> bool;
    fn env_var(&self, key: &str) -> Option<String>;
    fn home_dir(&self) -> Option<PathBuf>;
}

/// Production `FsProbe` reading real env + filesystem.
pub struct HostFsProbe;

impl FsProbe for HostFsProbe {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn env_var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.trim().is_empty())
    }

    fn home_dir(&self) -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// Probe the host for a Docker-compatible socket and return
/// the first match. See module-level docs for probe order.
///
/// # Errors
///
/// * `UnsupportedDockerHost` — `DOCKER_HOST` is set but uses
///   a non-unix scheme.
/// * `DockerHostMissing` — `DOCKER_HOST=unix://…` set but
///   the referenced file doesn't exist.
/// * `NotFound` — none of the fallback paths exist.
pub fn detect() -> Result<DetectedSocket, DetectError> {
    detect_with(&HostFsProbe)
}

/// Probe variant taking an explicit `FsProbe`. Used by tests.
///
/// # Errors
///
/// Same as [`detect`].
pub fn detect_with<P: FsProbe>(probe: &P) -> Result<DetectedSocket, DetectError> {
    // 1. DOCKER_HOST env var.
    if let Some(raw) = probe.env_var("DOCKER_HOST") {
        let raw_trimmed = raw.trim().to_string();
        if let Some(path_str) = raw_trimmed.strip_prefix("unix://") {
            let path = PathBuf::from(path_str);
            if probe.exists(&path) {
                return Ok(DetectedSocket {
                    path,
                    runtime: DockerRuntime::DockerHostEnv,
                });
            }
            return Err(DetectError::DockerHostMissing(raw_trimmed, path));
        }
        // tcp://, npipe://, http+unix:// — none bind-mount cleanly.
        return Err(DetectError::UnsupportedDockerHost(raw_trimmed));
    }

    // 2-5. Per-runtime well-known paths under $HOME.
    if let Some(home) = probe.home_dir() {
        let candidates: &[(DockerRuntime, PathBuf)] = &[
            (DockerRuntime::Colima, home.join(".colima/default/docker.sock")),
            (DockerRuntime::OrbStack, home.join(".orbstack/run/docker.sock")),
            (DockerRuntime::RancherDesktop, home.join(".rd/docker.sock")),
        ];
        for (runtime, path) in candidates {
            if probe.exists(path) {
                return Ok(DetectedSocket {
                    path: path.clone(),
                    runtime: *runtime,
                });
            }
        }
    }

    // Podman rootless under XDG_RUNTIME_DIR.
    if let Some(xdg) = probe.env_var("XDG_RUNTIME_DIR") {
        let path = PathBuf::from(xdg).join("podman/podman.sock");
        if probe.exists(&path) {
            return Ok(DetectedSocket {
                path,
                runtime: DockerRuntime::Podman,
            });
        }
    }

    // 6. Docker Desktop default.
    let default = PathBuf::from("/var/run/docker.sock");
    if probe.exists(&default) {
        return Ok(DetectedSocket {
            path: default,
            runtime: DockerRuntime::DockerDesktop,
        });
    }

    Err(DetectError::NotFound)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    /// Stub probe — tests pre-populate sets of "this exists"
    /// paths and env vars.
    struct StubProbe {
        files: Mutex<HashSet<PathBuf>>,
        env: Mutex<HashMap<String, String>>,
        home: Option<PathBuf>,
    }

    impl StubProbe {
        fn new() -> Self {
            Self {
                files: Mutex::new(HashSet::new()),
                env: Mutex::new(HashMap::new()),
                home: Some(PathBuf::from("/Users/test")),
            }
        }

        fn with_file(self, path: impl Into<PathBuf>) -> Self {
            self.files.lock().unwrap().insert(path.into());
            self
        }

        fn with_env(self, key: &str, value: &str) -> Self {
            self.env.lock().unwrap().insert(key.into(), value.into());
            self
        }

        fn with_no_home(mut self) -> Self {
            self.home = None;
            self
        }
    }

    impl FsProbe for StubProbe {
        fn exists(&self, path: &Path) -> bool {
            self.files.lock().unwrap().contains(path)
        }

        fn env_var(&self, key: &str) -> Option<String> {
            self.env.lock().unwrap().get(key).cloned()
        }

        fn home_dir(&self) -> Option<PathBuf> {
            self.home.clone()
        }
    }

    #[test]
    fn docker_host_env_unix_scheme_wins() {
        let probe = StubProbe::new()
            .with_env("DOCKER_HOST", "unix:///tmp/my.sock")
            .with_file("/tmp/my.sock")
            .with_file("/Users/test/.colima/default/docker.sock");
        let sock = detect_with(&probe).unwrap();
        assert_eq!(sock.path, PathBuf::from("/tmp/my.sock"));
        assert_eq!(sock.runtime, DockerRuntime::DockerHostEnv);
    }

    #[test]
    fn docker_host_tcp_is_unsupported() {
        let probe = StubProbe::new().with_env("DOCKER_HOST", "tcp://127.0.0.1:2375");
        let err = detect_with(&probe).unwrap_err();
        assert!(matches!(err, DetectError::UnsupportedDockerHost(_)));
    }

    #[test]
    fn docker_host_unix_but_missing_errors() {
        let probe = StubProbe::new().with_env("DOCKER_HOST", "unix:///tmp/missing.sock");
        let err = detect_with(&probe).unwrap_err();
        match err {
            DetectError::DockerHostMissing(raw, path) => {
                assert_eq!(raw, "unix:///tmp/missing.sock");
                assert_eq!(path, PathBuf::from("/tmp/missing.sock"));
            }
            other => panic!("expected DockerHostMissing, got {other:?}"),
        }
    }

    #[test]
    fn colima_path_detected() {
        let probe = StubProbe::new().with_file("/Users/test/.colima/default/docker.sock");
        let sock = detect_with(&probe).unwrap();
        assert_eq!(sock.runtime, DockerRuntime::Colima);
        assert_eq!(sock.path, PathBuf::from("/Users/test/.colima/default/docker.sock"));
    }

    #[test]
    fn orbstack_path_detected_when_colima_missing() {
        let probe = StubProbe::new().with_file("/Users/test/.orbstack/run/docker.sock");
        let sock = detect_with(&probe).unwrap();
        assert_eq!(sock.runtime, DockerRuntime::OrbStack);
    }

    #[test]
    fn rancher_detected_when_neither_colima_nor_orbstack() {
        let probe = StubProbe::new().with_file("/Users/test/.rd/docker.sock");
        let sock = detect_with(&probe).unwrap();
        assert_eq!(sock.runtime, DockerRuntime::RancherDesktop);
    }

    #[test]
    fn colima_wins_over_orbstack_when_both_present() {
        let probe = StubProbe::new()
            .with_file("/Users/test/.colima/default/docker.sock")
            .with_file("/Users/test/.orbstack/run/docker.sock");
        let sock = detect_with(&probe).unwrap();
        assert_eq!(sock.runtime, DockerRuntime::Colima);
    }

    #[test]
    fn podman_detected_via_xdg_runtime_dir() {
        let probe = StubProbe::new()
            .with_no_home()
            .with_env("XDG_RUNTIME_DIR", "/run/user/1000")
            .with_file("/run/user/1000/podman/podman.sock");
        let sock = detect_with(&probe).unwrap();
        assert_eq!(sock.runtime, DockerRuntime::Podman);
        assert_eq!(sock.path, PathBuf::from("/run/user/1000/podman/podman.sock"));
    }

    #[test]
    fn docker_desktop_default_path_is_last_resort() {
        let probe = StubProbe::new().with_no_home().with_file("/var/run/docker.sock");
        let sock = detect_with(&probe).unwrap();
        assert_eq!(sock.runtime, DockerRuntime::DockerDesktop);
        assert_eq!(sock.path, PathBuf::from("/var/run/docker.sock"));
    }

    #[test]
    fn nothing_found_returns_not_found() {
        let probe = StubProbe::new().with_no_home();
        let err = detect_with(&probe).unwrap_err();
        assert!(matches!(err, DetectError::NotFound));
    }

    #[test]
    fn docker_runtime_labels_render() {
        assert_eq!(DockerRuntime::DockerHostEnv.as_str(), "DOCKER_HOST env");
        assert_eq!(DockerRuntime::Colima.as_str(), "Colima");
        assert_eq!(DockerRuntime::OrbStack.as_str(), "OrbStack");
        assert_eq!(DockerRuntime::RancherDesktop.as_str(), "Rancher Desktop");
        assert_eq!(DockerRuntime::Podman.as_str(), "Podman");
        assert_eq!(DockerRuntime::DockerDesktop.as_str(), "Docker Desktop");
    }
}
