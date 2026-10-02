//! Finding the flow engine (Client Spec §2): `$SMOOTH_FLOW_ADDR`, then
//! `~/.smooth/flow.addr`, then `~/.smooth/daemon.addr`; the token from
//! `$SMOOTHFLOW_DAEMON_TOKEN`, `$SMOOTH_LOCAL_TOKEN`, then
//! `~/.smooth/operator-token`.

use std::path::Path;

/// Where to connect: `host:port` plus the local token, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub addr: String,
    pub token: Option<String>,
}

impl Endpoint {
    /// The flow WebSocket URL, with the token as `?token=`.
    #[must_use]
    pub fn ws_url(&self) -> String {
        let q = self.token.as_deref().map(|t| format!("?token={}", urlencoding::encode(t))).unwrap_or_default();
        format!("ws://{}/api/flow/ws{q}", self.addr)
    }
}

fn first_nonblank(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    values.into_iter().flatten().map(|v| v.trim().to_string()).find(|v| !v.is_empty())
}

/// The endpoint from explicit inputs. Pure, for tests.
#[must_use]
pub fn resolve(
    env_addr: Option<String>,
    flow_addr: Option<String>,
    daemon_addr: Option<String>,
    env_tokens: [Option<String>; 2],
    token_file: Option<String>,
) -> Option<Endpoint> {
    let addr = first_nonblank([env_addr, flow_addr, daemon_addr])?;
    let addr = addr.trim_start_matches("http://").trim_end_matches('/').to_string();
    let [a, b] = env_tokens;
    Some(Endpoint {
        addr,
        token: first_nonblank([a, b, token_file]),
    })
}

/// The endpoint for this machine, reading the environment and `smooth_dir`
/// (`~/.smooth`).
#[must_use]
pub fn discover(smooth_dir: &Path) -> Option<Endpoint> {
    discover_in(smooth_dir, |name| std::env::var(name).ok())
}

/// [`discover`] with the environment supplied by `env` (`|_| None` = an
/// empty environment, so only the files in `smooth_dir` count).
#[must_use]
pub fn discover_in(smooth_dir: &Path, env: impl Fn(&str) -> Option<String>) -> Option<Endpoint> {
    let read = |name: &str| std::fs::read_to_string(smooth_dir.join(name)).ok();
    resolve(
        env("SMOOTH_FLOW_ADDR"),
        read("flow.addr"),
        read("daemon.addr"),
        [env("SMOOTHFLOW_DAEMON_TOKEN"), env("SMOOTH_LOCAL_TOKEN")],
        read("operator-token"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn flow_addr_then_daemon_addr_env_first() {
        let e = resolve(None, s("127.0.0.1:5\n"), s("127.0.0.1:9"), [None, None], s("tok\n"));
        assert_eq!(
            e,
            Some(Endpoint {
                addr: "127.0.0.1:5".into(),
                token: s("tok")
            })
        );
        let e = resolve(s("http://h:1/"), s("127.0.0.1:5"), None, [s("env-tok"), None], s("file"));
        assert_eq!(
            e,
            Some(Endpoint {
                addr: "h:1".into(),
                token: s("env-tok")
            })
        );
        assert_eq!(
            resolve(None, s(" "), s("127.0.0.1:9"), [None, None], None).map(|e| e.addr).as_deref(),
            Some("127.0.0.1:9")
        );
        assert_eq!(resolve(None, None, None, [None, None], s("t")), None);
    }

    #[test]
    fn ws_url_carries_the_token_encoded() {
        let e = Endpoint {
            addr: "127.0.0.1:5".into(),
            token: s("a b"),
        };
        assert_eq!(e.ws_url(), "ws://127.0.0.1:5/api/flow/ws?token=a%20b");
        assert_eq!(
            Endpoint {
                addr: "h:1".into(),
                token: None
            }
            .ws_url(),
            "ws://h:1/api/flow/ws"
        );
    }
}
