//! Pending composer attachments — image/PDF files the user pastes or
//! Ctrl+V's before sending, shipped as data-URL `images[]` on the canonical
//! `send_message` frame (pearl th-d16f7c).
//!
//! Wire parity with the web SPA composer (`operator.ts::Attachment`): the
//! attachment is a full `data:<mime>;base64,…` string, sent only when
//! non-empty so text-only turns are unchanged.

use std::path::{Path, PathBuf};

use base64::Engine;

/// Files larger than this are refused rather than silently truncated —
/// a 50MB scan pasted into a chat turn helps nobody.
const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// One attachment staged in the composer, not yet sent.
#[derive(Debug, Clone)]
pub struct PendingAttachment {
    /// Display name (file name, or `clipboard.png` for a pasted image).
    pub name: String,
    /// MIME type, derived from the extension.
    pub mime: String,
    /// Full `data:<mime>;base64,…` string — the wire value.
    pub data_url: String,
}

/// Extension → MIME for the types the backend accepts. `None` means
/// "not attachable" and the paste is treated as plain text.
fn mime_for(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "pdf" => Some("application/pdf"),
        _ => None,
    }
}

/// If a bracketed paste is a single path to an existing image/PDF —
/// what terminals emit when a file is dragged onto them — return that
/// path. Handles the two quoting conventions: `'…'`/`"…"` wrapping and
/// backslash-escaped spaces. Anything else (multi-line, non-file,
/// unknown extension) is `None` → insert as text like before.
pub fn attachable_path(pasted: &str) -> Option<PathBuf> {
    let line = pasted.trim();
    if line.is_empty() || line.contains('\n') {
        return None;
    }
    let unquoted = line
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| line.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .map_or_else(|| line.replace("\\ ", " "), ToString::to_string);
    let expanded = if let Some(rest) = unquoted.strip_prefix("~/") {
        dirs_next::home_dir()?.join(rest)
    } else {
        PathBuf::from(&unquoted)
    };
    if mime_for(&expanded).is_some() && expanded.is_file() {
        Some(expanded)
    } else {
        None
    }
}

/// Read a file into a [`PendingAttachment`]. Errors are user-facing
/// strings (shown as a system chat line), not `anyhow` — the composer
/// keeps running either way.
pub fn attach_file(path: &Path) -> Result<PendingAttachment, String> {
    let mime = mime_for(path).ok_or_else(|| format!("{}: not an attachable type (png/jpg/gif/webp/pdf)", path.display()))?;
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_BYTES {
        return Err(format!("{}: too large ({}MB, max 5MB)", path.display(), meta.len() / (1024 * 1024)));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path.file_name().map_or_else(|| "attachment".into(), |n| n.to_string_lossy().into_owned());
    Ok(from_bytes(name, mime, &bytes))
}

/// Build an attachment from raw bytes (also the clipboard path).
fn from_bytes(name: String, mime: &str, bytes: &[u8]) -> PendingAttachment {
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    PendingAttachment {
        name,
        mime: mime.to_string(),
        data_url: format!("data:{mime};base64,{b64}"),
    }
}

/// Grab an image off the OS clipboard, if there is one.
///
/// ponytail: macOS-only via `osascript` (`«class PNGf»`) — zero new deps, and
/// the sandbox/daemon story is macOS-first anyway (th-08e05a). Other platforms
/// return None; add `arboard` when a Linux/Windows TUI user actually asks.
pub fn clipboard_image() -> Option<PendingAttachment> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let tmp = std::env::temp_dir().join(format!("smooth-paste-{}.png", std::process::id()));
    let script = format!(
        "set fp to open for access POSIX file \"{}\" with write permission\n\
         set eof fp to 0\n\
         write (the clipboard as \u{ab}class PNGf\u{bb}) to fp\n\
         close access fp",
        tmp.display()
    );
    let ok = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return None; // no image on the clipboard (or osascript refused)
    }
    let result = std::fs::read(&tmp)
        .ok()
        .filter(|b| !b.is_empty())
        .map(|bytes| from_bytes("clipboard.png".into(), "image/png", &bytes));
    let _ = std::fs::remove_file(&tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachable_path_rejects_text_and_accepts_dragged_file_forms() {
        // Plain prose, multi-line, missing file, wrong extension → None.
        assert!(attachable_path("fix the bug in app.rs").is_none());
        assert!(attachable_path("a.png\nb.png").is_none());
        assert!(attachable_path("/nope/definitely/missing.png").is_none());

        let dir = std::env::temp_dir().join(format!("smooth-attach-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("shot 1.png");
        std::fs::write(&img, [0x89, b'P', b'N', b'G']).unwrap();
        let rs = dir.join("main.rs");
        std::fs::write(&rs, "fn main() {}").unwrap();

        let quoted = format!("'{}'", img.display());
        let escaped = img.display().to_string().replace(' ', "\\ ");
        assert_eq!(attachable_path(&quoted).unwrap(), img);
        assert_eq!(attachable_path(&escaped).unwrap(), img);
        // Existing file but not an attachable type → text paste.
        assert!(attachable_path(&rs.display().to_string()).is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn attach_file_builds_data_url_and_enforces_cap() {
        let dir = std::env::temp_dir().join(format!("smooth-attach-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("x.png");
        std::fs::write(&img, [1u8, 2, 3]).unwrap();

        let a = attach_file(&img).unwrap();
        assert_eq!(a.name, "x.png");
        assert_eq!(a.mime, "image/png");
        assert_eq!(
            a.data_url,
            format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3]))
        );

        let big = dir.join("big.pdf");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_BYTES + 1).unwrap();
        drop(f);
        assert!(attach_file(&big).unwrap_err().contains("too large"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
