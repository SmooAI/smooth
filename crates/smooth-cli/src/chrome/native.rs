//! Chrome native-messaging framing: a 4-byte native-endian length, then UTF-8 JSON.

use std::io::{self, Read, Write};

use serde_json::Value;

/// Chrome refuses host→extension messages over 1 MiB.
pub const MAX_TO_CHROME: usize = 1024 * 1024;
/// Our own cap on extension→host messages (Chrome allows 4 GiB; screenshots are the big ones).
pub const MAX_FROM_CHROME: usize = 64 * 1024 * 1024;

/// Read one framed message. `Ok(None)` on clean EOF (Chrome closed the port).
///
/// # Errors
/// I/O failure, an oversized frame, or a body that isn't JSON.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Value>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_ne_bytes(len) as usize;
    if len > MAX_FROM_CHROME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes exceeds {MAX_FROM_CHROME}"),
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Write one framed message and flush.
///
/// # Errors
/// I/O failure, or a message over Chrome's 1 MiB limit.
pub fn write_frame(w: &mut (impl Write + ?Sized), msg: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if body.len() > MAX_TO_CHROME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message of {} bytes exceeds Chrome's 1 MiB limit", body.len()),
        ));
    }
    let len = u32::try_from(body.len()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    w.write_all(&len.to_ne_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test idiom")]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_frames() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &json!({"id": 1, "method": "bookmarks.getTree"})).unwrap();
        write_frame(&mut buf, &json!({"id": 2})).unwrap();
        assert_eq!(&buf[..4], &u32::try_from(buf.len() - 4 - 12).unwrap().to_ne_bytes());
        let mut r = io::Cursor::new(buf);
        assert_eq!(read_frame(&mut r).unwrap().unwrap()["method"], "bookmarks.getTree");
        assert_eq!(read_frame(&mut r).unwrap().unwrap()["id"], 2);
        assert!(read_frame(&mut r).unwrap().is_none());
    }

    #[test]
    fn rejects_oversized_and_garbage() {
        let big = json!("x".repeat(MAX_TO_CHROME));
        assert!(write_frame(&mut Vec::new(), &big).is_err());

        let mut huge = (u32::try_from(MAX_FROM_CHROME).unwrap() + 1).to_ne_bytes().to_vec();
        huge.extend_from_slice(b"{}");
        assert!(read_frame(&mut io::Cursor::new(huge)).is_err());

        let mut bad = 3u32.to_ne_bytes().to_vec();
        bad.extend_from_slice(b"{{{");
        assert!(read_frame(&mut io::Cursor::new(bad)).is_err());
    }

    #[test]
    fn truncated_body_is_an_error_not_eof() {
        let mut short = 10u32.to_ne_bytes().to_vec();
        short.extend_from_slice(b"{}");
        assert!(read_frame(&mut io::Cursor::new(short)).is_err());
    }
}
