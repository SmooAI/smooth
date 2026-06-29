//! Per-session control channel: who drives the pane.
//!
//! The supervisor (`th claude run`) and a human can share one tmux pane.
//! A per-session control file `<registry>/<id>.control` arbitrates input
//! authority so the two never type at once. The supervisor reads it each
//! poll; `th claude mode <id> <mode>` writes it.
//!
//! The parse is pure (`parse_mode`) and unit tested; the IO wrappers are
//! thin.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{anyhow, Context, Result};

use super::registry::registry_dir;

/// Who currently has input authority over a supervised session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Big Smooth drives: the supervisor sends task input + steering and
    /// rescues rate-limits.
    #[default]
    Driving,
    /// The human drives (attached): the supervisor sends no task input but
    /// still rescues the human's own rate-limited turn.
    Manual,
    /// The supervisor only watches: no sending at all.
    Paused,
}

impl Mode {
    /// May the supervisor send task input / steering?
    #[must_use]
    pub fn drives(self) -> bool {
        matches!(self, Mode::Driving)
    }

    /// May the supervisor resend on a rate-limit?
    #[must_use]
    pub fn rescues(self) -> bool {
        matches!(self, Mode::Driving | Mode::Manual)
    }

    /// Lowercase wire form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Driving => "driving",
            Mode::Manual => "manual",
            Mode::Paused => "paused",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Mode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "driving" | "drive" | "auto" => Ok(Mode::Driving),
            "manual" | "human" | "take-over" | "takeover" => Ok(Mode::Manual),
            "paused" | "pause" | "watch" => Ok(Mode::Paused),
            other => Err(anyhow!("unknown mode `{other}` (expected driving|manual|paused)")),
        }
    }
}

/// Parse a control-file body into a [`Mode`], defaulting to `Driving` for
/// empty/unrecognized contents. Pure — the IO-free core of [`read_mode`].
#[must_use]
pub fn parse_mode(contents: &str) -> Mode {
    contents.parse().unwrap_or_default()
}

fn control_path(id: &str) -> PathBuf {
    registry_dir().join(format!("{id}.control"))
}

/// Read a session's mode, defaulting to `Driving` when the file is absent
/// or unreadable.
#[must_use]
pub fn read_mode(id: &str) -> Mode {
    std::fs::read_to_string(control_path(id)).map(|s| parse_mode(&s)).unwrap_or_default()
}

/// Write a session's mode to its control file.
///
/// # Errors
/// On directory creation or write failure.
pub fn write_mode(id: &str, mode: Mode) -> Result<()> {
    let path = control_path(id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&path, mode.as_str()).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Remove a session's control file (best effort).
pub fn clear(id: &str) {
    let _ = std::fs::remove_file(control_path(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_aliases() {
        assert_eq!("driving".parse::<Mode>().unwrap(), Mode::Driving);
        assert_eq!("drive".parse::<Mode>().unwrap(), Mode::Driving);
        assert_eq!("MANUAL".parse::<Mode>().unwrap(), Mode::Manual);
        assert_eq!("take-over".parse::<Mode>().unwrap(), Mode::Manual);
        assert_eq!("pause".parse::<Mode>().unwrap(), Mode::Paused);
        assert!("nonsense".parse::<Mode>().is_err());
    }

    #[test]
    fn parse_mode_defaults_to_driving() {
        assert_eq!(parse_mode(""), Mode::Driving);
        assert_eq!(parse_mode("garbage"), Mode::Driving);
        assert_eq!(parse_mode("manual"), Mode::Manual);
        assert_eq!(parse_mode("  paused\n"), Mode::Paused);
    }

    #[test]
    fn authority_semantics() {
        assert!(Mode::Driving.drives() && Mode::Driving.rescues());
        assert!(!Mode::Manual.drives() && Mode::Manual.rescues());
        assert!(!Mode::Paused.drives() && !Mode::Paused.rescues());
    }

    #[test]
    fn display_roundtrips_through_parse() {
        for m in [Mode::Driving, Mode::Manual, Mode::Paused] {
            assert_eq!(m.to_string().parse::<Mode>().unwrap(), m);
        }
    }
}
