//! WebSocket connection resiliency — shared logic for `BigSmoothClient` and
//! `OperatorClient`.
//!
//! Provides exponential backoff with jitter, connection state tracking, and an
//! outbound message buffer for messages sent while disconnected.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------------------
// ConnectionState
// ---------------------------------------------------------------------------

/// Possible states of a WebSocket connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected = 0,
    Connecting = 1,
    Connected = 2,
    Reconnecting = 3,
}

impl ConnectionState {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Connecting,
            2 => Self::Connected,
            3 => Self::Reconnecting,
            _ => Self::Disconnected,
        }
    }
}

// ---------------------------------------------------------------------------
// ResiliencyConfig
// ---------------------------------------------------------------------------

/// Tunable knobs for the reconnection / heartbeat strategy.
#[derive(Debug, Clone)]
pub struct ResiliencyConfig {
    /// How often to send a Ping (default: 15 s).
    pub heartbeat_interval: Duration,
    /// How long to wait for a Pong before considering the link dead (default: 5 s).
    pub heartbeat_timeout: Duration,
    /// Initial backoff delay (default: 1 s).
    pub base_backoff: Duration,
    /// Upper bound for the backoff delay (default: 30 s).
    pub max_backoff: Duration,
    /// Give up after this many reconnect attempts.  `None` = unlimited.
    pub max_reconnect_attempts: Option<u32>,
    /// Random jitter as a percentage of the computed backoff (default: 20).
    pub jitter_percent: u8,
    /// Maximum number of outbound messages to buffer while disconnected
    /// (default: 256).
    pub message_buffer_size: usize,
}

impl Default for ResiliencyConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(15),
            heartbeat_timeout: Duration::from_secs(5),
            base_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            max_reconnect_attempts: None,
            jitter_percent: 20,
            message_buffer_size: 256,
        }
    }
}

// ---------------------------------------------------------------------------
// ConnectionManager
// ---------------------------------------------------------------------------

/// Tracks WebSocket connection state and computes reconnection backoff.
pub struct ConnectionManager {
    state: Arc<AtomicU8>,
    connected: Arc<AtomicBool>,
    reconnect_attempts: Arc<AtomicU32>,
    config: ResiliencyConfig,
}

impl ConnectionManager {
    /// Create a new manager with the given configuration.
    pub fn new(config: ResiliencyConfig) -> Self {
        Self {
            state: Arc::new(AtomicU8::new(ConnectionState::Disconnected as u8)),
            connected: Arc::new(AtomicBool::new(false)),
            reconnect_attempts: Arc::new(AtomicU32::new(0)),
            config,
        }
    }

    /// Calculate the backoff duration (with jitter) for the given attempt.
    ///
    /// The base delay is `base_backoff * 2^attempt`, clamped to `max_backoff`.
    /// Jitter subtracts up to `jitter_percent`% from the computed value so the
    /// result is never *larger* than the deterministic exponential value.
    pub fn backoff_duration(&self, attempt: u32) -> Duration {
        let base_ms = u64::try_from(self.config.base_backoff.as_millis()).unwrap_or(u64::MAX);
        let exp_ms = base_ms.saturating_mul(1u64 << attempt.min(31));
        let max_ms = u64::try_from(self.config.max_backoff.as_millis()).unwrap_or(u64::MAX);
        let capped_ms = exp_ms.min(max_ms);

        // Deterministic-ish jitter based on attempt number.  We avoid pulling
        // in `rand` by using a simple hash derived from the attempt counter.
        let jitter_range = capped_ms * u64::from(self.config.jitter_percent) / 100;
        let jitter = if jitter_range > 0 {
            // Simple deterministic scatter — varies per attempt.
            let hash = u64::from(attempt).wrapping_mul(2_654_435_761);
            hash % (jitter_range + 1)
        } else {
            0
        };

        Duration::from_millis(capped_ms.saturating_sub(jitter))
    }

    /// Record that the connection has been established.
    pub fn connected(&self) {
        self.state.store(ConnectionState::Connected as u8, Ordering::SeqCst);
        self.connected.store(true, Ordering::SeqCst);
        self.reconnect_attempts.store(0, Ordering::SeqCst);
    }

    /// Record that the connection has been lost.
    pub fn disconnected(&self) {
        self.state.store(ConnectionState::Disconnected as u8, Ordering::SeqCst);
        self.connected.store(false, Ordering::SeqCst);
    }

