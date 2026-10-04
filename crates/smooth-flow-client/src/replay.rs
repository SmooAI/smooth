//! Terminal stream ordering around `flow.replay` (spec §10, th-c61966).
//!
//! An engine that owns its PTYs answers `flow.attach` with a `flow.replay`: a
//! VT snapshot current through some `seq`. Live `flow.output` frames follow,
//! and the client must apply exactly the ones newer than the snapshot. This
//! module is that decision, per attached session, with no I/O: feed it the
//! frames in arrival order and do what the returned [`Action`]s say.
//!
//! The rules (normative text in `SmoothFlow-Client-Spec.md` §10):
//!
//! - While a replay is expected and has not arrived, outputs are **buffered**.
//!   The engine subscribes before it snapshots, so outputs the snapshot
//!   already covers can arrive on either side of it.
//! - A complete replay **resets** the terminal at its `cols`×`rows`, writes the
//!   snapshot, and becomes the baseline. Buffered outputs newer than it are
//!   then written in arrival order; the rest are dropped.
//! - With a baseline, an output whose `seq` is ≤ the baseline is dropped and
//!   any other is written. Outputs are never compared with each other: a run
//!   the relay split into chunks shares one `seq`, and every chunk applies.
//! - The latest complete replay is always the baseline, even when its `seq` is
//!   lower than the last one's (a relaunched session's new host).
//! - A chunked replay (`part`/`parts`) is never rendered partially. A new
//!   `part: 0` discards an incomplete one; a gap or a malformed part asks the
//!   client to re-attach ([`Action::Resync`]).

use serde::{Deserialize, Serialize};

/// One inbound terminal-stream frame for a single session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Frame {
    /// `flow.output`.
    #[serde(rename = "flow.output")]
    Output { seq: u64, data: Vec<u8> },
    /// `flow.replay`, or one part of a chunked one. An unchunked replay has
    /// no `part`/`parts` and is part 0 of 1.
    #[serde(rename = "flow.replay")]
    Replay {
        seq: u64,
        cols: u16,
        rows: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        part: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parts: Option<u32>,
        data: Vec<u8>,
    },
}

/// What the client does with its terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Action {
    /// Replace the terminal with a fresh one at this size: screen, history,
    /// modes, selection and scroll position all go.
    Reset { cols: u16, rows: u16 },
    /// Feed these bytes to the terminal.
    Write { data: Vec<u8> },
    /// The stream can't be trusted: call [`ReplayOrder::on_attach`] and send
    /// `flow.attach` again for a fresh replay.
    Resync { reason: ResyncReason },
}

/// Why a [`Action::Resync`] was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResyncReason {
    /// A chunked replay skipped a part, changed shape mid-way, or a part
    /// arrived with no part 0 before it.
    ReplayGap,
    /// `parts` was 0 or `part` ≥ `parts`.
    ReplayMalformed,
    /// More output was buffered, waiting for a replay, than the client allows.
    BufferOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Partial {
    seq: u64,
    cols: u16,
    rows: u16,
    parts: u32,
    next: u32,
    data: Vec<u8>,
}

/// The ordering state for one attached session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayOrder {
    expect_replay: bool,
    max_pending_bytes: usize,
    baseline: Option<u64>,
    partial: Option<Partial>,
    pending: Vec<(u64, Vec<u8>)>,
    pending_bytes: usize,
}

impl ReplayOrder {
    /// `expect_replay`: the engine advertised `replay` in `flow.hello` and the
    /// client attached with `replay: true`, so a `flow.replay` will open the
    /// stream. Against an older engine, pass `false`: outputs apply in arrival
    /// order, as before. `max_pending_bytes` bounds the buffer kept while a
    /// replay is awaited.
    #[must_use]
    pub const fn new(expect_replay: bool, max_pending_bytes: usize) -> Self {
        Self {
            expect_replay,
            max_pending_bytes,
            baseline: None,
            partial: None,
            pending: Vec::new(),
            pending_bytes: 0,
        }
    }

    /// The `seq` the terminal is current through, once a replay has applied.
    #[must_use]
    pub const fn baseline(&self) -> Option<u64> {
        self.baseline
    }

    /// True while outputs are being held for a replay.
    #[must_use]
    pub const fn is_waiting(&self) -> bool {
        self.partial.is_some() || (self.expect_replay && self.baseline.is_none())
    }

    /// The client is (re)sending `flow.attach`: forget the baseline and
    /// anything held, and wait for the replay the attach brings.
    pub fn on_attach(&mut self) {
        self.baseline = None;
        self.partial = None;
        self.pending.clear();
        self.pending_bytes = 0;
    }

