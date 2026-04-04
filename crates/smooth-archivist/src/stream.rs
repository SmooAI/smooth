use smooth_scribe::LogEntry;
use tokio::sync::broadcast;

/// An event stream that broadcasts log entries to multiple subscribers.
/// Used for SSE streaming to TUI/web clients.
#[derive(Debug)]
pub struct EventStream {
    sender: broadcast::Sender<LogEntry>,
}

impl EventStream {
    /// Create a new event stream with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Push a log entry into the stream, broadcasting to all subscribers.
    /// Returns the number of receivers that received the message.
    ///
    /// # Errors
    ///
    /// Returns an error if there are no active receivers.
    #[allow(clippy::result_large_err)]
    pub fn push(&self, entry: LogEntry) -> Result<usize, broadcast::error::SendError<LogEntry>> {
        self.sender.send(entry)
    }

    /// Subscribe to the event stream. Returns a receiver that will receive
    /// all future log entries pushed to this stream.
    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.sender.subscribe()
    }
}

impl Default for EventStream {
    fn default() -> Self {
        Self::new(256)
    }
}

#[cfg(test)]
mod tests {
    use smooth_scribe::LogLevel;

    use super::*;

    #[tokio::test]
    async fn test_push_and_receive() {
        let stream = EventStream::new(16);
        let mut rx = stream.subscribe();

        let entry = LogEntry::new("svc", LogLevel::Info, "hello");
        let count = stream.push(entry.clone()).expect("send");
        assert_eq!(count, 1);

        let received = rx.recv().await.expect("recv");
        assert_eq!(received.message, "hello");
    }

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let stream = EventStream::new(16);
        let mut rx1 = stream.subscribe();
        let mut rx2 = stream.subscribe();

        let entry = LogEntry::new("svc", LogLevel::Warn, "broadcast");
        let count = stream.push(entry).expect("send");
        assert_eq!(count, 2);

        let r1 = rx1.recv().await.expect("recv1");
        let r2 = rx2.recv().await.expect("recv2");
        assert_eq!(r1.message, "broadcast");
        assert_eq!(r2.message, "broadcast");
    }

    #[test]
    fn test_push_no_receivers() {
        let stream = EventStream::new(16);
        let entry = LogEntry::new("svc", LogLevel::Info, "nobody listening");
        let result = stream.push(entry);
        assert!(result.is_err());
    }
}
