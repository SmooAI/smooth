//! Terminal query replies a client must not send to a `pty` session
//! (SMOODEV-4238, SmoothFlow.md § Replay, engine rule 7).
//!
//! On the `pty` host the session's headless VT answers the program's
//! queries (DA, DSR, DECRQM, the Kitty keyboard flags, XTVERSION, colour
//! queries, size reports) itself. A client terminal parsing the same output
//! answers them too, and its answer arrives as `flow.input`, after the
//! program already has the host's: stray bytes typed into a shell's line
//! editor or a TUI. tmux swallowed those answers, which is why every client
//! (the Mac app, the phones, SmoothFlow Desktop) still sends them.
//!
//! The Mac app now mirrors (its surface suppresses replies), but phones and
//! older apps in the field keep answering. So the engine drops an input
//! frame on a `pty` session when the WHOLE frame is one or more reply
//! sequences ([`is_reply_only`]). A terminal writes each answer as its own
//! write, so a reply is never mixed with typed bytes; a frame that holds
//! anything else passes untouched.
//!
//! Only sequences a keyboard, mouse or paste never produces count. The one
//! overlap is the cursor position report `CSI row;col R`, which a legacy
//! keyboard also sends for a modified F3 (`CSI 1;mod R`, mod 2–16): row 1
//! with a column of 2–16 is let through, so Shift+F3 always reaches the
//! program. The colour-scheme notice `CSI ? 997 ; n n` is not an answer
//! (the client's OS theme, which the host can't know) and passes too.

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// Whether `data` is nothing but terminal replies (and not empty).
#[must_use]
pub fn is_reply_only(data: &[u8]) -> bool {
    let mut rest = data;
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        match reply_len(rest) {
            Some(n) => rest = &rest[n..],
            None => return false,
        }
    }
    true
}

/// The length of the reply sequence `data` starts with, if it starts with one.
fn reply_len(data: &[u8]) -> Option<usize> {
    match data {
        [ESC, b'[', ..] => csi_reply(data),
        [ESC, b'P', ..] => dcs_reply(data),
        [ESC, b']', ..] => osc_reply(data),
        _ => None,
    }
}

/// `CSI [?>]? params [$]? final`: a reply when the prefix and final say so.
fn csi_reply(data: &[u8]) -> Option<usize> {
    let body = &data[2..];
    let (prefix, body) = match body.first() {
        Some(p @ (b'?' | b'>')) => (Some(*p), &body[1..]),
        _ => (None, body),
    };
    let digits = body.iter().take_while(|b| b.is_ascii_digit() || **b == b';').count();
    let params = std::str::from_utf8(&body[..digits]).ok()?;
    let nums: Vec<u32> = if params.is_empty() {
        Vec::new()
    } else {
        params.split(';').map(|p| p.parse().ok()).collect::<Option<_>>()?
    };
    let tail = &body[digits..];
    let consumed = 2 + usize::from(prefix.is_some()) + digits;
    // DECRPM: `CSI ? mode ; value $ y` and `CSI mode ; value $ y`.
    if let [b'$', b'y', ..] = tail {
        return (prefix != Some(b'>') && nums.len() == 2).then_some(consumed + 2);
    }
    let fin = *tail.first()?;
    let ok = match (prefix, fin) {
        // DA1 and DA2.
        (Some(b'?' | b'>'), b'c') => !nums.is_empty(),
        // The Kitty keyboard flags.
        (Some(b'?'), b'u') => nums.len() == 1,
        // DECXCPR.
        (Some(b'?'), b'R') => (2..=3).contains(&nums.len()),
        // DECDSR answers, but not the colour-scheme notice.
        (Some(b'?'), b'n') => nums.first().is_some_and(|&n| n != 997),
        // DSR status.
        (None, b'n') => nums == [0] || nums == [3],
        // CPR, minus the modified-F3 overlap.
        (None, b'R') => nums.len() == 2 && !(nums[0] == 1 && (2..=16).contains(&nums[1])),
        // XTWINOPS reports and the in-band size report (mode 2048).
        (None, b't') => matches!(nums.first(), Some(1..=9 | 48)),
        _ => false,
    };
    ok.then_some(consumed + 1)
}

/// `DCS … ST`: DA3 (`!|`), XTVERSION (`>|`), DECRQSS (`0$r` / `1$r`) and
/// XTGETTCAP (`0+r` / `1+r`).
fn dcs_reply(data: &[u8]) -> Option<usize> {
    let body = &data[2..];
    let known = body.starts_with(b"!|") || body.starts_with(b">|") || matches!(body, [b'0' | b'1', b'$' | b'+', b'r', ..]);
    if !known {
        return None;
    }
    let end = body.windows(2).position(|w| w == [ESC, b'\\'])?;
    Some(2 + end + 2)
}