    /// One frame, in arrival order. Returns what to do, in order.
    pub fn on_frame(&mut self, frame: Frame) -> Vec<Action> {
        match frame {
            Frame::Output { seq, data } => self.on_output(seq, data),
            Frame::Replay {
                seq,
                cols,
                rows,
                part,
                parts,
                data,
            } => self.on_replay(seq, cols, rows, part.unwrap_or(0), parts.unwrap_or(1), data),
        }
    }

    fn on_output(&mut self, seq: u64, data: Vec<u8>) -> Vec<Action> {
        if self.is_waiting() {
            self.pending_bytes += data.len();
            self.pending.push((seq, data));
            if self.pending_bytes > self.max_pending_bytes {
                return self.resync(ResyncReason::BufferOverflow);
            }
            return Vec::new();
        }
        if self.baseline.is_some_and(|b| seq <= b) {
            return Vec::new();
        }
        vec![Action::Write { data }]
    }

    fn on_replay(&mut self, seq: u64, cols: u16, rows: u16, part: u32, parts: u32, data: Vec<u8>) -> Vec<Action> {
        if parts == 0 || part >= parts {
            return self.resync(ResyncReason::ReplayMalformed);
        }
        if part == 0 {
            // A new replay; an incomplete one before it is superseded.
            self.partial = Some(Partial {
                seq,
                cols,
                rows,
                parts,
                next: 1,
                data,
            });
        } else {
            match self.partial.as_mut() {
                Some(p) if p.seq == seq && p.parts == parts && p.cols == cols && p.rows == rows && p.next == part => {
                    p.data.extend(data);
                    p.next += 1;
                }
                _ => return self.resync(ResyncReason::ReplayGap),
            }
        }
        match self.partial.take() {
            Some(p) if p.next == p.parts => self.apply(p),
            other => {
                self.partial = other;
                Vec::new()
            }
        }
    }

    fn apply(&mut self, p: Partial) -> Vec<Action> {
        let mut out = vec![Action::Reset { cols: p.cols, rows: p.rows }];
        if !p.data.is_empty() {
            out.push(Action::Write { data: p.data });
        }
        self.baseline = Some(p.seq);
        self.pending_bytes = 0;
        out.extend(self.pending.drain(..).filter(|(seq, _)| *seq > p.seq).map(|(_, data)| Action::Write { data }));
        out
    }

