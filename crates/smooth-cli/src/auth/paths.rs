//! XDG-based auth storage + named profiles. SMOODEV-1739.
//!
//! `th` historically kept auth under `~/.smooth/auth/{smooai-user.json,
//! smooai.json}`. This module moves auth to the XDG config dir
//! (`$XDG_CONFIG_HOME/smooth/auth`, default `~/.config/smooth/auth`) and
//! adds named profiles so one host can hold several identities at once.
//!
//! A *profile* bundles a user + M2M session:
//!   - default (unnamed) → `<auth>/smooai-user.json`, `<auth>/smooai.json`
//!   - named `<name>`    → `<auth>/profiles/<name>/{smooai-user.json,smooai.json}`
//!
//! Selection order: `--profile` flag → `SMOOAI_PROFILE` env → the `active`
//! pointer file (`<auth>/active`) → default.
//!
//! Rather than thread the resolved paths through every call site, [`init`]
//! exports `SMOOAI_USER_AUTH_FILE` / `SMOOAI_AUTH_FILE` once at startup. Both
//! credential-store crates (`smooth-api-client` and `smooai-client-shared`)
//! already honor those env vars, so every `default_user()` / `default_m2m()` /
//! `default_path()` call transparently reads the active profile's files.
//!
//! Only the **auth** tree moves here; the rest of `~/.smooth` (dolt, registry,
//! logs, …) is a later phase.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const USER_FILE: &str = "smooai-user.json";
const M2M_FILE: &str = "smooai.json";

/// `$XDG_CONFIG_HOME/smooth`, or `~/.config/smooth`.
#[must_use]
pub fn config_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.trim().is_empty() {
            return PathBuf::from(x).join("smooth");
        }
    }
    dirs_next::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".config").join("smooth")
}

/// `<config>/auth`.
#[must_use]
pub fn auth_dir() -> PathBuf {
    config_dir().join("auth")
}

fn legacy_auth_dir() -> PathBuf {
    dirs_next::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".smooth").join("auth")
}

fn active_file() -> PathBuf {
    auth_dir().join("active")
}

/// Directory holding a profile's session files. `None` = the default
/// (unnamed) profile, whose files live directly in `auth/`.
fn profile_dir(profile: Option<&str>) -> PathBuf {
    match profile {
        None => auth_dir(),
        Some(name) => auth_dir().join("profiles").join(name),
    }
}

#[must_use]
pub fn user_file(profile: Option<&str>) -> PathBuf {
    profile_dir(profile).join(USER_FILE)
}

#[must_use]
pub fn m2m_file(profile: Option<&str>) -> PathBuf {
    profile_dir(profile).join(M2M_FILE)
}

