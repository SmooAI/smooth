//! Spot the alternate-screen switches (`CSI ? 1049 h/l`, `? 1047`, `? 47`) in
//! a byte stream, across `feed` calls.
//!
//! libghostty-vt's formatter only sees the ACTIVE screen, so while a TUI is on
//! the alternate screen the primary screen and its history are invisible to
//! it. The primary cannot change while the alternate screen is up, so
//! [`crate::Vt`] snapshots it at the instant the switch happens: it feeds the
//! terminal everything up to the switch's final byte, captures the primary,
//! then feeds the rest. This scanner says where those final bytes are.
//!
//! It tracks just enough of the VT parser to do that: ESC, CSI, the `?`
//! private marker and the parameter list. A sequence split across `feed`
//! calls carries over in the state, so nothing is buffered or held back —
//! every byte still reaches the terminal in the call it arrived in.

/// What a final byte did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// `CSI ? … h` naming an alternate-screen mode.
    Enter,
    /// `CSI ? … l` naming an alternate-screen mode.
    Exit,
}

/// The modes that put up / take down the alternate screen.
const ALT_MODES: [u32; 3] = [1049, 1047, 47];
/// More parameters than any real sequence; later ones are ignored.
const MAX_PARAMS: usize = 16;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Esc,
    /// `ESC [` — waiting for the private marker.
    Csi,
    /// `ESC [ ?` and parameters.
    Private,
    /// Inside a CSI we don't care about, until its final byte.
    Ignore,
}

#[derive(Debug, Clone, Default)]
pub struct Scanner {
    state: State,
    params: [u32; MAX_PARAMS],
    count: usize,
    current: u32,
}

impl Scanner {
    /// Advance by one byte; `Some` when this byte is the final byte of an
    /// alternate-screen switch.
    pub fn step(&mut self, b: u8) -> Option<Switch> {
        match b {
            // ESC restarts a sequence from any state; CAN and SUB abort one.
            0x1b => {
                self.state = State::Esc;
                return None;
            }
            0x18 | 0x1a => {
                self.state = State::Ground;
                return None;
            }
            _ => {}
        }
        match self.state {
            State::Ground => None,
            State::Esc => {
                self.state = if b == b'[' { State::Csi } else { State::Ground };
                None
            }
            State::Csi => {
                self.state = match b {
                    b'?' => {
                        self.params = [0; MAX_PARAMS];
                        self.count = 0;
                        self.current = 0;
                        State::Private
                    }
                    // C0 controls execute inside a sequence without ending it.
                    0x00..=0x1f => State::Csi,
                    0x40..=0x7e => State::Ground,
                    _ => State::Ignore,
                };
                None
            }
            State::Private => match b {
                b'0'..=b'9' => {
                    self.current = self.current.saturating_mul(10).saturating_add(u32::from(b - b'0'));
                    None
                }
                b';' | b':' => {
                    self.push_param();
                    None
                }
                0x00..=0x1f => None,
                b'h' | b'l' => {
                    self.push_param();
                    self.state = State::Ground;
                    let alt = self.params[..self.count].iter().any(|p| ALT_MODES.contains(p));
                    match (alt, b) {
                        (true, b'h') => Some(Switch::Enter),
                        (true, _) => Some(Switch::Exit),
                        _ => None,
                    }
                }
                0x40..=0x7e => {
                    self.state = State::Ground;
                    None
                }
                // Intermediates (`$`, `"`, …) make it a different sequence
                // (DECRQM is `CSI ? 1049 $ p`).
                _ => {
                    self.state = State::Ignore;
                    None
                }
            },
            State::Ignore => {
                if (0x40..=0x7e).contains(&b) {
                    self.state = State::Ground;
                }
                None
            }
        }
    }

    fn push_param(&mut self) {
        if self.count < MAX_PARAMS {
            self.params[self.count] = self.current;
            self.count += 1;
        }
        self.current = 0;
    }

    /// True when no sequence is in progress (the next ESC-free chunk needs no scan).
    pub fn is_ground(&self) -> bool {
        self.state == State::Ground
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every (index, switch) the scanner reports for `input`, fed in one go.
    fn scan(input: &[u8]) -> Vec<(usize, Switch)> {
        let mut s = Scanner::default();
        input.iter().enumerate().filter_map(|(i, &b)| s.step(b).map(|sw| (i, sw))).collect()
    }

    #[test]
    fn finds_each_alt_mode() {
        for mode in ["1049", "1047", "47"] {
            let on = format!("ab\x1b[?{mode}h");
            assert_eq!(scan(on.as_bytes()), vec![(on.len() - 1, Switch::Enter)], "{mode}");
            let off = format!("\x1b[?{mode}lz");
            assert_eq!(scan(off.as_bytes()), vec![(off.len() - 2, Switch::Exit)], "{mode}");
        }
    }

    #[test]
    fn finds_it_inside_a_parameter_list() {
        assert_eq!(scan(b"\x1b[?25;1049h"), vec![(10, Switch::Enter)]);
        assert_eq!(scan(b"\x1b[?1049;25l"), vec![(10, Switch::Exit)]);
    }

    #[test]
    fn ignores_lookalikes() {
        for s in [
            &b"\x1b[1049h"[..], // ANSI mode, not DEC private
            b"\x1b[?1048h",     // save cursor only
            b"\x1b[?10490h",    // a different number
            b"\x1b[?104h",
            b"\x1b[?1049$p",      // DECRQM query
            b"\x1b[?1049\x18h",   // cancelled by CAN
            b"\x1b]0;?1049h\x07", // inside an OSC payload
            b"?1049h",            // no CSI at all
        ] {
            assert!(scan(s).is_empty(), "{s:?}: {:?}", scan(s));
        }
    }

    #[test]
    fn esc_restarts_mid_sequence() {
        // An ESC inside an OSC ends it, as in the real parser; what follows
        // is a fresh CSI.
        assert_eq!(scan(b"\x1b]0;title\x1b[?1049h"), vec![(16, Switch::Enter)]);
        assert_eq!(scan(b"\x1b[?10\x1b[?47h"), vec![(10, Switch::Enter)]);
    }

    #[test]
    fn carries_a_sequence_across_steps() {
        // The state is the carry: stop after any prefix, resume, same answer.
        let whole = b"x\x1b[?1049hy";
        for split in 0..whole.len() {
            let mut s = Scanner::default();
            let first: Vec<_> = whole[..split].iter().filter_map(|&b| s.step(b)).collect();
            let second: Vec<_> = whole[split..].iter().filter_map(|&b| s.step(b)).collect();
            let all: Vec<_> = first.into_iter().chain(second).collect();
            assert_eq!(all, vec![Switch::Enter], "split at {split}");
            assert!(s.is_ground());
        }
    }

    #[test]
    fn survives_giant_parameters() {
        let mut s = Scanner::default();
        let mut seq = b"\x1b[?".to_vec();
        seq.extend(std::iter::repeat_n(b'9', 10_000));
        seq.extend(std::iter::repeat_n(b';', 100));
        seq.extend(b"1049h");
        let hits: Vec<_> = seq.iter().filter_map(|&b| s.step(b)).collect();
        // 1049 is past MAX_PARAMS, so it is ignored: no panic, no false switch.
        assert!(hits.is_empty());
        assert!(s.is_ground());
    }
}
