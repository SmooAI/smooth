//! The daemon `/health` probe — one implementation, every caller.
//!
//! Big Smooth serves `GET /health` from smooth-operator's `LocalServer`, whose
//! handler is `async fn health() -> &'static str { "ok" }`. It answers with the
//! plain string `ok`, **never** JSON. `th status` called `.json()` on that and
//! reported `error decoding response body` against a perfectly healthy daemon
//! (pearl th-b39484), while three other commands each rolled their own probe
//! with their own idea of what a non-200 means.
//!
//! So: parse the body opportunistically — a JSON object lights up the extra
//! detail lines, anything else just means "the listener is up" — and give every
//! caller the same two-line failure wording, because `th status` is what you run
//! when something is already wrong.

use std::time::Duration;

/// Big Smooth's default API port — matches `th up --port`.
pub const DEFAULT_PORT: u16 = 4400;

/// How long to wait before calling the daemon unreachable.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The `/health` URL for a given port.
#[must_use]
pub fn health_url(port: u16) -> String {
    format!("http://localhost:{port}/health")
}

/// What a probe found.
#[derive(Debug, Clone, PartialEq)]
pub enum Health {
    /// The daemon answered 2xx. `details` is `Some` only when the body was a
    /// JSON object — today's daemon sends plain `ok`, so it is normally `None`.
    Up { details: Option<serde_json::Value> },
    /// Something answered on the port, but not with success — almost always a
    /// different process squatting it.
    Foreign { status: u16 },
    /// Nothing answered.
    Down { reason: String },
}

impl Health {
    #[must_use]
    pub fn is_up(&self) -> bool {
        matches!(self, Self::Up { .. })
    }

    /// Classify a response we actually got back. Pure, so the tests drive it
    /// directly instead of standing up a server.
    #[must_use]
    pub fn classify(status: u16, body: &str) -> Self {
        if (200..300).contains(&status) {
            Self::Up {
                details: serde_json::from_str::<serde_json::Value>(body).ok().filter(serde_json::Value::is_object),
            }
        } else {
            Self::Foreign { status }
        }
    }

    /// Two lines for an unusable daemon: what is wrong, then what to do about
    /// it. `None` when the daemon is up. Unstyled — the caller owns the icon
    /// and the wordmark.
    #[must_use]
    pub fn failure_lines(&self, port: u16) -> Option<[String; 2]> {
        let url = health_url(port);
        match self {
            Self::Up { .. } => None,
            Self::Down { reason } => Some([
                format!("not running \u{2014} nothing listening at {url} ({reason})"),
                "Start it with: th up".to_string(),
            ]),
            Self::Foreign { status } => Some([
                format!("unreachable \u{2014} {url} answered HTTP {status}, so that port isn't Big Smooth"),
                format!("Check ~/.smooth/smooth.log, or move the daemon: th up --port <port> (default {DEFAULT_PORT})"),
            ]),
        }
    }
}

/// Probe the daemon. Never errors — an unreachable daemon is a result, not a
/// failure, and it is the single most likely thing the user is asking about.
pub async fn probe(port: u16) -> Health {
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => return Health::Down { reason: e.to_string() },
    };
    match client.get(health_url(port)).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            // A body we can't read is still a 200 — treat it as plain text.
            let body = resp.text().await.unwrap_or_default();
            Health::classify(status, &body)
        }
        Err(e) => Health::Down { reason: down_reason(&e) },
    }
}

/// Reduce a reqwest error to something a human can act on. The full Display is
/// a nest of source errors that tells the user nothing they can use.
fn down_reason(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        format!("no answer in {}s", PROBE_TIMEOUT.as_secs())
    } else if e.is_connect() {
        "connection refused".to_string()
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_ok_is_up_with_no_details() {
        // What the daemon actually serves — the bug this module exists for.
        assert_eq!(Health::classify(200, "ok"), Health::Up { details: None });
    }

    #[test]
    fn empty_body_is_up_with_no_details() {
        assert_eq!(Health::classify(200, ""), Health::Up { details: None });
    }

    #[test]
    fn json_body_is_up_with_details() {
        let Health::Up { details } = Health::classify(200, r#"{"version":"1.2.3","uptime_seconds":90}"#) else {
            panic!("JSON health body should classify as Up");
        };
        let details = details.expect("JSON object should be captured as details");
        assert_eq!(details["version"], "1.2.3");
        assert_eq!(details["uptime_seconds"], 90);
    }

    #[test]
    fn non_object_json_is_not_details() {
        // `"ok"` and `123` are valid JSON but carry no fields to render.
        assert_eq!(Health::classify(200, "\"ok\""), Health::Up { details: None });
        assert_eq!(Health::classify(200, "123"), Health::Up { details: None });
    }

    #[test]
    fn non_success_status_is_foreign() {
        assert_eq!(Health::classify(404, "Not Found"), Health::Foreign { status: 404 });
        assert_eq!(Health::classify(503, "ok"), Health::Foreign { status: 503 });
        assert!(!Health::classify(500, "").is_up());
    }

    #[test]
    fn success_range_is_inclusive_of_204() {
        assert!(Health::classify(204, "").is_up());
        assert!(!Health::classify(302, "").is_up());
    }

    #[test]
    fn down_says_what_and_what_to_do() {
        let health = Health::Down {
            reason: "connection refused".to_string(),
        };
        let [what, fix] = health.failure_lines(4400).expect("down has failure lines");
        assert!(what.contains("not running"), "{what}");
        assert!(what.contains("http://localhost:4400/health"), "{what}");
        assert!(what.contains("connection refused"), "{what}");
        assert!(fix.contains("th up"), "{fix}");
    }

    #[test]
    fn foreign_names_the_status_and_the_port_fix() {
        let [what, fix] = Health::Foreign { status: 404 }.failure_lines(9999).expect("foreign has failure lines");
        assert!(what.contains("HTTP 404"), "{what}");
        assert!(what.contains("http://localhost:9999/health"), "{what}");
        assert!(fix.contains("--port"), "{fix}");
    }

    #[test]
    fn up_has_no_failure_lines() {
        assert!(Health::classify(200, "ok").failure_lines(DEFAULT_PORT).is_none());
    }

    #[test]
    fn health_url_uses_the_given_port() {
        assert_eq!(health_url(DEFAULT_PORT), "http://localhost:4400/health");
        assert_eq!(health_url(8788), "http://localhost:8788/health");
    }

    #[tokio::test]
    async fn probe_of_a_dead_port_is_down_not_an_error() {
        // Port 1 needs root to bind, so nothing local is listening on it.
        let health = probe(1).await;
        assert!(matches!(health, Health::Down { .. }), "{health:?}");
        assert!(health.failure_lines(1).is_some());
    }
}
