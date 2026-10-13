//! `th chrome install`: unpack the extension, write the host wrapper and the
//! native-messaging host manifest that lets only our extension launch it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::json;

pub const HOST_NAME: &str = "ai.smoo.smooth_chrome";
/// Pinned by the `key` in the extension manifest, so it survives reloads and reinstalls.
pub const EXTENSION_ID: &str = "pgjpgmfljenhbbgkgjpmaghndenconlc";

pub const FILES: &[(&str, &str)] = &[
    ("manifest.json", include_str!("extension/manifest.json")),
    ("background.js", include_str!("extension/background.js")),
    ("popup.html", include_str!("extension/popup.html")),
    ("popup.js", include_str!("extension/popup.js")),
];

/// Where each browser looks for user-level native messaging host manifests.
pub fn manifest_dir(browser: &str) -> Result<PathBuf> {
    let home = dirs_next::home_dir().context("no home dir")?;
    let rel = if cfg!(target_os = "macos") {
        match browser {
            "chrome" => "Library/Application Support/Google/Chrome",
            "chrome-beta" => "Library/Application Support/Google/Chrome Beta",
            "chrome-canary" => "Library/Application Support/Google/Chrome Canary",
            "chromium" => "Library/Application Support/Chromium",
            "brave" => "Library/Application Support/BraveSoftware/Brave-Browser",
            "edge" => "Library/Application Support/Microsoft Edge",
            other => bail!("unknown browser '{other}' (chrome, chrome-beta, chrome-canary, chromium, brave, edge)"),
        }
    } else if cfg!(target_os = "linux") {
        match browser {
            "chrome" => ".config/google-chrome",
            "chrome-beta" => ".config/google-chrome-beta",
            "chromium" => ".config/chromium",
            "brave" => ".config/BraveSoftware/Brave-Browser",
            "edge" => ".config/microsoft-edge",
            other => bail!("unknown browser '{other}' (chrome, chrome-beta, chromium, brave, edge)"),
        }
    } else {
        bail!("th chrome supports macOS and Linux so far (Windows registers hosts in the registry)");
    };
    Ok(home.join(rel).join("NativeMessagingHosts"))
}

/// The `th` Chrome should launch: the PATH copy when it's this binary (a stable symlink across
/// upgrades), else this binary.
fn th_path() -> Result<PathBuf> {
    let me = std::env::current_exe()?;
    let canon = std::fs::canonicalize(&me).unwrap_or_else(|_| me.clone());
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join("th"))
        .find(|p| p.is_file());
    Ok(match on_path {
        Some(p) if std::fs::canonicalize(&p).is_ok_and(|c| c == canon) => p,
        _ => me,
    })
}

pub fn host_manifest(wrapper: &Path) -> serde_json::Value {
    json!({
        "name": HOST_NAME,
        "description": "Smooth (th) bridge for the Smooth Chrome extension",
        "path": wrapper,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{EXTENSION_ID}/")],
    })
}

pub struct Installed {
    pub extension_dir: PathBuf,
    pub manifest: PathBuf,
    pub th: PathBuf,
}

/// Write the extension, the host wrapper and the host manifest.
///
/// # Errors
/// Filesystem failure or an unsupported platform/browser.
pub fn install(browser: &str) -> Result<Installed> {
    let dir = super::chrome_dir()?;
    let ext = dir.join("extension");
    std::fs::create_dir_all(&ext)?;
    for (name, body) in FILES {
        std::fs::write(ext.join(name), body)?;
    }
    let th = th_path()?;
    let wrapper = dir.join("host");
    let quoted = th.display().to_string().replace('\'', r"'\''");
    std::fs::write(&wrapper, format!("#!/bin/sh\nexec '{quoted}' chrome host \"$@\"\n"))?;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;
    let mdir = manifest_dir(browser)?;
    std::fs::create_dir_all(&mdir)?;
    let manifest = mdir.join(format!("{HOST_NAME}.json"));
    std::fs::write(&manifest, serde_json::to_string_pretty(&host_manifest(&wrapper))? + "\n")?;
    Ok(Installed {
        extension_dir: ext,
        manifest,
        th,
    })
}

/// Remove the host manifest and wrapper. History and the unpacked extension stay.
///
/// # Errors
/// Filesystem failure other than "already gone".
pub fn uninstall(browser: &str) -> Result<PathBuf> {
    let manifest = manifest_dir(browser)?.join(format!("{HOST_NAME}.json"));
    for p in [&manifest, &super::chrome_dir()?.join("host")] {
        match std::fs::remove_file(p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(manifest)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test idiom")]
mod tests {
    use super::*;
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    /// Chrome's extension id: the first 32 hex digits of sha256(public key DER), as a–p.
    fn extension_id(manifest_key_b64: &str) -> Result<String> {
        let der = base64::engine::general_purpose::STANDARD
            .decode(manifest_key_b64)
            .context("manifest key isn't base64")?;
        let hex = Sha256::digest(der)[..16].iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        });
        Ok(hex
            .chars()
            .map(|c| char::from(b'a' + u8::try_from(c.to_digit(16).unwrap_or(0)).unwrap_or(0)))
            .collect())
    }

    #[test]
    fn pinned_id_matches_manifest_key() {
        let m: serde_json::Value = serde_json::from_str(FILES[0].1).unwrap();
        assert_eq!(extension_id(m["key"].as_str().unwrap()).unwrap(), EXTENSION_ID);
    }

    #[test]
    fn extension_asks_only_for_bookmarks_and_native_messaging_up_front() {
        let m: serde_json::Value = serde_json::from_str(FILES[0].1).unwrap();
        assert_eq!(m["manifest_version"], 3);
        assert_eq!(m["permissions"], json!(["bookmarks", "nativeMessaging"]));
        assert!(m.get("host_permissions").is_none());
        assert_eq!(m["optional_host_permissions"], json!(["<all_urls>"]));
        let bg = FILES.iter().find(|(n, _)| *n == "background.js").unwrap().1;
        assert!(bg.contains(HOST_NAME));
    }

    #[test]
    fn host_manifest_allows_only_our_extension() {
        let m = host_manifest(Path::new("/x/host"));
        assert_eq!(m["allowed_origins"], json!([format!("chrome-extension://{EXTENSION_ID}/")]));
        assert_eq!(m["type"], "stdio");
        assert!(HOST_NAME.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_'));
    }

    #[test]
    fn knows_browser_dirs() {
        assert!(manifest_dir("chrome").unwrap().ends_with("NativeMessagingHosts"));
        assert!(manifest_dir("netscape").is_err());
    }
}
