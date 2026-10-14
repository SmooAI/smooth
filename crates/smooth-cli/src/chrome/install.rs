//! `th chrome install`: unpack the extension, write the host wrapper and the
//! native-messaging host manifest that lets only our extension launch it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::json;

pub const HOST_NAME: &str = "ai.smoo.smooth_chrome";
/// Pinned by the `key` in the extension manifest, so it survives reloads and reinstalls.
pub const EXTENSION_ID: &str = "pgjpgmfljenhbbgkgjpmaghndenconlc";

pub const FILES: &[(&str, &[u8])] = &[
    ("manifest.json", include_bytes!("extension/manifest.json")),
    ("background.js", include_bytes!("extension/background.js")),
    ("cdp.js", include_bytes!("extension/cdp.js")),
    ("popup.html", include_bytes!("extension/popup.html")),
    ("popup.js", include_bytes!("extension/popup.js")),
    ("icon.svg", include_bytes!("extension/icon.svg")),
    ("icons/16.png", include_bytes!("extension/icons/16.png")),
    ("icons/32.png", include_bytes!("extension/icons/32.png")),
    ("icons/48.png", include_bytes!("extension/icons/48.png")),
    ("icons/128.png", include_bytes!("extension/icons/128.png")),
];

pub const BROWSERS: &[&str] = &["chrome", "chrome-beta", "chrome-canary", "chromium", "brave", "edge"];

/// A browser's user-data dir, relative to home.
fn data_dir(browser: &str) -> Result<&'static str> {
    Ok(if cfg!(target_os = "macos") {
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
    })
}

fn home() -> Result<PathBuf> {
    dirs_next::home_dir().context("no home dir")
}

fn manifest_dir_in(home: &Path, browser: &str) -> Result<PathBuf> {
    Ok(home.join(data_dir(browser)?).join("NativeMessagingHosts"))
}

/// Where each browser looks for user-level native messaging host manifests.
pub fn manifest_dir(browser: &str) -> Result<PathBuf> {
    manifest_dir_in(&home()?, browser)
}

/// Browsers this user has run (their data dir exists); just `chrome` when none has.
fn detected_in(home: &Path) -> Vec<&'static str> {
    let found: Vec<&str> = BROWSERS.iter().copied().filter(|b| data_dir(b).is_ok_and(|d| home.join(d).is_dir())).collect();
    if found.is_empty() {
        vec!["chrome"]
    } else {
        found
    }
}

pub fn detected() -> Result<Vec<&'static str>> {
    Ok(detected_in(&home()?))
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
    pub manifests: Vec<PathBuf>,
    pub th: PathBuf,
}

/// Write the extension, the host wrapper and a host manifest for each browser.
///
/// # Errors
/// Filesystem failure or an unsupported platform/browser.
pub fn install(browsers: &[&str]) -> Result<Installed> {
    let dir = super::chrome_dir()?;
    let ext = dir.join("extension");
    std::fs::create_dir_all(&ext)?;
    for (name, body) in FILES {
        let path = ext.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, body)?;
    }
    let th = th_path()?;
    let wrapper = dir.join("host");
    let quoted = th.display().to_string().replace('\'', r"'\''");
    std::fs::write(&wrapper, format!("#!/bin/sh\nexec '{quoted}' chrome host \"$@\"\n"))?;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;
    let mut manifests = Vec::new();
    for b in browsers {
        let mdir = manifest_dir(b)?;
        std::fs::create_dir_all(&mdir)?;
        let manifest = mdir.join(format!("{HOST_NAME}.json"));
        std::fs::write(&manifest, serde_json::to_string_pretty(&host_manifest(&wrapper))? + "\n")?;
        manifests.push(manifest);
    }
    Ok(Installed {
        extension_dir: ext,
        manifests,
        th,
    })
}

/// Remove the host manifests and wrapper. History and the unpacked extension stay.
///
/// # Errors
/// Filesystem failure other than "already gone".
pub fn uninstall(browsers: &[&str]) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    let mut paths = vec![super::chrome_dir()?.join("host")];
    for b in browsers {
        paths.push(manifest_dir(b)?.join(format!("{HOST_NAME}.json")));
    }
    for p in paths {
        match std::fs::remove_file(&p) {
            Ok(()) => removed.push(p),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            Err(_) => {}
        }
    }
    Ok(removed)
}

/// Why no extension may be connecting: one `(ok, line)` per check.
pub fn diagnose() -> Result<Vec<(bool, String)>> {
    diagnose_in(&home()?, &super::chrome_dir()?)
}

