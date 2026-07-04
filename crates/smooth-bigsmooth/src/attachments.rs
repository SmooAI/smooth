//! Inbound chat attachments (pearl th-5295a2, Phase 2).
//!
//! Web sends `{ name, mime, data }` where `data` is raw base64 (no
//! `data:` prefix). We decode, content-address by sha256, and cache the
//! bytes under `~/.smooth/attachments/{sha}.{ext}` so identical uploads
//! dedupe. Images additionally get a `data:` URL for the multimodal turn;
//! documents are marker-only this phase (ingestion is Phase 4).

use anyhow::{Context, Result};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// One inbound attachment as sent by the web client. `data` is raw
/// base64 with NO `data:` prefix (the wire contract — keep it exact).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub data: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Document,
}

impl AttachmentKind {
    /// Lower-case label used in the persisted `[attached <kind>: <name>]` marker.
    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Document => "document",
        }
    }
}

/// Images route to a vision model + `with_user_images`; everything else
/// is a document (marker-only this phase).
pub fn classify(mime: &str) -> AttachmentKind {
    if mime.starts_with("image/") {
        AttachmentKind::Image
    } else {
        AttachmentKind::Document
    }
}

pub struct StoredAttachment {
    pub path: PathBuf,
    pub data_url: String,
    pub kind: AttachmentKind,
}

/// Decode, content-address, cache under `~/.smooth/attachments/`, and
/// return the `data:` URL for the (possibly-image) attachment.
///
/// # Errors
/// Returns an error if the home dir can't be resolved, `att.data` isn't
/// valid base64, or the cache dir/file can't be created/written.
pub fn store_and_data_url(att: &Attachment) -> Result<StoredAttachment> {
    let dir = dirs_next::home_dir().context("no home dir")?.join(".smooth/attachments");
    store_and_data_url_in(&dir, att)
}

/// Testable seam — `store_and_data_url` with an explicit cache dir.
fn store_and_data_url_in(dir: &Path, att: &Attachment) -> Result<StoredAttachment> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(att.data.as_bytes())
        .context("invalid attachment data")?;

    let digest = Sha256::digest(&bytes);
    let mut sha = String::with_capacity(64);
    for b in digest {
        let _ = write!(sha, "{b:02x}");
    }
    let ext = ext_for(&att.mime, &att.name);

    std::fs::create_dir_all(dir).context("creating attachments dir")?;
    let path = dir.join(format!("{sha}.{ext}"));
    // Dedupe: content-addressed, so an existing file is byte-identical — skip the write.
    if !path.exists() {
        std::fs::write(&path, &bytes).context("writing attachment")?;
    }

    Ok(StoredAttachment {
        path,
        data_url: format!("data:{};base64,{}", att.mime, att.data),
        kind: classify(&att.mime),
    })
}

/// File extension from the mime, falling back to the filename's, then `bin`.
fn ext_for(mime: &str, name: &str) -> String {
    let by_mime = match mime {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/svg+xml" => Some("svg"),
        "application/pdf" => Some("pdf"),
        _ => None,
    };
    by_mime
        .map(str::to_string)
        .or_else(|| Path::new(name).extension().and_then(|e| e.to_str()).map(str::to_string))
        .unwrap_or_else(|| "bin".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_image_vs_document() {
        assert_eq!(classify("image/png"), AttachmentKind::Image);
        assert_eq!(classify("image/jpeg"), AttachmentKind::Image);
        assert_eq!(classify("application/pdf"), AttachmentKind::Document);
        assert_eq!(classify("text/plain"), AttachmentKind::Document);
        assert_eq!(classify(""), AttachmentKind::Document);
    }

    #[test]
    fn ext_from_mime_then_name_then_bin() {
        assert_eq!(ext_for("image/png", "whatever"), "png");
        assert_eq!(ext_for("image/jpeg", "x.png"), "jpg");
        assert_eq!(ext_for("application/octet-stream", "report.csv"), "csv");
        assert_eq!(ext_for("application/octet-stream", "noext"), "bin");
    }

    #[test]
    fn store_produces_data_url_and_roundtrips() {
        // 1x1 transparent PNG.
        let raw = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==")
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        let att = Attachment {
            name: "pixel.png".into(),
            mime: "image/png".into(),
            data: b64.clone(),
        };

        let dir = std::env::temp_dir().join(format!("smooth-att-test-{}", uuid::Uuid::new_v4().simple()));
        let stored = store_and_data_url_in(&dir, &att).expect("store");

        assert_eq!(stored.kind, AttachmentKind::Image);
        assert!(stored.data_url.starts_with("data:image/png;base64,"));
        assert!(stored.data_url.ends_with(&b64));
        assert!(stored.path.starts_with(&dir));
        assert_eq!(stored.path.extension().and_then(|e| e.to_str()), Some("png"));

        // Bytes on disk decode back to the original.
        let on_disk = std::fs::read(&stored.path).unwrap();
        assert_eq!(on_disk, raw);

        // Second store is a no-op write (dedupe) but returns the same path.
        let again = store_and_data_url_in(&dir, &att).expect("store again");
        assert_eq!(again.path, stored.path);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_base64_errors() {
        let att = Attachment {
            name: "x.png".into(),
            mime: "image/png".into(),
            data: "!!!not base64!!!".into(),
        };
        let dir = std::env::temp_dir().join("smooth-att-test-badb64");
        assert!(store_and_data_url_in(&dir, &att).is_err());
    }
}