/// Profile names are restricted to a filesystem-safe charset.
#[must_use]
pub fn valid_profile_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The active profile from the `active` pointer file, if any.
#[must_use]
pub fn active_profile() -> Option<String> {
    fs::read_to_string(active_file()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Resolve which profile to use: flag → `SMOOAI_PROFILE` env → `active`
/// file → default (`None`).
#[must_use]
pub fn resolve_profile(flag: Option<String>) -> Option<String> {
    if let Some(p) = flag.filter(|s| !s.trim().is_empty()) {
        return Some(p);
    }
    if let Ok(p) = std::env::var("SMOOAI_PROFILE") {
        if !p.trim().is_empty() {
            return Some(p);
        }
    }
    active_profile()
}

/// The email behind a session file, best-effort (display + migration naming).
fn session_email(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("user").and_then(serde_json::Value::as_str).map(str::to_string)
}

/// Turn an email into a profile name (`tara@offsetwell.com` → `tara`).
fn name_from_email(email: &str) -> Option<String> {
    let local = email.split('@').next().unwrap_or(email);
    let s: String = local
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = s.trim_matches('-').to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// One-time migration: if the XDG auth dir doesn't exist yet but the legacy
/// `~/.smooth/auth` does, COPY the legacy sessions into a named profile
/// (derived from the user email, else `default`) and mark it active.
/// Non-destructive — the legacy files are left as a backup.
pub fn migrate_legacy() -> Result<()> {
    let auth = auth_dir();
    if auth.exists() {
        return Ok(());
    }
    let legacy = legacy_auth_dir();
    let lu = legacy.join(USER_FILE);
    let lm = legacy.join(M2M_FILE);
    if !lu.exists() && !lm.exists() {
        fs::create_dir_all(&auth).ok();
        return Ok(());
    }
    let name = session_email(&lu).and_then(|e| name_from_email(&e)).unwrap_or_else(|| "default".to_string());
    let pdir = auth.join("profiles").join(&name);
    fs::create_dir_all(&pdir).with_context(|| format!("create {}", pdir.display()))?;
    if lu.exists() {
        fs::copy(&lu, pdir.join(USER_FILE)).ok();
    }
    if lm.exists() {
        fs::copy(&lm, pdir.join(M2M_FILE)).ok();
    }
    fs::write(active_file(), &name).ok();
    eprintln!(
        "th: migrated legacy ~/.smooth/auth → {} (profile '{name}', now active). Originals left as a backup.",
        pdir.display()
    );
    Ok(())
}

/// Resolve the active profile and export `SMOOAI_USER_AUTH_FILE` /
/// `SMOOAI_AUTH_FILE` so both credential-store crates read its files.
/// Honors pre-set overrides (won't clobber a value the caller already set,
/// e.g. tests / harness). Call once at startup before any auth use.
pub fn init(profile_flag: Option<String>) {
    migrate_legacy().ok();
    let profile = resolve_profile(profile_flag);
    if std::env::var_os("SMOOAI_USER_AUTH_FILE").is_none() {
        std::env::set_var("SMOOAI_USER_AUTH_FILE", user_file(profile.as_deref()));
    }
    if std::env::var_os("SMOOAI_AUTH_FILE").is_none() {
        std::env::set_var("SMOOAI_AUTH_FILE", m2m_file(profile.as_deref()));
    }
}

/// Named profiles present on disk (sorted). Does not include the default.
#[must_use]
pub fn list_profiles() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(auth_dir().join("profiles")) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                if let Some(n) = e.file_name().to_str() {
                    out.push(n.to_string());
                }
            }
        }
    }
    out.sort();
    out
}

/// Whether the default (unnamed) profile has any stored session.
#[must_use]
pub fn default_profile_present() -> bool {
    user_file(None).exists() || m2m_file(None).exists()
}

pub fn set_active(name: &str) -> Result<()> {
    if !valid_profile_name(name) {
        anyhow::bail!("invalid profile name '{name}' (allowed: letters, digits, '-', '_', '.')");
    }
    fs::create_dir_all(auth_dir())?;
    fs::write(active_file(), name).context("write active-profile pointer")?;
    Ok(())
}

pub fn clear_active() {
    let _ = fs::remove_file(active_file());
}

pub fn remove_profile(name: &str) -> Result<()> {
    if !valid_profile_name(name) {
        anyhow::bail!("invalid profile name '{name}'");
    }
    let pdir = auth_dir().join("profiles").join(name);
    if !pdir.exists() {
        anyhow::bail!("no such profile: {name}");
    }
    fs::remove_dir_all(&pdir).with_context(|| format!("remove {}", pdir.display()))?;
    if active_profile().as_deref() == Some(name) {
        clear_active();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_profile_names() {
        assert!(valid_profile_name("brent"));
        assert!(valid_profile_name("tara.offsetwell_2026"));
        assert!(!valid_profile_name(""));
        assert!(!valid_profile_name("has space"));
        assert!(!valid_profile_name("../etc"));
    }

    #[test]
    fn email_to_profile_name() {
        assert_eq!(name_from_email("tara@offsetwell.com").as_deref(), Some("tara"));
        assert_eq!(name_from_email("Brent.Rager@smoo.ai").as_deref(), Some("brent.rager"));
        assert_eq!(name_from_email("a+b@x.com").as_deref(), Some("a-b"));
        assert_eq!(name_from_email("@nope.com"), None);
    }

    #[test]
    fn profile_file_layout() {
        // Default profile files live directly under auth/; named ones nest.
        let default_user = user_file(None);
        let named_user = user_file(Some("tara"));
        assert!(default_user.ends_with("auth/smooai-user.json"));
        assert!(named_user.ends_with("auth/profiles/tara/smooai-user.json"));
        assert!(m2m_file(Some("tara")).ends_with("auth/profiles/tara/smooai.json"));
    }
}