/// `OSC Ps ; … (ST | BEL)`: the colour answers (OSC 4, 10–19), which carry
/// an `rgb:` spec. A clipboard read (OSC 52) is not answered by the host and
/// passes.
fn osc_reply(data: &[u8]) -> Option<usize> {
    let body = &data[2..];
    let (end, term) = body.iter().enumerate().find_map(|(i, b)| match b {
        &BEL => Some((i, 1)),
        &ESC if body.get(i + 1) == Some(&b'\\') => Some((i, 2)),
        _ => None,
    })?;
    let text = std::str::from_utf8(&body[..end]).ok()?;
    let (ps, rest) = text.split_once(';')?;
    let colour = match ps.parse::<u32>().ok()? {
        4 => rest.split_once(';').is_some_and(|(_, spec)| spec.starts_with("rgb:")),
        10..=19 => rest.starts_with("rgb:"),
        _ => false,
    };
    colour.then_some(2 + end + term)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reply_kind_the_host_answers_is_recognised() {
        let replies: &[&[u8]] = &[
            b"\x1b[?62;22c",                              // DA1
            b"\x1b[?1;2c",                                // DA1, a VT100
            b"\x1b[>1;10;0c",                             // DA2
            b"\x1bP!|00000000\x1b\\",                     // DA3
            b"\x1b[0n",                                   // DSR status
            b"\x1b[12;40R",                               // CPR
            b"\x1b[?12;40;1R",                            // DECXCPR
            b"\x1b[?10n",                                 // DECDSR printer
            b"\x1b[?2004;1$y",                            // DECRPM, private
            b"\x1b[4;2$y",                                // DECRPM, ANSI
            b"\x1b[?0u",                                  // Kitty flags
            b"\x1b[?15u",                                 // Kitty flags
            b"\x1bP>|ghostty 1.2.0\x1b\\",                // XTVERSION
            b"\x1bP1$r0m\x1b\\",                          // DECRQSS
            b"\x1bP1+r544e=787465726d\x1b\\",             // XTGETTCAP
            b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\",          // OSC 10, ST
            b"\x1b]11;rgb:0000/0000/0000\x07",            // OSC 11, BEL
            b"\x1b]4;1;rgb:cccc/0000/0000\x1b\\",         // OSC 4
            b"\x1b[8;40;120t",                            // XTWINOPS chars
            b"\x1b[4;800;1200t",                          // XTWINOPS pixels
            b"\x1b[48;40;120;800;1200t",                  // in-band size (2048)
            b"\x1b[?62;22c\x1b[?0u",                      // two in one write
            b"\x1b[>1;10;0c\x1b[0n\x1b]10;rgb:1/2/3\x07", // three
        ];
        for r in replies {
            assert!(is_reply_only(r), "{:?} is a reply", String::from_utf8_lossy(r));
        }
    }

    #[test]
    fn what_a_person_types_is_never_dropped() {
        let typed: &[&[u8]] = &[
            b"",
            b"a",
            b"ls -la\r",
            b"\r",
            b"\x7f",
            b"\x03",                           // Ctrl-C
            b"\x1b",                           // Esc
            b"\x1bx",                          // Alt-x
            b"\x1b[A",                         // Up
            b"\x1bOA",                         // Up, DECCKM
            b"\x1b[1;5A",                      // Ctrl-Up
            b"\x1b[13;2u",                     // Shift+Enter, Kitty
            b"\x1b[27;2;13~",                  // Shift+Enter, modifyOtherKeys
            b"\x1b[1;2R",                      // Shift+F3, legacy
            b"\x1b[1;5R",                      // Ctrl+F3
            b"\x1b[R",                         // F3, Kitty
            b"\x1b[15~",                       // F5
            b"\x1b[<0;10;5M",                  // SGR mouse press
            b"\x1b[M !!",                      // X10 mouse
            b"\x1b[I",                         // focus in
            b"\x1b[O",                         // focus out
            b"\x1b[200~hello\x1b[201~",        // bracketed paste
            b"\x1b[200~\x1b[?62;22c\x1b[201~", // a pasted reply is a paste
            b"\x1b[?997;1n",                   // colour-scheme notice
            b"\x1b]52;c;aGVsbG8=\x07",         // clipboard read answer
            b"\x1b[?62;22cx",                  // a reply plus a keystroke
            b"x\x1b[?62;22c",
            b"\x1b[?62;22", // truncated
            b"\x1bP>|unterminated",
            b"\x1b]10;rgb:1/2/3",
            b"\x1b[5n", // a DSR request, not an answer
            b"\x1b[6n",
            b"\x1b[c", // a DA request
            b"\x1b[>c",
            b"\x1b[?u",       // a Kitty flags query
            b"\x1b[18t",      // a size query
            b"\x1b[12;40;1R", // three-field CPR is not one
            b"\x1b[>1;2$y",
            b"\x1b]0;title\x07",
            b"\x1b]10;?\x07", // a colour query
        ];
        for t in typed {
            assert!(!is_reply_only(t), "{:?} is input", String::from_utf8_lossy(t));
        }
    }

    #[test]
    fn a_cursor_report_on_row_one_past_column_sixteen_is_a_reply() {
        assert!(is_reply_only(b"\x1b[1;17R"));
        assert!(is_reply_only(b"\x1b[1;1R"));
        assert!(is_reply_only(b"\x1b[2;2R"));
        assert!(!is_reply_only(b"\x1b[1;16R"));
    }

    #[test]
    fn adversarial_input_never_panics() {
        let mut seed = 0x2545_f491_u32;
        for len in 0..64 {
            for _ in 0..200 {
                let bytes: Vec<u8> = (0..len)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        // Bias towards the bytes the grammar cares about.
                        const ALPHABET: &[u8] = b"\x1b[]P?>;$+!|0123456789cnRuyt\x07\\rgb:";
                        ALPHABET[(seed as usize) % ALPHABET.len()]
                    })
                    .collect();
                let _ = is_reply_only(&bytes);
            }
        }
        let _ = is_reply_only(b"\x1b[99999999999999999999;1R");
        assert!(!is_reply_only(b"\x1b[99999999999999999999;1R"), "an overflowing param is not a reply");
    }
}
