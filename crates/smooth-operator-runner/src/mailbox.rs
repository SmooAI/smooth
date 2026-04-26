//! Pearl-comment mailbox poller for the operator runner.
//!
//! Operators inside a sandbox don't have a direct WebSocket to Big Smooth — by
//! design, the only authoritative durable channel between the lead and a
//! running teammate is the pearl's comment list (audit-friendly, replayable,
//! visible in the pearls UI). This module wraps that store as a *mailbox*: a
//! background tokio task polls `PearlStore::get_comments(pearl_id)` every
//! ~1.5 s, parses each new comment by prefix, and emits typed messages onto a
//! channel that the agent loop drains at the top of each iteration.
//!
//! Prefix conventions (see plan `sorted-orbiting-hummingbird.md`):
//!
//! | Prefix                       | Meaning                                      |
//! |------------------------------|----------------------------------------------|
//! | `[CHAT:USER]`                | direct-chat user-turn                        |
//! | `[STEERING:GUIDANCE]`        | mid-flight nudge from the lead               |
//! | `[ANSWER:USER:q-{id}]`       | reply to an `ask_smooth` blocking question   |
//! | `[ANSWER:SMOOTH:q-{id}]`     | reply auto-answered by Big Smooth's chat     |
//!
//! Comments without a recognised prefix are ignored (they're often the
//! teammate's own `[CHAT:TEAMMATE]`, `[PROGRESS]`, `[QUESTION:TEAMMATE]`,
//! `[IDLE]` posts, which a teammate must not consume).
//!
//! The poller never blocks the agent: the channel is unbounded and the
//! agent uses `try_recv` on each iteration. If the poll fails (Dolt
//! transient error, store unavailable), it logs and tries again next tick.

use std::sync::Arc;
use std::time::Duration;

use smooth_operator::agent::{InjectedMessage, InjectedMessageKind};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::Mutex;

use crate::pearl_tools::PearlStoreHandle;

const POLL_INTERVAL: Duration = Duration::from_millis(1500);

/// Spawn the mailbox poller. Returns the receiver half wrapped for
/// `AgentConfig::with_chat_rx`. Caller is expected to keep the join handle
/// alive for the agent's lifetime; the poller exits when the sender is
/// dropped or the pearl store reports a fatal error.
#[must_use]
pub fn spawn_poller(handle: Arc<PearlStoreHandle>, pearl_id: String) -> (Arc<Mutex<UnboundedReceiver<InjectedMessage>>>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<InjectedMessage>();
    let join = tokio::spawn(poll_loop(handle, pearl_id, tx));
    (Arc::new(Mutex::new(rx)), join)
}

async fn poll_loop(handle: Arc<PearlStoreHandle>, pearl_id: String, tx: UnboundedSender<InjectedMessage>) {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut first = true;
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;
        if tx.is_closed() {
            tracing::debug!(pearl_id = %pearl_id, "mailbox: receiver dropped, exiting poll loop");
            return;
        }

        let comments = match handle.store.get_comments(&pearl_id) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, pearl_id = %pearl_id, "mailbox: get_comments failed");
                continue;
            }
        };

        for comment in comments {
            // First pass primes `seen` without emitting anything: messages
            // posted before the operator started are not user-turns it should
            // act on. (They're already part of the pearl's history; the lead's
            // dispatch prompt subsumes them.)
            if !seen.insert(comment.id.clone()) {
                continue;
            }
            if first {
                continue;
            }

            if let Some(msg) = parse_comment(&comment.content) {
                tracing::info!(pearl_id = %pearl_id, comment_id = %comment.id, kind = ?msg.kind, "mailbox: injecting");
                if tx.send(msg).is_err() {
                    return;
                }
            }
        }

        first = false;
    }
}

/// Parse a comment body and return an `InjectedMessage` if the prefix matches
/// one of the lead-to-teammate kinds. Returns `None` for prefixes the teammate
/// originated itself (`[CHAT:TEAMMATE]`, `[PROGRESS]`, `[QUESTION:TEAMMATE]`,
/// `[IDLE]`) or for arbitrary unprefixed comments.
pub fn parse_comment(body: &str) -> Option<InjectedMessage> {
    let trimmed = body.trim_start();
    if let Some(rest) = trimmed.strip_prefix("[CHAT:USER]") {
        return Some(InjectedMessage {
            kind: InjectedMessageKind::UserChat,
            body: rest.trim().to_string(),
        });
    }
    if let Some(rest) = trimmed.strip_prefix("[STEERING:GUIDANCE]") {
        return Some(InjectedMessage {
            kind: InjectedMessageKind::LeadGuidance,
            body: rest.trim().to_string(),
        });
    }
    if trimmed.starts_with("[ANSWER:USER") || trimmed.starts_with("[ANSWER:SMOOTH") {
        // strip past the first `]`
        if let Some(idx) = trimmed.find(']') {
            return Some(InjectedMessage {
                kind: InjectedMessageKind::AnswerToQuestion,
                body: trimmed[idx + 1..].trim().to_string(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chat_user() {
        let m = parse_comment("[CHAT:USER] please switch to staging").unwrap();
        assert_eq!(m.kind, InjectedMessageKind::UserChat);
        assert_eq!(m.body, "please switch to staging");
    }

    #[test]
    fn parses_lead_guidance() {
        let m = parse_comment("[STEERING:GUIDANCE] focus on the auth module").unwrap();
        assert_eq!(m.kind, InjectedMessageKind::LeadGuidance);
        assert_eq!(m.body, "focus on the auth module");
    }

    #[test]
    fn parses_answer_user_with_id() {
        let m = parse_comment("[ANSWER:USER:q-abc123] use port 4400").unwrap();
        assert_eq!(m.kind, InjectedMessageKind::AnswerToQuestion);
        assert_eq!(m.body, "use port 4400");
    }

    #[test]
    fn parses_answer_smooth_with_id() {
        let m = parse_comment("[ANSWER:SMOOTH:q-xyz] the lint command is `pnpm lint`").unwrap();
        assert_eq!(m.kind, InjectedMessageKind::AnswerToQuestion);
        assert_eq!(m.body, "the lint command is `pnpm lint`");
    }

    #[test]
    fn ignores_teammate_originated_prefixes() {
        assert!(parse_comment("[CHAT:TEAMMATE] working on it").is_none());
        assert!(parse_comment("[PROGRESS] step 2 of 5").is_none());
        assert!(parse_comment("[QUESTION:TEAMMATE:q-1] should I bump deps?").is_none());
        assert!(parse_comment("[IDLE]").is_none());
    }

    #[test]
    fn ignores_unprefixed_comments() {
        assert!(parse_comment("just a regular comment").is_none());
        assert!(parse_comment("").is_none());
    }

    #[test]
    fn tolerates_leading_whitespace() {
        let m = parse_comment("   [CHAT:USER] hi").unwrap();
        assert_eq!(m.body, "hi");
    }
}
