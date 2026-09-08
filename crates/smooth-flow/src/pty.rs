//! The PTY bridge: one `portable-pty` per *attached* session running
//! `tmux attach -t <session>` on the flow socket.
//!
//! Why attach-over-PTY and not `pipe-pane`: `pipe-pane` yields the pane's
//! raw output for the pane's *own* size and only from the moment the pipe
//! opens — a late-joining client would get bytes positioned for a foreign
//! geometry and no initial screen. A tmux *client* on a PTY gets a full
//! redraw on attach, follows the PTY's size (`window-size latest`), and
//! passes keystrokes through untouched — lossless bytes + resize, which is
//! exactly what a terminal-emulator surface (ghostty) needs. The PTY exists
//! only while ≥1 client is attached; detaching the last one drops it and the
//! tmux session keeps running detached.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

/// Output chunk sink: `(seq, bytes)`; `bytes` is empty exactly once, at EOF.
pub type OnOutput = Arc<dyn Fn(u64, Vec<u8>) + Send + Sync>;

/// A live `tmux attach` client on a PTY.
pub struct PtyAttach {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    seq: AtomicU64,
    /// Number of flow clients currently attached through this PTY.
    pub clients: AtomicU64,
}

impl PtyAttach {
    /// Spawn `argv` (the `tmux attach` line) on a fresh PTY of `cols`×`rows`
    /// and stream its output to `on_output` from a reader thread.
    ///
    /// # Errors
    /// When the PTY cannot be opened or the command cannot be spawned.
    pub fn spawn(argv: &[String], cols: u16, rows: u16, on_output: OnOutput) -> Result<Arc<Self>> {
        let (program, rest) = argv.split_first().context("empty attach argv")?;
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: rows.max(2),
                cols: cols.max(2),
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(rest);
        cmd.env("TERM", "xterm-256color");
        // A tmux client inside a tmux server would try to nest — never what we want.
        cmd.env_remove("TMUX");
        let mut child = pair.slave.spawn_command(cmd).context("spawn attach")?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let writer = pair.master.take_writer().context("take pty writer")?;
        let this = Arc::new(Self {
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            seq: AtomicU64::new(0),
            clients: AtomicU64::new(0),
        });
        let weak = Arc::downgrade(&this);
        std::thread::Builder::new()
            .name("flow-pty-reader".into())
            .spawn(move || {
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let seq = weak.upgrade().map_or(0, |p| p.seq.fetch_add(1, Ordering::Relaxed));
                            on_output(seq, buf[..n].to_vec());
                        }
                    }
                }
                let _ = child.wait();
                let seq = weak.upgrade().map_or(0, |p| p.seq.fetch_add(1, Ordering::Relaxed));
                on_output(seq, Vec::new());
            })
            .context("spawn pty reader thread")?;
        Ok(this)
    }

    /// Write raw bytes (keystrokes) to the PTY.
    ///
    /// # Errors
    /// When the PTY is closed.
    pub fn write(&self, data: &[u8]) -> Result<()> {
        let mut w = self.writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        w.write_all(data).context("pty write")?;
        w.flush().context("pty flush")
    }

    /// Resize the PTY; tmux follows (`window-size latest`).
    ///
    /// # Errors
    /// When the PTY is closed.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .resize(PtySize {
                rows: rows.max(2),
                cols: cols.max(2),
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("pty resize")
    }

    /// The next output sequence number that will be assigned.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    /// Kill the attach client (the tmux session is untouched).
    pub fn close(&self) {
        let _ = self.killer.lock().unwrap_or_else(std::sync::PoisonError::into_inner).kill();
    }
}

impl Drop for PtyAttach {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    #[cfg(unix)]
    fn streams_bytes_and_accepts_input_and_resize() {
        let (tx, rx) = mpsc::channel::<(u64, Vec<u8>)>();
        let on_output: OnOutput = Arc::new(move |seq, bytes| {
            let _ = tx.send((seq, bytes));
        });
        // `cat` echoes what we type; `stty size` after a resize proves the
        // PTY geometry reached the child.
        let pty = PtyAttach::spawn(&["sh".into(), "-c".into(), "stty size; cat".into()], 80, 24, on_output).unwrap();
        let mut collected = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !String::from_utf8_lossy(&collected).contains("24 80") {
            if let Ok((_, b)) = rx.recv_timeout(Duration::from_millis(200)) {
                collected.extend(b);
            }
        }
        assert!(String::from_utf8_lossy(&collected).contains("24 80"), "{}", String::from_utf8_lossy(&collected));
        pty.resize(100, 30).unwrap();
        pty.write(b"hello-pty\n").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut seqs = Vec::new();
        while std::time::Instant::now() < deadline && !String::from_utf8_lossy(&collected).contains("hello-pty") {
            if let Ok((s, b)) = rx.recv_timeout(Duration::from_millis(200)) {
                seqs.push(s);
                collected.extend(b);
            }
        }
        assert!(String::from_utf8_lossy(&collected).contains("hello-pty"));
        assert!(seqs.windows(2).all(|w| w[1] > w[0]), "seq is monotonic: {seqs:?}");
        assert!(pty.next_seq() > 0);
        pty.close();
        // EOF marker: an empty chunk arrives after the child dies.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut saw_eof = false;
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok((_, b)) if b.is_empty() => {
                    saw_eof = true;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        assert!(saw_eof, "reader thread reports EOF with an empty chunk");
    }

    #[test]
    fn empty_argv_is_an_error() {
        let on_output: OnOutput = Arc::new(|_, _| {});
        assert!(PtyAttach::spawn(&[], 80, 24, on_output).is_err());
    }
}