fn diagnose_in(home: &Path, dir: &Path) -> Result<Vec<(bool, String)>> {
    let wrapper = dir.join("host");
    let mut out = Vec::new();
    for b in detected_in(home) {
        let path = manifest_dir_in(home, b)?.join(format!("{HOST_NAME}.json"));
        let m: Option<serde_json::Value> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok());
        out.push(match m {
            None => (false, format!("{b}: no host manifest at {}; run `th chrome install`", path.display())),
            Some(m) if m != host_manifest(&wrapper) => (false, format!("{b}: host manifest {} is out of date; run `th chrome install`", path.display())),
            Some(_) => (true, format!("{b}: host registered")),
        });
    }
    let th = std::fs::read_to_string(&wrapper).ok().and_then(|w| {
        w.lines()
            .find_map(|l| l.strip_prefix("exec '")?.split("' chrome host").next().map(|p| p.replace(r"'\''", "'")))
    });
    out.push(match th {
        None => (false, format!("no host wrapper at {}; run `th chrome install`", wrapper.display())),
        Some(_) if std::fs::metadata(&wrapper).is_ok_and(|m| m.permissions().mode() & 0o111 == 0) => {
            (false, format!("{} isn't executable; run `th chrome install`", wrapper.display()))
        }
        Some(th) if !Path::new(&th).is_file() => (false, format!("the host wrapper runs {th}, which is gone; run `th chrome install`")),
        Some(th) => (true, format!("host runs {th}")),
    });
    let ext = dir.join("extension");
    let stale: Vec<&str> = FILES
        .iter()
        .filter(|(n, body)| std::fs::read(ext.join(n)).ok().as_deref() != Some(*body))
        .map(|(n, _)| *n)
        .collect();
    out.push(if stale.is_empty() {
        (true, format!("extension files at {} match this th", ext.display()))
    } else {
        (
            false,
            format!(
                "extension at {} differs from this th ({}); run `th chrome install`, then reload it in chrome://extensions",
                ext.display(),
                stale.join(", ")
            ),
        )
    });
    out.push((
        true,
        format!(
            "in each Chrome profile: chrome://extensions → Developer mode → Load unpacked → {}; its id must be {EXTENSION_ID}",
            ext.display()
        ),
    ));
    Ok(out)
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
        let m: serde_json::Value = serde_json::from_slice(FILES[0].1).unwrap();
        assert_eq!(extension_id(m["key"].as_str().unwrap()).unwrap(), EXTENSION_ID);
    }

    #[test]
    fn every_icon_the_manifest_names_ships() {
        let m: serde_json::Value = serde_json::from_slice(FILES[0].1).unwrap();
        for set in [&m["icons"], &m["action"]["default_icon"]] {
            let set = set.as_object().unwrap();
            let mut sizes: Vec<u32> = set.keys().map(|k| k.parse().unwrap()).collect();
            sizes.sort_unstable();
            assert_eq!(sizes, [16, 32, 48, 128]);
            for path in set.values() {
                let body = FILES.iter().find(|(n, _)| *n == path.as_str().unwrap()).map(|(_, b)| *b);
                assert!(body.is_some_and(|b| b.starts_with(b"\x89PNG")), "{path} is not a shipped PNG");
            }
        }
    }

    #[test]
    fn extension_asks_only_for_bookmarks_and_native_messaging_up_front() {
        let m: serde_json::Value = serde_json::from_slice(FILES[0].1).unwrap();
        assert_eq!(m["manifest_version"], 3);
        // `alarms` shows no install warning; it only re-wakes the worker to reconnect.
        assert_eq!(m["permissions"], json!(["bookmarks", "nativeMessaging", "alarms"]));
        assert!(m.get("host_permissions").is_none());
        assert_eq!(m["optional_host_permissions"], json!(["<all_urls>"]));
        let bg = std::str::from_utf8(FILES.iter().find(|(n, _)| *n == "background.js").unwrap().1).unwrap();
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

    #[test]
    fn detects_installed_browsers_or_falls_back_to_chrome() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(detected_in(home.path()), vec!["chrome"]);
        std::fs::create_dir_all(home.path().join(data_dir("brave").unwrap())).unwrap();
        std::fs::create_dir_all(home.path().join(data_dir("chromium").unwrap())).unwrap();
        assert_eq!(detected_in(home.path()), vec!["chromium", "brave"]);
    }

    #[test]
    fn diagnose_flags_each_missing_piece_then_passes() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("smooth-chrome");
        let fails = |d: &[(bool, String)]| d.iter().filter(|(ok, _)| !ok).map(|(_, l)| l.clone()).collect::<Vec<_>>();
        let d = diagnose_in(home.path(), &dir).unwrap();
        let bad = fails(&d);
        assert_eq!(bad.len(), 3, "{bad:?}");
        assert!(bad[0].contains("no host manifest"));
        assert!(bad[1].contains("no host wrapper"));
        assert!(bad[2].contains("differs from this th"));

        let ext = dir.join("extension");
        std::fs::create_dir_all(&ext).unwrap();
        for (n, body) in FILES {
            let path = ext.join(n);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let th = home.path().join("it's th");
        std::fs::write(&th, "").unwrap();
        let wrapper = dir.join("host");
        let quoted = th.display().to_string().replace('\'', r"'\''");
        std::fs::write(&wrapper, format!("#!/bin/sh\nexec '{quoted}' chrome host \"$@\"\n")).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mdir = manifest_dir_in(home.path(), "chrome").unwrap();
        std::fs::create_dir_all(&mdir).unwrap();
        std::fs::write(mdir.join(format!("{HOST_NAME}.json")), host_manifest(&wrapper).to_string()).unwrap();
        let d = diagnose_in(home.path(), &dir).unwrap();
        assert!(fails(&d).is_empty(), "{d:?}");
        assert!(d.iter().any(|(_, l)| l.contains("it's th")), "{d:?}");

        std::fs::write(ext.join("background.js"), "old").unwrap();
        std::fs::remove_file(&th).unwrap();
        let bad = fails(&diagnose_in(home.path(), &dir).unwrap());
        assert!(bad.iter().any(|l| l.contains("which is gone")), "{bad:?}");
        assert!(bad.iter().any(|l| l.contains("background.js")), "{bad:?}");
    }
}