    fn resync(&mut self, reason: ResyncReason) -> Vec<Action> {
        self.on_attach();
        vec![Action::Resync { reason }]
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn out(seq: u64, s: &str) -> Frame {
        Frame::Output {
            seq,
            data: s.as_bytes().to_vec(),
        }
    }

    fn replay(seq: u64, s: &str) -> Frame {
        Frame::Replay {
            seq,
            cols: 80,
            rows: 24,
            part: None,
            parts: None,
            data: s.as_bytes().to_vec(),
        }
    }

    fn part(seq: u64, part: u32, parts: u32, s: &str) -> Frame {
        Frame::Replay {
            seq,
            cols: 80,
            rows: 24,
            part: Some(part),
            parts: Some(parts),
            data: s.as_bytes().to_vec(),
        }
    }

    fn w(s: &str) -> Action {
        Action::Write { data: s.as_bytes().to_vec() }
    }

    const RESET: Action = Action::Reset { cols: 80, rows: 24 };

    fn run(o: &mut ReplayOrder, frames: Vec<Frame>) -> Vec<Action> {
        frames.into_iter().flat_map(|f| o.on_frame(f)).collect()
    }

    #[test]
    fn replay_then_newer_output() {
        let mut o = ReplayOrder::new(true, 1024);
        assert!(o.is_waiting());
        assert_eq!(run(&mut o, vec![replay(5, "S"), out(6, "a"), out(7, "b")]), vec![RESET, w("S"), w("a"), w("b")]);
        assert_eq!(o.baseline(), Some(5));
        assert!(!o.is_waiting());
    }

    #[test]
    fn outputs_before_the_replay_are_buffered_and_stale_ones_dropped() {
        let mut o = ReplayOrder::new(true, 1024);
        assert!(run(&mut o, vec![out(4, "old"), out(5, "covered"), out(6, "new")]).is_empty());
        assert_eq!(o.on_frame(replay(5, "S")), vec![RESET, w("S"), w("new")]);
    }

    #[test]
    fn stale_output_after_the_replay_is_dropped() {
        let mut o = ReplayOrder::new(true, 1024);
        assert_eq!(
            run(&mut o, vec![replay(9, "S"), out(9, "x"), out(3, "y"), out(10, "z")]),
            vec![RESET, w("S"), w("z")]
        );
    }

    #[test]
    fn a_later_replay_supersedes_even_with_a_lower_seq() {
        let mut o = ReplayOrder::new(true, 1024);
        run(&mut o, vec![replay(50, "A"), out(51, "a")]);
        assert_eq!(run(&mut o, vec![replay(2, "B"), out(3, "b")]), vec![RESET, w("B"), w("b")]);
        assert_eq!(o.baseline(), Some(2));
    }

    #[test]
    fn a_chunked_replay_applies_only_when_complete() {
        let mut o = ReplayOrder::new(true, 1024);
        assert!(run(&mut o, vec![part(7, 0, 3, "AB"), out(8, "x"), part(7, 1, 3, "CD")]).is_empty());
        assert!(o.is_waiting());
        assert_eq!(o.on_frame(part(7, 2, 3, "E")), vec![RESET, w("ABCDE"), w("x")]);
    }

    #[test]
    fn a_new_part_zero_discards_an_incomplete_replay() {
        let mut o = ReplayOrder::new(true, 1024);
        assert!(o.on_frame(part(7, 0, 2, "old")).is_empty());
        assert!(o.on_frame(part(9, 0, 2, "N")).is_empty());
        assert_eq!(o.on_frame(part(9, 1, 2, "EW")), vec![RESET, w("NEW")]);
    }

    #[test]
    fn a_gap_or_bad_part_resyncs() {
        let mut o = ReplayOrder::new(true, 1024);
        o.on_frame(part(7, 0, 3, "A"));
        assert_eq!(
            o.on_frame(part(7, 2, 3, "C")),
            vec![Action::Resync {
                reason: ResyncReason::ReplayGap
            }]
        );
        assert!(o.is_waiting());
        assert_eq!(
            o.on_frame(part(7, 3, 3, "C")),
            vec![Action::Resync {
                reason: ResyncReason::ReplayMalformed
            }]
        );
        assert_eq!(
            o.on_frame(part(7, 0, 0, "C")),
            vec![Action::Resync {
                reason: ResyncReason::ReplayMalformed
            }]
        );
        // A part from a different replay than the one in progress is a gap too.
        o.on_frame(part(7, 0, 2, "A"));
        assert_eq!(
            o.on_frame(part(8, 1, 2, "B")),
            vec![Action::Resync {
                reason: ResyncReason::ReplayGap
            }]
        );
    }

    #[test]
    fn relay_chunks_sharing_a_seq_all_apply() {
        let mut o = ReplayOrder::new(true, 1024);
        assert_eq!(
            run(&mut o, vec![replay(1, ""), out(4, "a"), out(4, "b"), out(4, "c")]),
            vec![RESET, w("a"), w("b"), w("c")]
        );
    }

    #[test]
    fn an_old_engine_streams_in_arrival_order() {
        let mut o = ReplayOrder::new(false, 1024);
        assert!(!o.is_waiting());
        assert_eq!(run(&mut o, vec![out(3, "a"), out(1, "b")]), vec![w("a"), w("b")]);
        // A replay is still honoured if one shows up.
        assert_eq!(run(&mut o, vec![replay(5, "S"), out(5, "x"), out(6, "y")]), vec![RESET, w("S"), w("y")]);
    }

    #[test]
    fn overflowing_the_buffer_resyncs_and_reattach_waits_again() {
        let mut o = ReplayOrder::new(true, 4);
        assert!(o.on_frame(out(1, "abc")).is_empty());
        assert_eq!(
            o.on_frame(out(2, "de")),
            vec![Action::Resync {
                reason: ResyncReason::BufferOverflow
            }]
        );
        assert!(o.is_waiting());
        assert_eq!(run(&mut o, vec![replay(2, "S"), out(3, "f")]), vec![RESET, w("S"), w("f")]);
    }

    #[test]
    fn on_attach_forgets_the_baseline() {
        let mut o = ReplayOrder::new(true, 1024);
        run(&mut o, vec![replay(5, "S")]);
        o.on_attach();
        assert_eq!(o.baseline(), None);
        assert!(o.on_frame(out(6, "a")).is_empty());
    }

    #[test]
    fn frames_parse_from_their_vector_shape() {
        let f: Frame = serde_json::from_str(r#"{"type":"flow.replay","seq":1,"cols":2,"rows":3,"data":[65]}"#).unwrap();
        assert_eq!(
            f,
            Frame::Replay {
                seq: 1,
                cols: 2,
                rows: 3,
                part: None,
                parts: None,
                data: vec![65]
            }
        );
    }
}
