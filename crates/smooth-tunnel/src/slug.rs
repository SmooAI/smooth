//! Subdomain slugs for th.smoo.ai sessions.
//!
//! Ephemeral slugs look like `scratch-3f21` — a fixed prefix plus
//! four hex chars. Stable slugs (reserved by an account) can be any
//! valid subdomain label the owner chose, with the same character
//! rules. Both are validated locally before we bother the service.

use std::fmt;

use uuid::Uuid;

/// A slug the client wants to request (or has already been given).
/// `Ephemeral` is the default — we generate a fresh random suffix
/// each session so two users on the same machine never collide.
#[derive(Debug, Clone)]
pub enum SlugPreference {
    /// Server picks (or matches `<prefix>-<random>`).
    Ephemeral,
    /// User asked for this specific slug. Subject to service-side
    /// checks (availability + entitlement) — the server may reject
    /// and assign a fresh ephemeral one.
    Requested(String),
}

impl SlugPreference {
    /// Resolve to the string the client will send in
    /// [`crate::protocol::ClientHello::slug_preference`].
    /// `Ephemeral` stays `None` so the server knows to allocate.
    #[must_use]
    pub fn to_wire(&self) -> Option<String> {
        match self {
            Self::Ephemeral => None,
            Self::Requested(s) => Some(s.clone()),
        }
    }

    /// Reject obviously-bad slugs before hitting the network.
    /// Server-side validation is authoritative but duplicating the
    /// cheap rules here gives a better error to the CLI user.
    ///
    /// # Errors
    ///
    /// Returns [`SlugError`] if the slug is empty, too long, or
    /// contains characters that aren't valid in a DNS label.
    pub fn validate(&self) -> Result<(), SlugError> {
        match self {
            Self::Ephemeral => Ok(()),
            Self::Requested(s) => validate_slug(s),
        }
    }
}

impl fmt::Display for SlugPreference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ephemeral => f.write_str("<ephemeral>"),
            Self::Requested(s) => f.write_str(s),
        }
    }
}

/// Generate a fresh ephemeral slug suggestion.
///
/// The server is free to ignore this — [`SlugPreference::Ephemeral`]
/// maps to `None` on the wire. This exists for diagnostics + local
/// previews: `th tunnel status` can show "would've been: scratch-3f21"
/// so users can eyeball uniqueness before dialing out.
#[must_use]
pub fn generate_ephemeral_hint() -> String {
    let id = Uuid::new_v4();
    let bytes = id.as_bytes();
    // 4 hex chars → 65 536 buckets. Plenty for a local preview; real
    // uniqueness comes from the server's allocator.
    format!("scratch-{:02x}{:02x}", bytes[0], bytes[1])
}

/// Rules mirror DNS label validity plus th.smoo.ai's own conventions.
/// Kept pure + allocation-free so it's safe to call hot.
fn validate_slug(s: &str) -> Result<(), SlugError> {
    if s.is_empty() {
        return Err(SlugError::Empty);
    }
    // DNS labels are capped at 63 bytes; we keep the same rule here
    // so the slug always round-trips through DNS.
    if s.len() > 63 {
        return Err(SlugError::TooLong(s.len()));
    }
    if !s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) {
        return Err(SlugError::BadFirstChar);
    }
    if let Some(c) = s.chars().find(|c| !(c.is_ascii_alphanumeric() || *c == '-')) {
        return Err(SlugError::BadChar(c));
    }
    // Disallow trailing hyphen because some resolvers choke on it.
    if s.ends_with('-') {
        return Err(SlugError::TrailingHyphen);
    }
    Ok(())
}

/// Why a slug was rejected. The CLI turns these into human messages.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SlugError {
    #[error("slug is empty")]
    Empty,
    #[error("slug is {0} chars long; DNS labels max at 63")]
    TooLong(usize),
    #[error("slug must start with a letter or digit")]
    BadFirstChar,
    #[error("slug must only contain letters, digits, or '-'; got {0:?}")]
    BadChar(char),
    #[error("slug may not end with '-'")]
    TrailingHyphen,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ephemeral_to_wire_is_none() {
        assert!(SlugPreference::Ephemeral.to_wire().is_none());
    }

    #[test]
    fn requested_to_wire_is_some() {
        let p = SlugPreference::Requested("my-review".into());
        assert_eq!(p.to_wire().as_deref(), Some("my-review"));
    }

    #[test]
    fn valid_slugs_accepted() {
        for ok in ["a", "abc", "scratch-3f21", "team-review", "x0", "ABC123"] {
            SlugPreference::Requested(ok.into()).validate().expect(ok);
        }
    }

    #[test]
    fn empty_slug_rejected() {
        let err = SlugPreference::Requested(String::new()).validate().unwrap_err();
        assert_eq!(err, SlugError::Empty);
    }

    #[test]
    fn oversize_slug_rejected() {
        let slug = "a".repeat(64);
        let err = SlugPreference::Requested(slug).validate().unwrap_err();
        assert!(matches!(err, SlugError::TooLong(64)));
    }

    #[test]
    fn leading_hyphen_rejected() {
        let err = SlugPreference::Requested("-abc".into()).validate().unwrap_err();
        assert_eq!(err, SlugError::BadFirstChar);
    }

    #[test]
    fn trailing_hyphen_rejected() {
        let err = SlugPreference::Requested("abc-".into()).validate().unwrap_err();
        assert_eq!(err, SlugError::TrailingHyphen);
    }

    #[test]
    fn special_char_rejected() {
        let err = SlugPreference::Requested("my_review".into()).validate().unwrap_err();
        assert_eq!(err, SlugError::BadChar('_'));
    }

    #[test]
    fn ephemeral_hint_starts_with_scratch_prefix() {
        let hint = generate_ephemeral_hint();
        assert!(hint.starts_with("scratch-"), "got {hint}");
        // 4 hex chars + "scratch-" prefix.
        assert_eq!(hint.len(), "scratch-".len() + 4);
        // The suffix must be valid hex so the whole slug passes
        // `validate_slug`.
        assert!(hint.chars().skip("scratch-".len()).all(|c| c.is_ascii_hexdigit()));
        // And the hint itself must be a valid slug request.
        SlugPreference::Requested(hint).validate().expect("hint is valid slug");
    }
}