    /// Set the state to `Connecting`.
    pub fn set_connecting(&self) {
        self.state.store(ConnectionState::Connecting as u8, Ordering::SeqCst);
    }

    /// Set the state to `Reconnecting` and bump the attempt counter.
    pub fn set_reconnecting(&self) {
        self.state.store(ConnectionState::Reconnecting as u8, Ordering::SeqCst);
        self.reconnect_attempts.fetch_add(1, Ordering::SeqCst);
    }

    /// Returns `true` if another reconnect attempt should be made.
    pub fn should_reconnect(&self) -> bool {
        self.config
            .max_reconnect_attempts
            .is_none_or(|max| self.reconnect_attempts.load(Ordering::SeqCst) < max)
    }

    /// Current connection state.
    pub fn state(&self) -> ConnectionState {
        ConnectionState::from_u8(self.state.load(Ordering::SeqCst))
    }

    /// Whether the connection is currently alive.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Reset the reconnect counter (e.g. after a successful connection).
    pub fn reset(&self) {
        self.reconnect_attempts.store(0, Ordering::SeqCst);
    }

    /// Current reconnect attempt count.
    pub fn reconnect_attempts(&self) -> u32 {
        self.reconnect_attempts.load(Ordering::SeqCst)
    }

    /// Borrow the configuration.
    pub fn config(&self) -> &ResiliencyConfig {
        &self.config
    }
}

impl std::fmt::Debug for ConnectionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionManager")
            .field("state", &self.state())
            .field("reconnect_attempts", &self.reconnect_attempts())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// MessageBuffer
// ---------------------------------------------------------------------------

/// A bounded FIFO buffer for outbound messages queued while disconnected.
pub struct MessageBuffer {
    queue: Mutex<VecDeque<String>>,
    max_size: usize,
}

