//! `th files …` — the org file system (folders, files, shares) [ADR-060].
//!
//! Wraps the same HTTP surface the dashboard uses: folder/file CRUD, presigned
//! upload/download, deletion locks, and anonymous/tracked shares. Byte transfers
//! (upload PUT, download GET) go straight to the presigned S3 URL with a
//! bearer-less client — a presigned URL carries its own auth in the query, and
//! an extra `Authorization` header makes S3 reject the request.

use anstream::println;
use anyhow::{bail, Context, Result};
use chrono::{Duration, Utc};
use clap::{Subcommand, ValueEnum};
use owo_colors::OwoColorize;
use serde_json::json;

use super::{print_json, require_active_org, require_authed};

/// A lockable/shareable resource — a file or a folder.
#[derive(Clone, Copy, ValueEnum)]
pub enum Kind {
    File,
    Folder,
}

impl Kind {
    /// URL path segment for this resource kind.
    fn seg(self) -> &'static str {
        match self {
            Kind::File => "files",
            Kind::Folder => "folders",
        }
    }
}

#[derive(Subcommand)]
pub enum Cmd {
    /// List the folders and files in a folder (org root when `--folder` is omitted).
    Ls {
        /// Parent folder id; lists the org root when omitted.
        #[arg(long)]
        folder: Option<String>,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a folder.
    Mkdir {
        /// Folder name.
        name: String,
        /// Parent folder id (root when omitted).
        #[arg(long)]
        parent: Option<String>,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Upload a local file (presigned PUT). Prints the created file row.
    Upload {
        /// Path to the local file to upload.
        path: String,
        /// Destination folder id (root when omitted).
        #[arg(long)]
        folder: Option<String>,
        /// Override the stored file name (defaults to the local basename).
        #[arg(long)]
        name: Option<String>,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Download a file by id (writes to `--out`, or its name in the CWD).
    Download {
        /// The file id from `th files ls`.
        file_id: String,
        /// Output path (defaults to the file's stored name in the CWD).
        #[arg(long, short)]
        out: Option<String>,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Move (`--to-folder`) and/or rename (`--name`) a file.
    Mv {
        /// The file id.
        file_id: String,
        /// Destination folder id (pass `root` or an empty value to move to root).
        #[arg(long = "to-folder")]
        to_folder: Option<String>,
        /// New name.
        #[arg(long)]
        name: Option<String>,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Move (`--to-parent`) and/or rename (`--name`) a folder.
    Mvdir {
        /// The folder id.
        folder_id: String,
        /// New parent folder id (pass `root` or an empty value to move to root).
        #[arg(long = "to-parent")]
        to_parent: Option<String>,
        /// New name.
        #[arg(long)]
        name: Option<String>,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete a file.
    Rm {
        /// The file id.
        file_id: String,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete a folder and its contents (blocked if the subtree is deletion-locked).
    Rmdir {
        /// The folder id.
        folder_id: String,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Set a deletion lock on a file or folder (org admin only).
    Lock {
        /// `file` or `folder`.
        kind: Kind,
        /// The resource id.
        id: String,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Clear a deletion lock on a file or folder (org admin only).
    Unlock {
        /// `file` or `folder`.
        kind: Kind,
        /// The resource id.
        id: String,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create an anonymous share link for a file or folder.
    Share {
        /// `file` or `folder`.
        kind: Kind,
        /// The resource id.
        id: String,
        /// `view` or `download`.
        #[arg(long, default_value = "download")]
        permission: String,
        /// Optional password gate.
        #[arg(long)]
        password: Option<String>,
        /// Expire the link this many hours from now.
        #[arg(long = "expires-in-hours")]
        expires_in_hours: Option<i64>,
        /// Cap the number of downloads.
        #[arg(long = "max-downloads")]
        max_downloads: Option<u32>,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// List the shares on a file or folder.
    Shares {
        /// `file` or `folder`.
        kind: Kind,
        /// The resource id.
        id: String,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Revoke a share.
    Unshare {
        /// `file` or `folder`.
        kind: Kind,
        /// The resource id.
        id: String,
        /// The share id from `th files shares`.
        share_id: String,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Send a tracked email invite for an existing share.
    Invite {
        /// `file` or `folder`.
        kind: Kind,
        /// The resource id.
        id: String,
        /// The share id from `th files shares`.
        share_id: String,
        /// Recipient email address.
        email: String,
        /// `view` or `download`.
        #[arg(long, default_value = "download")]
        permission: String,
        /// Expire the invite this many hours from now.
        #[arg(long = "expires-in-hours")]
        expires_in_hours: Option<i64>,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::Ls { folder, org } => {
            let o = require_active_org(&client, org)?;
            let folders_path = match &folder {
                Some(f) => format!("/organizations/{o}/folders?parentFolderId={f}"),
                None => format!("/organizations/{o}/folders"),
            };
            let files_path = match &folder {
                Some(f) => format!("/organizations/{o}/files?folderId={f}"),
                None => format!("/organizations/{o}/files"),
            };
            let folders = client.get(&folders_path).await.context("GET folders")?;
            let files = client.get(&files_path).await.context("GET files")?;
            print_listing(&folders, &files);
        }
        Cmd::Mkdir { name, parent, org } => {
            let o = require_active_org(&client, org)?;
            let body = json!({ "name": name, "parentFolderId": parent });
            print_json(&client.post(&format!("/organizations/{o}/folders"), Some(&body)).await.context("POST folder")?);
        }
        Cmd::Upload { path, folder, name, org } => {
            let o = require_active_org(&client, org)?;
            upload(&client, &o, &path, folder.as_deref(), name.as_deref()).await?;
        }
        Cmd::Download { file_id, out, org } => {
            let o = require_active_org(&client, org)?;
            download(&client, &o, &file_id, out.as_deref()).await?;
        }
        Cmd::Mv { file_id, to_folder, name, org } => {
            let o = require_active_org(&client, org)?;
            let mut body = serde_json::Map::new();
            if let Some(dest) = to_folder {
                body.insert("folderId".into(), dest_folder_value(&dest));
            }
            if let Some(n) = name {
                body.insert("name".into(), json!(n));
            }
            if body.is_empty() {
                bail!("nothing to do — pass --to-folder and/or --name");
            }
            print_json(
                &client
                    .patch(&format!("/organizations/{o}/files/{file_id}"), &serde_json::Value::Object(body))
                    .await
                    .context("PATCH file")?,
            );
        }
        Cmd::Mvdir {
            folder_id,
            to_parent,
            name,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let mut body = serde_json::Map::new();
            if let Some(dest) = to_parent {
                body.insert("parentFolderId".into(), dest_folder_value(&dest));
            }
            if let Some(n) = name {
                body.insert("name".into(), json!(n));
            }
            if body.is_empty() {
                bail!("nothing to do — pass --to-parent and/or --name");
            }
            print_json(
                &client
                    .patch(&format!("/organizations/{o}/folders/{folder_id}"), &serde_json::Value::Object(body))
                    .await
                    .context("PATCH folder")?,
            );
        }
        Cmd::Rm { file_id, org } => {
            let o = require_active_org(&client, org)?;
            print_json(&client.delete(&format!("/organizations/{o}/files/{file_id}")).await.context("DELETE file")?);
        }
        Cmd::Rmdir { folder_id, org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .delete(&format!("/organizations/{o}/folders/{folder_id}"))
                    .await
                    .context("DELETE folder")?,
            );
        }
        Cmd::Lock { kind, id, org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .post(&format!("/organizations/{o}/{}/{id}/lock", kind.seg()), None)
                    .await
                    .context("POST lock")?,
            );
        }
        Cmd::Unlock { kind, id, org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .delete(&format!("/organizations/{o}/{}/{id}/lock", kind.seg()))
                    .await
                    .context("DELETE lock")?,
            );
        }
        Cmd::Share {
            kind,
            id,
            permission,
            password,
            expires_in_hours,
            max_downloads,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let body = json!({
                "permission": permission,
                "password": password,
                "expiresAt": expires_at(expires_in_hours),
                "maxDownloads": max_downloads,
            });
            let share = client
                .post(&format!("/organizations/{o}/{}/{id}/shares", kind.seg()), Some(&body))
                .await
                .context("POST share")?;
            if let Some(token) = share.get("token").and_then(|v| v.as_str()) {
                println!();
                println!("  {} {}", "Share link:".dimmed(), format!("https://smoo.ai/share/{token}").cyan().bold());
            }
            print_json(&share);
        }
        Cmd::Shares { kind, id, org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{o}/{}/{id}/shares", kind.seg()))
                    .await
                    .context("GET shares")?,
            );
        }
        Cmd::Unshare { kind, id, share_id, org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .delete(&format!("/organizations/{o}/{}/{id}/shares/{share_id}", kind.seg()))
                    .await
                    .context("DELETE share")?,
            );
        }
        Cmd::Invite {
            kind,
            id,
            share_id,
            email,
            permission,
            expires_in_hours,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let body = json!({
                "email": email,
                "permission": permission,
                "expiresAt": expires_at(expires_in_hours),
            });
            let recipient = client
                .post(&format!("/organizations/{o}/{}/{id}/shares/{share_id}/invite", kind.seg()), Some(&body))
                .await
                .context("POST invite")?;
            if let Some(token) = recipient.get("token").and_then(|v| v.as_str()) {
                println!();
                println!(
                    "  {} {}",
                    "Invite link:".dimmed(),
                    format!("https://smoo.ai/share/recipient/{token}").cyan().bold()
                );
            }
            print_json(&recipient);
        }
    }
    Ok(())
}

/// `root` / empty → move to org root (null); anything else → that folder id.
fn dest_folder_value(dest: &str) -> serde_json::Value {
    if dest.is_empty() || dest == "root" {
        serde_json::Value::Null
    } else {
        json!(dest)
    }
}

/// ISO-8601 timestamp `hours` from now, or null when no expiry was requested.
fn expires_at(hours: Option<i64>) -> serde_json::Value {
    match hours {
        Some(h) => json!((Utc::now() + Duration::hours(h)).to_rfc3339()),
        None => serde_json::Value::Null,
    }
}

/// Presigned upload: create the file row + PUT the bytes to S3.
async fn upload(client: &smooth_api_client::SmoothApiClient, org: &str, path: &str, folder: Option<&str>, name_override: Option<&str>) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("read {path}"))?;
    let basename = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("cannot derive a file name from {path}"))?;
    let name = name_override.unwrap_or(basename);
    let extension = std::path::Path::new(name).extension().and_then(|s| s.to_str());
    let mime = guess_mime(name);

    let body = json!({
        "name": name,
        "mimeType": mime,
        "size": bytes.len(),
        "folderId": folder,
        "extension": extension,
    });
    let resp = client
        .post(&format!("/organizations/{org}/files/upload-url"), Some(&body))
        .await
        .context("POST upload-url")?;
    let upload_url = resp
        .get("uploadUrl")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("upload-url response missing uploadUrl"))?;

    // Bearer-less client: the presigned URL is self-authorizing; an extra
    // Authorization header makes S3 400.
    let put = reqwest::Client::new()
        .put(upload_url)
        .header(reqwest::header::CONTENT_TYPE, mime)
        .body(bytes)
        .send()
        .await
        .context("PUT to presigned URL")?;
    if !put.status().is_success() {
        let status = put.status();
        let text = put.text().await.unwrap_or_default();
        bail!("upload PUT failed: HTTP {status}: {text}");
    }

    print_json(resp.get("file").unwrap_or(&resp));
    Ok(())
}

/// Presigned download: resolve the URL, GET the bytes, write to disk.
async fn download(client: &smooth_api_client::SmoothApiClient, org: &str, file_id: &str, out: Option<&str>) -> Result<()> {
    // Fetch metadata (for a default output name) and the presigned URL.
    let listing = client.get(&format!("/organizations/{org}/files")).await.ok();
    let default_name = out.map(std::string::ToString::to_string).or_else(|| find_file_name(listing.as_ref(), file_id));

    let resp = client
        .get(&format!("/organizations/{org}/files/{file_id}/download"))
        .await
        .context("GET download URL")?;
    let url = resp
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("download response missing url"))?;

    let out_path = default_name.unwrap_or_else(|| format!("{file_id}.download"));
    let bytes = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .context("GET presigned URL")?
        .error_for_status()
        .context("download GET failed")?
        .bytes()
        .await
        .context("read download bytes")?;
    std::fs::write(&out_path, &bytes).with_context(|| format!("write {out_path}"))?;
    println!();
    println!("  {} {} ({} bytes)", "Downloaded →".dimmed(), out_path.cyan().bold(), bytes.len());
    println!();
    Ok(())
}

/// Best-effort lookup of a file's stored name from a `{files:[...]}` listing.
fn find_file_name(listing: Option<&serde_json::Value>, file_id: &str) -> Option<String> {
    listing?
        .get("files")?
        .as_array()?
        .iter()
        .find(|f| f.get("id").and_then(|v| v.as_str()) == Some(file_id))
        .and_then(|f| f.get("name").and_then(|v| v.as_str()))
        .map(std::string::ToString::to_string)
}

/// Minimal extension → MIME map (mirrors the browser's octet-stream fallback).
fn guess_mime(name: &str) -> &'static str {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

/// Print a folders-first, files-second listing from the two list envelopes.
fn print_listing(folders: &serde_json::Value, files: &serde_json::Value) {
    println!();
    let folder_arr = folders.get("folders").and_then(|v| v.as_array());
    let file_arr = files.get("files").and_then(|v| v.as_array());
    let empty = folder_arr.is_none_or(|a| a.is_empty()) && file_arr.is_none_or(|a| a.is_empty());
    if empty {
        println!("  {} {}", "●".dimmed(), "empty".dimmed());
        println!();
        return;
    }
    if let Some(arr) = folder_arr {
        for f in arr {
            let id = f.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let locked = f.get("deletionLocked").and_then(serde_json::Value::as_bool).unwrap_or(false);
            let suffix = if locked { " [locked]" } else { "" };
            println!("  {} {} {}/{}", "▸".cyan(), id.cyan(), name.bold(), suffix.dimmed());
        }
    }
    if let Some(arr) = file_arr {
        for f in arr {
            let id = f.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let size = f.get("size").and_then(serde_json::Value::as_u64).unwrap_or(0);
            let locked = f.get("deletionLocked").and_then(serde_json::Value::as_bool).unwrap_or(false);
            let suffix = if locked { " [locked]" } else { "" };
            println!(
                "  {} {} {} {}{}",
                "○".dimmed(),
                id.cyan(),
                name.bold(),
                format!("({size} B)").dimmed(),
                suffix.dimmed()
            );
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{dest_folder_value, find_file_name, guess_mime};

    #[test]
    fn dest_root_and_empty_map_to_null() {
        assert!(dest_folder_value("root").is_null());
        assert!(dest_folder_value("").is_null());
    }

    #[test]
    fn dest_id_maps_to_string() {
        assert_eq!(dest_folder_value("abc-123"), json!("abc-123"));
    }

    #[test]
    fn mime_from_extension() {
        assert_eq!(guess_mime("report.pdf"), "application/pdf");
        assert_eq!(guess_mime("photo.JPG"), "image/jpeg"); // case-insensitive
        assert_eq!(guess_mime("archive.tar.gz"), "application/octet-stream"); // unknown → octet-stream
        assert_eq!(guess_mime("noext"), "application/octet-stream");
    }

    #[test]
    fn find_file_name_matches_by_id() {
        let listing = json!({ "files": [{ "id": "a", "name": "one.txt" }, { "id": "b", "name": "two.pdf" }] });
        assert_eq!(find_file_name(Some(&listing), "b").as_deref(), Some("two.pdf"));
        assert_eq!(find_file_name(Some(&listing), "missing"), None);
        assert_eq!(find_file_name(None, "b"), None);
    }
}
