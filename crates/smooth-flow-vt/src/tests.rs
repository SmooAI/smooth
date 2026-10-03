//! Behaviour tests for [`Vt`]. The spike that proved the snapshot design
//! (th-5025fb comment, 2026-10-03) is ported first; then the round-trip
//! property, the alternate screen, bounding, resize, adversarial input and
//! input encoding.
// Test fixtures: small known values, readable over defensive.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_possible_truncation, clippy::format_collect)]

use super::*;

const BIG: usize = usize::MAX;

fn vt(cols: u16, rows: u16) -> Vt {
    Vt::new(cols, rows, 1000).unwrap()
}

/// A fresh terminal of the snapshot's size with the snapshot applied — what a
/// client does on attach.
fn replay(snap: &Snapshot) -> Vt {
    let mut c = Vt::new(snap.cols, snap.rows, 1000).unwrap();
    c.feed(&snap.bytes);
    c
}

/// Everything a client could observe, for equality checks.
#[derive(Debug, PartialEq, Eq)]
struct Observed {
    screen: String,
    /// History + screen row by row (the replay keeps rows, not soft wraps).
    rows: String,
    cursor: (u16, u16),
    alternate: bool,
    bracketed: bool,
    decckm: bool,
}

fn observe(v: &mut Vt) -> Observed {
    Observed {
        screen: v.plain_screen(),
        rows: rows(v),
        cursor: v.cursor(),
        alternate: v.alternate_on(),
        bracketed: v.bracketed_paste(),
        decckm: v.cursor_keys_application(),
    }
}

/// Source and replayed client agree on everything observable, including the
/// VT re-snapshot byte for byte.
fn assert_round_trips(src: &mut Vt, context: &str) {
    let snap = src.snapshot(BIG);
    assert_eq!(snap.screen, Fidelity::Full, "{context}");
    let mut client = replay(&snap);
    assert_eq!(observe(&mut client), observe(src), "{context}");
    let again = client.snapshot(BIG);
    assert_eq!(
        String::from_utf8_lossy(&again.bytes),
        String::from_utf8_lossy(&snap.bytes),
        "{context}: VT re-snapshot differs"
    );
}

/// Every row of history and screen, as laid out.
fn rows(v: &mut Vt) -> String {
    tidy(&String::from_utf8_lossy(&v.term.format(Format::PlainRows).unwrap()))
}

fn lines(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\r\n")).collect()
}

// ---- the spike -------------------------------------------------------------

#[test]
fn spike_formatter_includes_history() {
    let mut t = vt(20, 5);
    t.feed(lines(12).as_bytes());
    t.feed(b"\x1b[1;31mred\x1b[0m");
    let expected: String = (1..=12).map(|i| format!("line {i}\n")).collect::<String>() + "red";
    assert_eq!(t.plain_scrollback(), expected);
    let snap = t.snapshot(BIG);
    assert!(String::from_utf8_lossy(&snap.bytes).starts_with("line 1\r\n"), "history leads the replay");
    let mut c = replay(&snap);
    assert_eq!(c.plain_scrollback(), expected, "12 lines of history from a 5-row terminal survive the replay");
    assert_round_trips(&mut t, "spike 1");
}

#[test]
fn spike_alt_screen_composite_matches_while_active_and_after_exit() {
    let mut t = vt(20, 5);
    t.feed(lines(12).as_bytes());
    t.feed(b"\x1b[?1049h\x1b[2J\x1b[HALT SCREEN TUI");
    assert!(t.alternate_on());
    let snap = t.snapshot(BIG);
    assert_eq!(snap.primary, Some(Fidelity::Full));
    let mut c = replay(&snap);
    assert!(c.alternate_on(), "the client lands on the alternate screen");
    assert_eq!(c.plain_screen(), "ALT SCREEN TUI");
    // The alt screen replays exactly: compare the formatter's own output.
    assert_eq!(
        String::from_utf8_lossy(&c.term.format(Format::Vt).unwrap()),
        String::from_utf8_lossy(&t.term.format(Format::Vt).unwrap()),
        "alt VT equal"
    );
    // The TUI exits on both: the primary and all its history come back.
    t.feed(b"\x1b[?1049l");
    c.feed(b"\x1b[?1049l");
    assert_eq!(observe(&mut c), observe(&mut t), "primary + history equal after exit");
    assert!(t.plain_scrollback().starts_with("line 1\n"));
}

// ---- round-trip property ---------------------------------------------------

