//! SmoothFlow engine e2e — the cmux/orca-style suite (pearl th-8e3087).
//!
//! Every test boots a REAL `smooth-daemon` (its own HOME, port, tmux server;
//! see `support`) and drives it exactly the way the apps, the phones, `th
//! flow` and a harness's hook script do: the flow WS, the HTTP siblings, the
//! `th` binary, `POST /api/flow/hooks`. The agent under test is `fake-agent`
//! (`tests/fixtures/fake-agent`), installed through a harness manifest like
//! any other coding CLI, in four flavours: hooks / learned-id / native /
//! scrape — one per state source the engine supports.
//!
//! Every scenario runs on both session hosts (th-dc9822): `<file>::tmux::<name>`
//! on tmux, and `<file>::pty::<name>` on the engine-owned PTY host (ADR-011)
//! when the daemon is built with `--features pty-host`. `restart` holds the
//! pty-only daemon-restart scenarios.
//!
//! Skips (or fails, with `SMOOTH_E2E_STRICT=1`) when tmux (tmux host only),
//! bash, curl or the `th` binary is missing. Run it alone with
//! `cargo nextest run -p smooai-smooth-daemon --test flow_e2e`; the full
//! contract, the runtime budget and the CI split are in
//! docs/Engineering/SmoothFlow-Testing.md.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    reason = "unwrap/expect are the idiom for test assertions; a scenario is one long test on purpose"
)]

/// Run each scenario (`async fn name(host: Host)`) on both session hosts:
/// `tmux::name`, and `pty::name` when the daemon has the `pty-host` feature.
macro_rules! on_both_hosts {
    ($($name:ident),+ $(,)?) => {
        mod tmux {
            $(
                #[tokio::test]
                async fn $name() {
                    super::$name(crate::support::Host::Tmux).await;
                }
            )+
        }
        #[cfg(feature = "pty-host")]
        mod pty {
            $(
                #[tokio::test]
                async fn $name() {
                    super::$name(crate::support::Host::Pty).await;
                }
            )+
        }
    };
}
pub(crate) use on_both_hosts;

mod support;

mod agent;
mod cli;
mod diff;
mod finder_env;
mod harnesses;
mod hooks;
mod isolation;
#[cfg(feature = "pty-host")]
mod restart;
mod shell;
