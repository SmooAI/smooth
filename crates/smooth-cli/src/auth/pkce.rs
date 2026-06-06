//! PKCE (RFC 7636) helper for the browser-based `th auth login` flow.
//!
//! [Proof Key for Code Exchange](https://datatracker.ietf.org/doc/html/rfc7636)
//! is the OAuth2 extension that lets a public client (the CLI) prove
//! to the authorization server that the same client that started the
//! flow is the one redeeming the code — without ever shipping a
//! client_secret.
//!
//! Flow recap:
//! 1. Generate a high-entropy `code_verifier` (43–128 chars from a
//!    URL-safe alphabet) and a `code_challenge = BASE64URL-NO-PAD(
//!    SHA-256(code_verifier))`.
//! 2. Authorize request carries `code_challenge` + `code_challenge_method=S256`.
//! 3. Token exchange carries the original `code_verifier`. The server
//!    re-hashes it and rejects the exchange if it doesn't match.
//!
//! Generators here are deliberately small + dependency-light so they
//! can be audited at a glance. Pearl th-fcb579.

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Length in bytes of the random source for the code_verifier.
/// Per RFC 7636 §4.1 the resulting verifier MUST be 43-128 chars from
/// the unreserved set `[A-Za-z0-9-._~]`. We feed 32 random bytes
/// (256 bits) through URL-safe-no-pad base64, which always lands at
/// exactly 43 chars — the minimum recommended length.
const CODE_VERIFIER_BYTES: usize = 32;

/// Length in bytes for the CSRF `state` token. 16 bytes → 22 base64
/// chars, plenty unguessable.
const STATE_TOKEN_BYTES: usize = 16;

/// One PKCE pair: the secret verifier the CLI hangs on to, and the
/// derived public challenge it forwards to the authorization server.
#[derive(Debug, Clone)]
pub struct PkcePair {
    /// High-entropy random string; sent on the token exchange so the
    /// server can recompute SHA-256 and verify it matches.
    pub verifier: String,
    /// `BASE64URL-NO-PAD(SHA-256(verifier))` — sent on the authorize
    /// request as `code_challenge` with `code_challenge_method=S256`.
    pub challenge: String,
}

impl PkcePair {
    /// Generate a fresh PKCE pair using the platform RNG.
    pub fn generate() -> Self {
        Self::generate_with(&mut rand::thread_rng())
    }

    /// Generate a PKCE pair from a caller-supplied RNG. Useful for
    /// deterministic tests; production callers should use
    /// [`PkcePair::generate`].
    pub fn generate_with<R: RngCore>(rng: &mut R) -> Self {
        let mut buf = [0u8; CODE_VERIFIER_BYTES];
        rng.fill_bytes(&mut buf);
        let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
        let challenge = derive_challenge(&verifier);
        Self { verifier, challenge }
    }
}

/// Derive `BASE64URL-NO-PAD(SHA-256(verifier))` per RFC 7636 §4.2.
#[must_use]
pub fn derive_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let digest = hasher.finalize();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// Generate a fresh random CSRF state token using the platform RNG.
/// Returned as URL-safe-no-pad base64.
pub fn random_state() -> String {
    random_state_with(&mut rand::thread_rng())
}

/// Generate a state token from a caller-supplied RNG (for tests).
pub fn random_state_with<R: RngCore>(rng: &mut R) -> String {
    let mut buf = [0u8; STATE_TOKEN_BYTES];
    rng.fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 §A.1 reference vector — the canonical PKCE example
    /// every implementer is expected to round-trip cleanly.
    ///
    /// Verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
    /// Expected challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    #[test]
    fn rfc7636_a1_reference_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = derive_challenge(verifier);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn generated_verifier_is_43_chars() {
        // 32 bytes → 43 base64-url-no-pad chars. RFC 7636 §4.1 requires
        // 43-128 chars; we hug the lower bound.
        let pair = PkcePair::generate();
        assert_eq!(pair.verifier.len(), 43, "verifier was {} chars", pair.verifier.len());
    }

    #[test]
    fn generated_verifier_charset_is_url_safe() {
        let pair = PkcePair::generate();
        // RFC 7636 unreserved set: ALPHA / DIGIT / "-" / "." / "_" / "~"
        // base64url uses ALPHA / DIGIT / "-" / "_" — strict subset, no
        // padding because we use NO_PAD.
        for c in pair.verifier.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "verifier contains disallowed char {c:?}: {}",
                pair.verifier
            );
        }
    }

    #[test]
    fn generated_pair_roundtrips_through_derive() {
        let pair = PkcePair::generate();
        // The pair MUST internally round-trip: re-deriving the challenge
        // from the verifier yields the same challenge we stored.
        assert_eq!(derive_challenge(&pair.verifier), pair.challenge);
    }

    #[test]
    fn two_consecutive_pairs_differ() {
        let a = PkcePair::generate();
        let b = PkcePair::generate();
        assert_ne!(a.verifier, b.verifier, "verifier collision is statistically impossible");
        assert_ne!(a.challenge, b.challenge);
    }

    #[test]
    fn random_state_is_url_safe_and_nonempty() {
        let s = random_state();
        assert!(!s.is_empty());
        for c in s.chars() {
            assert!(c.is_ascii_alphanumeric() || c == '-' || c == '_', "state contains disallowed char {c:?}: {s}");
        }
    }

    #[test]
    fn two_consecutive_states_differ() {
        // 16 random bytes → collision is statistically impossible.
        assert_ne!(random_state(), random_state());
    }

    #[test]
    fn derive_challenge_is_deterministic() {
        // Same input → same output, every time.
        let v = "abcdefghijklmnopqrstuvwxyz0123456789-._~";
        assert_eq!(derive_challenge(v), derive_challenge(v));
    }

    #[test]
    fn derive_challenge_changes_with_input() {
        let a = derive_challenge("verifier-a");
        let b = derive_challenge("verifier-b");
        assert_ne!(a, b);
    }

    #[test]
    fn challenge_has_no_base64_padding() {
        let c = derive_challenge("any input");
        // SHA-256 → 32 bytes → 43 base64-no-pad chars. Padding would
        // round it to 44 with a trailing '='.
        assert!(!c.contains('='), "challenge should not include base64 padding: {c}");
        assert_eq!(c.len(), 43);
    }
}
