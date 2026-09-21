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
//!
//! Disposal hazard (th-6d8f84): `portable-pty`'s unix master *writer* writes
//! `\n` + `VEOF` into the PTY when it is dropped, so the child sees EOF. Our
//! child is a raw-mode tmux client, which forwards those two bytes to the
//! pane as keystrokes — an empty line, then `^D` at an empty prompt, and the
//! pane's login shell prints `logout` and exits. `close()` only SIGHUPs the
//! client (tmux prints `[lost tty]`), so dropping the writer right after it
//! races the signal. The writer therefore lives in a slot the reader thread
//! also holds and is released only after `child.wait()` returns: once the
//! client is gone there is nobody left to forward the bytes.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

/// Output chunk sink: `(seq, bytes)`; `bytes` is empty exactly once, at EOF.
pub type OnOutput = Arc<dyn Fn(u64, Vec<u8>) + Send + Sync>;

/// The PTY writer, shared with the reader thread so it is only ever dropped
/// after the child exited (see the module doc). `None` once released.
type WriterSlot = Arc<Mutex<Option<Box<dyn Write + Send>>>>;

/// A live `tmux attach` client on a PTY.
pub struct PtyAttach {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: WriterSlot,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    seq: AtomicU64,
    /// Set by the reader thread once the child has exited.
    exited: Arc<AtomicBool>,
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
        let writer: WriterSlot = Arc::new(Mutex::new(Some(pair.master.take_writer().context("take pty writer")?)));
        let exited = Arc::new(AtomicBool::new(false));
        let this = Arc::new(Self {
            master: Mutex::new(pair.master),
            writer: writer.clone(),
            killer: Mutex::new(killer),
            seq: AtomicU64::new(0),
            exited: exited.clone(),
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
                // The client is gone, so the writer's EOF-on-drop has nobody
                // to forward it to the pane. Never release it earlier.
                drop(writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take());
                exited.store(true, Ordering::Release);
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
        let mut slot = self.writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let w = slot.as_mut().context("pty closed")?;
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

    /// Whether the attach client has exited (the reader thread saw it go).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.exited.load(Ordering::Acquire)
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

    /// th-6d8f84: dropping a bridge whose child is still running must not
    /// type anything into it. portable-pty's writer sends `\n` + `VEOF` on
    /// drop; through a raw-mode tmux client those land in the pane as a
    /// blank line and `^D`, and the login shell logs out. The child here
    /// ignores SIGHUP (so `close()` cannot race it away) and records every
    /// byte it reads in raw mode, then exits on its own.
    #[test]
    #[cfg(unix)]
    fn dropping_a_live_bridge_types_nothing_into_the_child() {
        let tmp = tempfile::tempdir().unwrap();
        let got = tmp.path().join("stdin.bin");
        let script = format!(
            "trap '' HUP; stty raw -echo; exec 3<&0; cat <&3 > '{}' & echo READY; sleep 1; kill $!; wait",
            got.display()
        );
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let on_output: OnOutput = Arc::new(move |_, bytes| {
            let _ = tx.send(bytes);
        });
        let pty = PtyAttach::spawn(&["sh".into(), "-c".into(), script], 80, 24, on_output).unwrap();
        let mut seen = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !String::from_utf8_lossy(&seen).contains("READY") {
            if let Ok(b) = rx.recv_timeout(Duration::from_millis(100)) {
                seen.extend(b);
            }
        }
        assert!(String::from_utf8_lossy(&seen).contains("READY"), "{}", String::from_utf8_lossy(&seen));
        drop(pty);
        // The child exits by itself; the reader thread reports EOF after it.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(b) if b.is_empty() => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        let bytes = std::fs::read(&got).unwrap_or_default();
        assert!(bytes.is_empty(), "the dropped bridge typed {bytes:?} into its live child");
    }

    #[test]
    fn empty_argv_is_an_error() {
        let on_output: OnOutput = Arc::new(|_, _| {});
        assert!(PtyAttach::spawn(&[], 80, 24, on_output).is_err());
    }
}