/// xorshift64*: deterministic, so a failure names its seed.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A stream of what agents print: text, SGR in all its colour forms, cursor
/// motion, line feeds that scroll into history, tabs and a few mode flips.
/// `rich` adds wide (CJK) characters and line erases.
fn agent_stream(rng: &mut Rng, ops: usize, rich: bool) -> Vec<u8> {
    const WORDS: [&str; 8] = ["alpha", "beta", "gamma", "δέλτα", "漢字", "x", "  ", "end."];
    let mut out = Vec::new();
    for _ in 0..ops {
        let piece = match rng.below(16) {
            0..=3 => match WORDS[rng.below(WORDS.len() as u64) as usize] {
                "漢字" if !rich => "kanji".to_string(),
                w => w.to_string(),
            },
            4 => "\r\n".to_string(),
            5 => format!("\x1b[{}m", [0, 1, 2, 3, 4, 7, 9, 22, 23, 24, 27][rng.below(11) as usize]),
            6 => format!("\x1b[{}m", 30 + rng.below(8)),
            7 => format!("\x1b[38;5;{}m", rng.below(256)),
            8 => format!("\x1b[48;2;{};{};{}m", rng.below(256), rng.below(256), rng.below(256)),
            9 => format!("\x1b[{};{}H", 1 + rng.below(6), 1 + rng.below(24)),
            10 => format!("\x1b[{}{}", 1 + rng.below(3), ['A', 'B', 'C', 'D'][rng.below(4) as usize]),
            11 if rich => format!("\x1b[{}K", rng.below(3)),
            11 | 12 => "\r".to_string(),
            13 => ["\x1b[?2004h", "\x1b[?2004l", "\x1b[?1h", "\x1b[?1l", "\x1b[?25l", "\x1b[?25h"][rng.below(6) as usize].to_string(),
            14 => lines(1 + rng.below(4) as usize),
            _ => "\t".to_string(),
        };
        out.extend_from_slice(piece.as_bytes());
    }
    out
}

/// Byte equality that reports where the first difference is.
fn assert_same_bytes(got: &[u8], want: &[u8], context: &str) {
    if got == want {
        return;
    }
    let at = got.iter().zip(want).position(|(a, b)| a != b).unwrap_or_else(|| got.len().min(want.len()));
    let around = |b: &[u8]| String::from_utf8_lossy(&b[at.saturating_sub(60)..(at + 60).min(b.len())]).into_owned();
    panic!(
        "VT re-snapshot differs at byte {at}\n   got: {:?}\n  want: {:?}\n{context}",
        around(got),
        around(want)
    );
}