impl MessageBuffer {
    /// Create a new buffer with the given capacity.
    pub fn new(max_size: usize) -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(max_size.min(1024))),
            max_size,
        }
    }

    /// Enqueue a message.  Returns `false` (and drops the message) if the
    /// buffer is already at capacity.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn enqueue(&self, message: String) -> bool {
        let mut q = self.queue.lock().expect("MessageBuffer lock poisoned");
        if q.len() >= self.max_size {
            return false;
        }
        q.push_back(message);
        true
    }

    /// Drain and return all buffered messages in FIFO order.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn drain(&self) -> Vec<String> {
        let mut q = self.queue.lock().expect("MessageBuffer lock poisoned");
        q.drain(..).collect()
    }

    /// Number of messages currently buffered.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn len(&self) -> usize {
        self.queue.lock().expect("MessageBuffer lock poisoned").len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for MessageBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageBuffer")
            .field("len", &self.len())
            .field("max_size", &self.max_size)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // 1. ResiliencyConfig defaults are correct
    #[test]
    fn resiliency_config_defaults() {
        let cfg = ResiliencyConfig::default();
        assert_eq!(cfg.heartbeat_interval, Duration::from_secs(15));
        assert_eq!(cfg.heartbeat_timeout, Duration::from_secs(5));
        assert_eq!(cfg.base_backoff, Duration::from_secs(1));
        assert_eq!(cfg.max_backoff, Duration::from_secs(30));
        assert!(cfg.max_reconnect_attempts.is_none());
        assert_eq!(cfg.jitter_percent, 20);
        assert_eq!(cfg.message_buffer_size, 256);
    }

    // 2. backoff_duration exponential growth (1s, 2s, 4s, 8s...)
    #[test]
    fn backoff_exponential_growth() {
        let cfg = ResiliencyConfig {
            jitter_percent: 0, // disable jitter so we can check exact values
            ..Default::default()
        };
        let mgr = ConnectionManager::new(cfg);

        assert_eq!(mgr.backoff_duration(0), Duration::from_secs(1));
        assert_eq!(mgr.backoff_duration(1), Duration::from_secs(2));
        assert_eq!(mgr.backoff_duration(2), Duration::from_secs(4));
        assert_eq!(mgr.backoff_duration(3), Duration::from_secs(8));
    }

    // 3. backoff_duration capped at max_backoff
    #[test]
    fn backoff_capped_at_max() {
        let cfg = ResiliencyConfig {
            jitter_percent: 0,
            ..Default::default()
        };
        let mgr = ConnectionManager::new(cfg);

        // 2^10 = 1024s, which exceeds the 30s max
        assert_eq!(mgr.backoff_duration(10), Duration::from_secs(30));
        assert_eq!(mgr.backoff_duration(20), Duration::from_secs(30));
    }

    // 4. backoff_duration has jitter (not exactly 2^n)
    #[test]
    fn backoff_has_jitter() {
        let cfg = ResiliencyConfig {
            jitter_percent: 20,
            ..Default::default()
        };
        let mgr = ConnectionManager::new(cfg);

        // With 20% jitter, attempt 3 base = 8000 ms, jitter up to 1600 ms,
        // so result should be in [6400, 8000].
        let d = mgr.backoff_duration(3);
        assert!(d >= Duration::from_millis(6400), "duration {d:?} below expected floor");
        assert!(d <= Duration::from_millis(8000), "duration {d:?} above expected ceiling");

        // At least one of several attempts should differ from the exact base
        // (the jitter hash varies per attempt).
        let exact_values: Vec<bool> = (0..5).map(|a| mgr.backoff_duration(a).as_millis() == 1000 * (1u128 << a)).collect();
        let all_exact = exact_values.iter().all(|&v| v);
        assert!(!all_exact, "jitter should cause at least one attempt to differ from exact 2^n");
    }

    // 5. ConnectionState transitions: Disconnected -> Connecting -> Connected
    #[test]
    fn connection_state_transitions() {
        let mgr = ConnectionManager::new(ResiliencyConfig::default());

        assert_eq!(mgr.state(), ConnectionState::Disconnected);
        assert!(!mgr.is_connected());

        mgr.set_connecting();
        assert_eq!(mgr.state(), ConnectionState::Connecting);
        assert!(!mgr.is_connected());

        mgr.connected();
        assert_eq!(mgr.state(), ConnectionState::Connected);
        assert!(mgr.is_connected());

        mgr.disconnected();
        assert_eq!(mgr.state(), ConnectionState::Disconnected);
        assert!(!mgr.is_connected());

        mgr.set_reconnecting();
        assert_eq!(mgr.state(), ConnectionState::Reconnecting);
        assert_eq!(mgr.reconnect_attempts(), 1);
    }

    // 6. should_reconnect respects max_attempts
    #[test]
    fn should_reconnect_respects_max_attempts() {
        let cfg = ResiliencyConfig {
            max_reconnect_attempts: Some(3),
            ..Default::default()
        };
        let mgr = ConnectionManager::new(cfg);

        assert!(mgr.should_reconnect()); // 0 < 3
        mgr.set_reconnecting(); // attempts = 1
        assert!(mgr.should_reconnect()); // 1 < 3
        mgr.set_reconnecting(); // attempts = 2
        assert!(mgr.should_reconnect()); // 2 < 3
        mgr.set_reconnecting(); // attempts = 3
        assert!(!mgr.should_reconnect()); // 3 >= 3

        // Unlimited should always allow reconnect
        let unlimited_mgr = ConnectionManager::new(ResiliencyConfig::default());
        for _ in 0..100 {
            unlimited_mgr.set_reconnecting();
        }
        assert!(unlimited_mgr.should_reconnect());
    }

    // 7. MessageBuffer enqueue + drain
    #[test]
    fn message_buffer_enqueue_and_drain() {
        let buf = MessageBuffer::new(10);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);

        assert!(buf.enqueue("msg1".into()));
        assert!(buf.enqueue("msg2".into()));
        assert!(buf.enqueue("msg3".into()));
        assert_eq!(buf.len(), 3);
        assert!(!buf.is_empty());

        let drained = buf.drain();
        assert_eq!(drained, vec!["msg1", "msg2", "msg3"]);
        assert!(buf.is_empty());

        // Drain on empty returns empty vec
        let empty = buf.drain();
        assert!(empty.is_empty());
    }

    // 8. MessageBuffer rejects when full
    #[test]
    fn message_buffer_rejects_when_full() {
        let buf = MessageBuffer::new(3);

        assert!(buf.enqueue("a".into()));
        assert!(buf.enqueue("b".into()));
        assert!(buf.enqueue("c".into()));
        assert_eq!(buf.len(), 3);

        // Should reject — buffer is full
        assert!(!buf.enqueue("d".into()));
        assert_eq!(buf.len(), 3);

        // After draining, can enqueue again
        let drained = buf.drain();
        assert_eq!(drained.len(), 3);
        assert!(buf.enqueue("d".into()));
        assert_eq!(buf.len(), 1);
    }
}