/// Drop `ESC[48;…m <spaces> ESC[0m` runs (after a reset) that end a row: the
/// background-only blanks BCE leaves (see the property test).
fn without_bg_blank_runs(b: &[u8]) -> Vec<u8> {
    const OPEN: &[u8] = b"\x1b[48;";
    const CLOSE: &[u8] = b"\x1b[0m";
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(OPEN) && out.ends_with(b"\x1b[0m") {
            let mut j = i + OPEN.len();
            while b.get(j).is_some_and(|c| c.is_ascii_digit() || *c == b';') {
                j += 1;
            }
            if b.get(j) == Some(&b'm') {
                let mut k = j + 1;
                while b.get(k) == Some(&b' ') {
                    k += 1;
                }
                let end = k + CLOSE.len();
                if k > j + 1 && b[k..].starts_with(CLOSE) && (b[end..].starts_with(b"\r\n") || end == b.len() || b[end] == 0x1b) {
                    i = end;
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    // A removed run can leave its neighbours' resets back to back.
    let mut collapsed: Vec<u8> = Vec::with_capacity(out.len());
    for &c in &out {
        collapsed.push(c);
        if collapsed.ends_with(b"\x1b[0m\x1b[0m") {
            collapsed.truncate(collapsed.len() - CLOSE.len());
        }
    }
    // … or a reset right before a row break, where it changes nothing.
    let mut done: Vec<u8> = Vec::with_capacity(collapsed.len());
    for &c in &collapsed {
        done.push(c);
        if done.ends_with(b"\x1b[0m\r\n") {
            let n = done.len();
            done.drain(n - 6..n - 2);
        }
    }
    done
}

/// Feed a seeded agent stream in random chunk sizes (chunking must not
/// matter), snapshot, replay, and return both ends plus a failure context.
fn property_case(seed: u64, rich: bool) -> (Vt, Vt, Snapshot, String) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let (cols, rows) = (10 + rng.below(30) as u16, 3 + rng.below(8) as u16);
    let mut t = vt(cols, rows);
    let ops = 40 + rng.below(200) as usize;
    let stream = agent_stream(&mut rng, ops, rich);
    let mut i = 0;
    while i < stream.len() {
        let n = (1 + rng.below(64) as usize).min(stream.len() - i);
        t.feed(&stream[i..i + n]);
        i += n;
    }
    let context = format!("seed {seed} ({cols}x{rows}): {:?}", String::from_utf8_lossy(&stream));
    let snap = t.snapshot(BIG);
    assert_eq!(snap.screen, Fidelity::Full, "{context}");
    let client = replay(&snap);
    (t, client, snap, context)
}

/// The replay is a fixed point from the client on: re-snapshotting a client
/// and replaying that gives the same bytes again.
fn assert_fixed_point(client: &mut Vt, context: &str) -> Snapshot {
    let second = client.snapshot(BIG);
    let mut client2 = replay(&second);
    assert_eq!(client2.snapshot(BIG).bytes, second.bytes, "{context}: replay is not a fixed point");
    second
}

#[test]
fn round_trip_property_random_agent_streams() {
    // FLOW_VT_SEEDS=3000 for a longer soak; 80 per flavour by default.
    let seeds = std::env::var("FLOW_VT_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(80u64);
    for rich in [false, true] {
        for seed in 1..=seeds {
            let (mut t, mut client, snap, context) = property_case(seed, rich);
            assert_eq!(observe(&mut client), observe(&mut t), "{context}");
            let second = assert_fixed_point(&mut client, &context);
            // Byte-exact VT equality, modulo one libghostty-vt behaviour:
            // text that wraps at the bottom row while a background colour is
            // set. The replay's wrap scrolls, and the scrolled-in row is
            // filled with that background (BCE), so the client gains
            // invisible coloured blanks after the wrapped row. Everything
            // observable matched above.
            assert_same_bytes(&without_bg_blank_runs(&second.bytes), &without_bg_blank_runs(&snap.bytes), &context);
        }
    }
}

#[test]
fn round_trip_history_longer_than_screen() {
    let mut t = vt(30, 4);
    t.feed(lines(500).as_bytes());
    t.feed(b"\x1b[32mprompt$ \x1b[0m");
    assert!(t.plain_scrollback().lines().count() > 400);
    assert_round_trips(&mut t, "500 lines on 4 rows");
}

#[test]
fn round_trip_each_formatter_extra() {
    let cases: &[(&str, &[u8])] = &[
        ("modes", b"\x1b[?2004h\x1b[?1h\x1b[?1000h\x1b[?1006h\x1b[?7l"),
        ("scrolling region", b"\x1b[2;4r\x1b[3;1Hinside"),
        ("cursor + pen", b"\x1b[3;7H\x1b[1;4;35mpen"),
        ("tab stops", b"\x1b[3g\x1b[1;5H\x1bH\x1b[1;12H\x1bH\x1b[1;1H\tA\tB"),
        ("charsets", b"\x1b(0lqk\x1b(B"),
        ("kitty keyboard", b"\x1b[>5u"),
        ("modifyOtherKeys", b"\x1b[>4;2m"),
        ("pwd", b"\x1b]7;file://host/tmp/work\x1b\\"),
        ("hyperlink", b"\x1b]8;;https://smoo.ai\x1b\\link\x1b]8;;\x1b\\"),
        ("protection", b"\x1b[1\"qprot\x1b[0\"q"),
        ("region + tab stops + cursor", b"\x1b[3g\x1b[1;4H\x1bH\x1b[2;5r\x1b[4;3H\x1b[1;41mX"),
        ("origin mode", b"\x1b[2;5r\x1b[?6h\x1b[2;2Ho"),
        ("pending wrap", b"\x1b[1;20Hz"),
        ("blank rows below", b"\r\n\r\n\r\n"),
        ("cleared screen", b"\x1b[H\x1b[2J"),
    ];
    for (name, bytes) in cases {
        let mut t = vt(20, 6);
        t.feed(b"before\r\n");
        t.feed(bytes);
        t.feed(b"after");
        assert_round_trips(&mut t, name);
    }
}

#[test]
fn round_trip_carries_input_modes_the_keys_depend_on() {
    let mut t = vt(20, 4);
    t.feed(b"\x1b[?1h\x1b[>1u");
    let snap = t.snapshot(BIG);
    let mut c = replay(&snap);
    assert!(c.cursor_keys_application());
    assert_eq!(c.encode_key("Up"), t.encode_key("Up"));
    assert_eq!(c.encode_key("Escape"), t.encode_key("Escape"), "Kitty flags survive the replay");
}

// ---- the alternate screen --------------------------------------------------

#[test]
fn alt_screen_each_enter_mode() {
    for (on, off) in [("\x1b[?1049h", "\x1b[?1049l"), ("\x1b[?1047h", "\x1b[?1047l"), ("\x1b[?47h", "\x1b[?47l")] {
        let mut t = vt(20, 5);
        t.feed(lines(9).as_bytes());
        t.feed(b"shell$ ");
        t.feed(on.as_bytes());
        t.feed(b"\x1b[2J\x1b[3;3H\x1b[7mTUI\x1b[0m");
        let snap = t.snapshot(BIG);
        let mut c = replay(&snap);
        assert!(c.alternate_on(), "{on:?}");
        assert_eq!(
            String::from_utf8_lossy(&c.term.format(Format::Vt).unwrap()),
            String::from_utf8_lossy(&t.term.format(Format::Vt).unwrap()),
            "{on:?}: alt screen"
        );
        assert_eq!(c.cursor(), t.cursor(), "{on:?}");
        t.feed(off.as_bytes());
        c.feed(off.as_bytes());
        assert_eq!(observe(&mut c), observe(&mut t), "{on:?}: after exit");
    }
}

#[test]
fn alt_enter_split_across_feeds_at_every_byte() {
    let before = lines(15);
    let seq = b"\x1b[?1049h";
    let mut whole = vt(20, 5);
    whole.feed(before.as_bytes());
    whole.feed(seq);
    whole.feed(b"ALT");
    let reference = whole.snapshot(BIG);
    for split in 1..seq.len() {
        let mut t = vt(20, 5);
        t.feed(before.as_bytes());
        t.feed(&seq[..split]);
        t.feed(&seq[split..]);
        t.feed(b"ALT");
        assert_eq!(t.snapshot(BIG), reference, "split after {split} bytes");
    }
    // And with the text before it in the same chunk as the first half.
    let mut t = vt(20, 5);
    let mut first = before.into_bytes();
    first.extend_from_slice(&seq[..4]);
    t.feed(&first);
    let mut second = seq[4..].to_vec();
    second.extend_from_slice(b"ALT");
    t.feed(&second);
    assert_eq!(t.snapshot(BIG), reference);
}

#[test]
fn alt_enter_and_exit_in_one_chunk() {
    let mut t = vt(20, 5);
    t.feed(lines(10).as_bytes());
    t.feed(b"\x1b[?1049hTUI\x1b[?1049lback");
    assert!(!t.alternate_on());
    assert!(t.primary.is_none(), "cache dropped on exit");
    assert_round_trips(&mut t, "enter+exit in one chunk");
    assert!(t.plain_scrollback().starts_with("line 1\n"));
}

#[test]
fn reenter_while_alt_keeps_the_first_primary() {
    let mut t = vt(20, 5);
    t.feed(b"primary text\r\n");
    t.feed(b"\x1b[?1049hone");
    t.feed(b"\x1b[?1049htwo");
    let mut c = replay(&t.snapshot(BIG));
    c.feed(b"\x1b[?1049l");
    t.feed(b"\x1b[?1049l");
    assert_eq!(c.plain_screen(), "primary text");
    assert_eq!(observe(&mut c), observe(&mut t));
}

#[test]
fn full_reset_while_alt_drops_the_cache() {
    let mut t = vt(20, 5);
    t.feed(b"primary\r\n\x1b[?1049hTUI");
    assert!(t.primary.is_some());
    t.feed(b"\x1bc");
    assert!(!t.alternate_on());
    assert!(t.primary.is_none());
    assert_eq!(t.snapshot(BIG).primary, None);
}

#[test]
fn alt_snapshot_without_a_seen_enter_has_empty_primary() {
    // DECSET 1049 via a parameter list the scanner does see …
    let mut t = vt(20, 5);
    t.feed(b"p\r\n\x1b[?25;1049hTUI");
    assert!(t.primary.is_some());
    // … and the composite still lands the client on the alt screen when the
    // cache is missing (forced here).
    t.primary = None;
    let snap = t.snapshot(BIG);
    assert_eq!(snap.primary, Some(Fidelity::Empty));
    let mut c = replay(&snap);
    assert!(c.alternate_on());
    assert_eq!(c.plain_screen(), t.plain_screen());
    assert_eq!(c.cursor(), t.cursor());
}

// ---- bounding --------------------------------------------------------------

#[test]
fn budget_is_never_exceeded() {
    let mut t = vt(40, 6);
    t.feed(lines(300).as_bytes());
    t.feed(b"\x1b[1;33mtail");
    let full = t.snapshot(BIG).bytes.len();
    for max in [0, 1, 7, 15, 40, 100, 200, 500, 1000, full / 2, full - 1, full, full + 1] {
        let s = t.snapshot(max);
        assert!(s.bytes.len() <= max, "max {max}: {} bytes ({:?})", s.bytes.len(), s.screen);
    }
}

#[test]
fn budget_keeps_the_newest_rows() {
    let mut t = vt(40, 5);
    t.feed(lines(300).as_bytes());
    t.feed(b"prompt$ ");
    let full = t.snapshot(BIG).bytes.len();
    let s = t.snapshot(full / 3);
    let Fidelity::History { dropped_rows } = s.screen else {
        panic!("{:?}", s.screen)
    };
    assert!(dropped_rows > 0 && dropped_rows < 296, "{dropped_rows}");
    let mut c = replay(&s);
    assert_eq!(c.plain_screen(), t.plain_screen(), "the visible screen is always whole");
    assert_eq!(c.cursor(), t.cursor());
    let kept = c.plain_scrollback();
    assert!(kept.ends_with("line 300\nprompt$"), "{kept}");
    assert!(!kept.contains("line 1\n"), "the oldest rows went first");
    // Exactly the newest rows: the client's history is a suffix of the source's.
    assert!(t.plain_scrollback().ends_with(&kept));
    // A tighter budget keeps fewer rows.
    let tighter = t.snapshot(full / 6);
    let Fidelity::History { dropped_rows: more } = tighter.screen else { panic!() };
    assert!(more > dropped_rows);
}

#[test]
fn budget_smaller_than_the_screen_falls_back_to_plain_then_empty() {
    let mut t = vt(20, 3);
    t.feed(b"\x1b[31mred\x1b[0m\r\n\x1b[1;32mgreen\x1b[0m");
    let styled = t.snapshot(BIG).bytes.len();
    let plain_len = "red\r\ngreen\x1b[2;6H".len();
    assert!(plain_len < styled);
    let s = t.snapshot(plain_len);
    assert_eq!(s.screen, Fidelity::Plain);
    let mut c = replay(&s);
    assert_eq!(c.plain_screen(), "red\ngreen");
    assert_eq!(c.cursor(), t.cursor());
    let s = t.snapshot(plain_len - 1);
    assert_eq!((s.screen, s.bytes.len()), (Fidelity::Empty, 0));
}

#[test]
fn a_row_bigger_than_the_budget() {
    // One visible row of 'e' + 15 combining accents per cell: the screen
    // alone is several KB.
    let mut t = vt(80, 2);
    let heavy: String = std::iter::repeat_n("e\u{301}\u{302}\u{303}\u{304}\u{305}\u{306}\u{307}\u{308}", 80).collect();
    t.feed(heavy.as_bytes());
    let s = t.snapshot(512);
    assert!(s.bytes.len() <= 512);
    assert!(matches!(s.screen, Fidelity::Empty | Fidelity::Plain), "{:?}", s.screen);
    replay(&s); // parses
}

#[test]
fn budget_with_alt_screen_prioritizes_the_alt_screen() {
    let mut t = vt(40, 5);
    t.feed(lines(300).as_bytes());
    t.feed(b"\x1b[?1049h\x1b[2J\x1b[HTUI");
    let full = t.snapshot(BIG);
    assert_eq!(full.primary, Some(Fidelity::Full));
    let s = t.snapshot(full.bytes.len() / 3);
    assert_eq!(s.screen, Fidelity::Full);
    assert!(matches!(s.primary, Some(Fidelity::History { .. })), "{:?}", s.primary);
    let mut c = replay(&s);
    assert!(c.alternate_on());
    assert_eq!(c.plain_screen(), "TUI");
    c.feed(b"\x1b[?1049l");
    t.feed(b"\x1b[?1049l");
    assert_eq!(c.plain_screen(), t.plain_screen());
    assert!(t.plain_scrollback().ends_with(&c.plain_scrollback()));
    // Too small for any primary: still on the alt screen.
    let tiny = 40;
    let mut t2 = vt(40, 5);
    t2.feed(b"p\r\n\x1b[?1049hTUI");
    let s = t2.snapshot(tiny);
    assert!(s.bytes.len() <= tiny);
    let mut c = replay(&s);
    assert!(c.alternate_on(), "{:?}", String::from_utf8_lossy(&s.bytes));
    // Below the preamble: nothing at all.
    assert_eq!(t2.snapshot(ALT_PREAMBLE_MAX - 1).bytes, Vec::<u8>::new());
}

// ---- resize ----------------------------------------------------------------

#[test]
fn resize_then_snapshot_round_trips() {
    for (cols, rows) in [(10, 3), (60, 12), (25, 5)] {
        let mut t = vt(30, 6);
        t.feed(lines(40).as_bytes());
        t.feed(b"a long line that will reflow when the width changes, then a prompt$ ");
        t.resize(cols, rows).unwrap();
        assert_eq!(t.size(), (cols, rows));
        let snap = t.snapshot(BIG);
        assert_eq!((snap.cols, snap.rows), (cols, rows));
        assert_round_trips(&mut t, &format!("{cols}x{rows}"));
    }
}

#[test]
fn resize_while_alt_reflows_the_cached_primary_too() {
    let mut t = vt(30, 6);
    t.feed(lines(20).as_bytes());
    t.feed(b"a line of twenty-six chars\r\n$ ");
    t.feed(b"\x1b[?1049hTUI");
    t.resize(12, 4).unwrap();
    let mut c = replay(&t.snapshot(BIG));
    t.feed(b"\x1b[?1049l");
    c.feed(b"\x1b[?1049l");
    // Screen and history reflow identically. The cursor `?1049l` restores
    // may not: libghostty-vt keeps the cursor `?1049h` saved through a
    // resize by its own rules, while the replay saved it after the cached
    // primary had already reflowed. The next prompt redraw fixes it.
    let (mut co, mut to) = (observe(&mut c), observe(&mut t));
    co.cursor = (0, 0);
    to.cursor = (0, 0);
    assert_eq!(co, to);
}

#[test]
fn resize_while_alt_keeps_soft_wrapped_text_but_not_its_joins() {
    // The cache holds rows as laid out, so a line that was ALREADY
    // soft-wrapped when the TUI came up reflows as its separate rows after a
    // resize during the TUI. Nothing is lost; the line breaks differ until
    // the program redraws. (Without a resize it is exact; see above.)
    let mut t = vt(30, 6);
    t.feed(b"a long line that will reflow when the width changes\r\n$ ");
    t.feed(b"\x1b[?1049hTUI");
    t.resize(12, 4).unwrap();
    let mut c = replay(&t.snapshot(BIG));
    t.feed(b"\x1b[?1049l");
    c.feed(b"\x1b[?1049l");
    let squash = |s: String| s.split_whitespace().collect::<String>();
    assert_eq!(squash(rows(&mut c)), squash(rows(&mut t)));
}

#[test]
fn invalid_sizes_are_refused() {
    assert_eq!(Vt::new(0, 5, 10).unwrap_err(), Error::InvalidSize { cols: 0, rows: 5 });
    assert_eq!(Vt::new(5, 0, 10).unwrap_err(), Error::InvalidSize { cols: 5, rows: 0 });
    let mut t = vt(10, 3);
    assert_eq!(t.resize(0, 3), Err(Error::InvalidSize { cols: 0, rows: 3 }));
    assert_eq!(t.size(), (10, 3), "unchanged");
}

#[test]
fn zero_scrollback_keeps_no_history() {
    let mut t = Vt::new(10, 3, 0).unwrap();
    t.feed(lines(10).as_bytes());
    assert_eq!(t.plain_scrollback(), t.plain_screen());
    let mut c = Vt::new(10, 3, 0).unwrap();
    c.feed(&t.snapshot(BIG).bytes);
    assert_eq!(c.plain_screen(), t.plain_screen());
}

// ---- adversarial input -----------------------------------------------------

#[test]
fn adversarial_input_never_panics_and_snapshots_stay_bounded() {
    let mut nasties: Vec<Vec<u8>> = vec![
        vec![0xff, 0xfe, 0xc3, 0x28, 0xe2, 0x82, 0xf0, 0x9f, 0x92],
        b"\x1b[".iter().copied().chain(std::iter::repeat_n(b'9', 100_000)).chain(*b"m").collect(),
        b"\x1b[?".iter().copied().chain(std::iter::repeat_n(b';', 50_000)).chain(*b"1049h").collect(),
        b"\x1b]0;".iter().copied().chain(std::iter::repeat_n(b'A', 200_000)).collect(), // unterminated OSC
        b"\x1bP".iter().copied().chain(std::iter::repeat_n(b'q', 10_000)).collect(),    // unterminated DCS
        vec![0; 4096],
        b"\x1b[99999;99999H\x1b[99999A\x1b[-5B\x1b[0;0r\x1b[999;1r".to_vec(),
        b"\x1b[?1049h\x1b[?1049h\x1b[?1049l\x1b[?1049l\x1b[?47l".to_vec(),
        b"\x1b_Gf=100;AAAA\x1b\\".to_vec(),
    ];
    let mut rng = Rng(0xdead_beef);
    for _ in 0..20 {
        nasties.push((0..2048).map(|_| rng.next() as u8).collect());
    }
    for (i, bytes) in nasties.iter().enumerate() {
        let mut t = vt(20, 5);
        t.feed(b"before\r\n");
        // Whole, then byte-by-byte on a second terminal.
        t.feed(bytes);
        let mut u = vt(20, 5);
        u.feed(b"before\r\n");
        for b in bytes.iter().take(4096) {
            u.feed(std::slice::from_ref(b));
        }
        for v in [&mut t, &mut u] {
            v.feed(b"\x18\x1b\\after");
            let _ = (v.plain_screen(), v.plain_scrollback(), v.cursor(), v.title(), v.take_replies());
            for max in [0, 64, 4096, BIG] {
                let s = v.snapshot(max);
                assert!(s.bytes.len() <= max, "case {i}");
                replay(&s);
            }
        }
    }
}

// ---- scraping --------------------------------------------------------------

#[test]
fn plain_screen_is_capture_pane_shaped() {
    let mut t = vt(20, 6);
    t.feed(b"ab   \r\n\r\n  cd  \r\n");
    assert_eq!(t.plain_screen(), "ab\n\n  cd", "trailing spaces and blank rows trimmed");
    t.feed(lines(10).as_bytes());
    assert_eq!(t.plain_screen().lines().count(), 5, "visible rows only");
    assert!(!t.plain_screen().contains("ab"));
    // Soft wraps are not joined (capture-pane without -J) …
    let mut w = vt(5, 3);
    w.feed(b"abcdefgh");
    assert_eq!(w.plain_screen(), "abcde\nfgh");
    // … but the scrollback view joins them.
    assert_eq!(w.plain_scrollback(), "abcdefgh");
}

#[test]
fn cursor_title_and_modes() {
    let mut t = vt(20, 5);
    assert_eq!(t.title(), None);
    t.feed(b"\x1b]0;first\x07");
    assert_eq!(t.title().as_deref(), Some("first"));
    t.feed(b"\x1b]2;second\x1b\\");
    assert_eq!(t.title().as_deref(), Some("second"));
    t.feed(b"\x1b[3;7H");
    assert_eq!(t.cursor(), (6, 2));
    assert!(!t.bracketed_paste() && !t.cursor_keys_application() && !t.alternate_on());
    t.feed(b"\x1b[?2004h\x1b[?1h");
    assert!(t.bracketed_paste() && t.cursor_keys_application());
    t.feed(b"\x1b[?2004l\x1b[?1l");
    assert!(!t.bracketed_paste() && !t.cursor_keys_application());
}

#[test]
fn snapshot_carries_the_title() {
    let mut t = vt(20, 5);
    t.feed(b"\x1b]2;agent: thinking\x07hi");
    let c = &mut replay(&t.snapshot(BIG));
    assert_eq!(c.title().as_deref(), Some("agent: thinking"));
    assert_eq!(c.plain_screen(), "hi");
    // A title that would crowd the screen out of a small budget is dropped.
    let s = t.snapshot(40);
    assert!(!String::from_utf8_lossy(&s.bytes).contains("thinking"));
}

#[test]
fn stream_ground_tracks_partial_sequences() {
    let mut t = vt(20, 5);
    assert!(t.stream_is_ground());
    t.feed(b"\x1b[3");
    assert!(!t.stream_is_ground());
    t.feed(b"1m");
    assert!(t.stream_is_ground());
    t.feed(&[0xe6, 0xbc]); // half of 漢
    assert!(!t.stream_is_ground());
    t.feed(&[0xa2]);
    assert!(t.stream_is_ground());
}

#[test]
fn device_attribute_queries_get_answers() {
    let mut t = vt(20, 5);
    t.feed(b"\x1b[c");
    let reply = t.take_replies();
    assert!(reply.starts_with(b"\x1b[?62;"), "{:?}", String::from_utf8_lossy(&reply));
    assert!(t.take_replies().is_empty(), "drained");
    t.feed(b"\x1b[6n");
    assert_eq!(t.take_replies(), b"\x1b[1;1R");
}

// ---- input encoding --------------------------------------------------------

#[test]
fn paste_plain_and_bracketed() {
    let mut t = vt(20, 5);
    assert_eq!(t.encode_paste("echo hi\nls\tx"), b"echo hi\rls\tx", "newlines become CR; tabs survive");
    t.feed(b"\x1b[?2004h");
    assert_eq!(t.encode_paste("echo hi\nls"), b"\x1b[200~echo hi\nls\x1b[201~");
    assert_eq!(t.encode_paste(""), b"\x1b[200~\x1b[201~");
}

#[test]
fn paste_cannot_end_itself_early() {
    let mut t = vt(20, 5);
    t.feed(b"\x1b[?2004h");
    let out = t.encode_paste("safe\x1b[201~rm -rf /\x1b[200~\0\x7f");
    assert!(out.starts_with(b"\x1b[200~") && out.ends_with(b"\x1b[201~"));
    let inner = &out[6..out.len() - 6];
    assert!(!inner.contains(&0x1b), "{:?}", String::from_utf8_lossy(inner));
    assert!(!inner.contains(&0) && !inner.contains(&0x7f));
    assert_eq!(out.windows(6).filter(|w| w == b"\x1b[201~").count(), 1, "exactly one end marker");
    // Not bracketed: still no ESC reaches the program.
    t.feed(b"\x1b[?2004l");
    assert!(!t.encode_paste("a\x1b[201~b").contains(&0x1b));
    // Large pastes are not truncated.
    let big = "x".repeat(100_000);
    assert_eq!(t.encode_paste(&big).len(), 100_000);
}

fn key(t: &mut Vt, name: &str) -> Vec<u8> {
    t.encode_key(name).unwrap_or_else(|| panic!("{name:?} did not encode"))
}

#[test]
fn keys_legacy_encoding() {
    let mut t = vt(20, 5);
    let cases: &[(&str, &[u8])] = &[
        ("Enter", b"\r"),
        ("Escape", b"\x1b"),
        ("Tab", b"\t"),
        ("BTab", b"\x1b[Z"),
        ("BSpace", b"\x7f"),
        ("Space", b" "),
        ("Up", b"\x1b[A"),
        ("Down", b"\x1b[B"),
        ("Right", b"\x1b[C"),
        ("Left", b"\x1b[D"),
        ("Home", b"\x1b[H"),
        ("End", b"\x1b[F"),
        ("PageUp", b"\x1b[5~"),
        ("PPage", b"\x1b[5~"),
        ("PageDown", b"\x1b[6~"),
        ("NPage", b"\x1b[6~"),
        ("DC", b"\x1b[3~"),
        ("IC", b"\x1b[2~"),
        ("F1", b"\x1bOP"),
        ("F2", b"\x1bOQ"),
        ("F3", b"\x1bOR"),
        ("F4", b"\x1bOS"),
        ("F5", b"\x1b[15~"),
        ("F6", b"\x1b[17~"),
        ("F7", b"\x1b[18~"),
        ("F8", b"\x1b[19~"),
        ("F9", b"\x1b[20~"),
        ("F10", b"\x1b[21~"),
        ("F11", b"\x1b[23~"),
        ("F12", b"\x1b[24~"),
        ("y", b"y"),
        ("n", b"n"),
        ("Y", b"Y"),
        ("1", b"1"),
        ("2", b"2"),
        ("-", b"-"),
        ("/", b"/"),
        ("é", "é".as_bytes()),
        ("M-x", b"\x1bx"),
        ("M-Enter", b"\x1b\r"),
        ("S-Up", b"\x1b[1;2A"),
        ("C-Up", b"\x1b[1;5A"),
    ];
    for (name, want) in cases {
        assert_eq!(String::from_utf8_lossy(&key(&mut t, name)), String::from_utf8_lossy(want), "{name}");
    }
    for (i, c) in ('a'..='z').enumerate() {
        let want = u8::try_from(i + 1).unwrap();
        assert_eq!(key(&mut t, &format!("C-{c}")), vec![want], "C-{c}");
    }
}

#[test]
fn keys_honour_cursor_key_mode() {
    let mut t = vt(20, 5);
    t.feed(b"\x1b[?1h");
    for (name, want) in [
        ("Up", "\x1bOA"),
        ("Down", "\x1bOB"),
        ("Right", "\x1bOC"),
        ("Left", "\x1bOD"),
        ("Home", "\x1bOH"),
        ("End", "\x1bOF"),
    ] {
        assert_eq!(String::from_utf8_lossy(&key(&mut t, name)), want, "{name} under DECCKM");
    }
    t.feed(b"\x1b[?1l");
    assert_eq!(key(&mut t, "Up"), b"\x1b[A");
}

#[test]
fn keys_honour_kitty_flags() {
    let mut t = vt(20, 5);
    t.feed(b"\x1b[>1u"); // disambiguate escape codes
    assert_eq!(String::from_utf8_lossy(&key(&mut t, "Escape")), "\x1b[27u");
    assert_eq!(String::from_utf8_lossy(&key(&mut t, "C-c")), "\x1b[99;5u");
    assert_eq!(key(&mut t, "Enter"), b"\r", "Enter stays legacy under disambiguate");
    assert_eq!(key(&mut t, "y"), b"y");
    t.feed(b"\x1b[<u"); // pop
    assert_eq!(key(&mut t, "Escape"), b"\x1b");
}

#[test]
fn unknown_key_names_encode_nothing() {
    let mut t = vt(20, 5);
    for bad in ["", "Bogus", "C-", "ab", "\n"] {
        assert_eq!(t.encode_key(bad), None, "{bad:?}");
    }
}

/// Every key name a shipped harness manifest or the engine's defaults use.
#[test]
fn every_manifest_key_name_encodes() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../smooth-flow/harnesses");
    let mut names: Vec<String> = vec![
        // crates/smooth-flow/src/harness.rs defaults and engine.rs.
        "Enter".into(),
        "1".into(),
        "2".into(),
        "Escape".into(),
        "C-c".into(),
    ];
    let mut manifests = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        manifests += 1;
        let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let Some(steer) = doc.get("steer").and_then(toml::Value::as_table) else {
            continue;
        };
        for (k, v) in steer {
            match (k.as_str(), v) {
                ("submit_key", toml::Value::String(s)) => names.push(s.clone()),
                (k, toml::Value::Array(a)) if k.ends_with("_keys") => names.extend(a.iter().filter_map(|x| x.as_str().map(str::to_string))),
                _ => {}
            }
        }
    }
    assert!(manifests >= 10, "found the harness manifests in {}", dir.display());
    names.sort();
    names.dedup();
    let mut t = vt(20, 5);
    for name in &names {
        assert!(t.encode_key(name).is_some_and(|b| !b.is_empty()), "manifest key {name:?}");
    }
    for expected in ["Enter", "Escape", "Down", "Right", "y", "n", "1", "2", "C-c"] {
        assert!(names.iter().any(|n| n == expected), "{expected} is still used — the scan found {names:?}");
    }
}

#[test]
fn vt_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Vt>();
}
