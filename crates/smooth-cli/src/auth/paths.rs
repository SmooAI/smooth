//! Auth storage paths + named profiles.
//!
//! The implementation moved to `smooth_policy::auth_paths` so `smooth-daemon`
//! resolves the same profile the CLI does — see that module for why. This
//! re-export keeps every `auth::paths::…` call site in the CLI unchanged.

pub use smooth_policy::auth_paths::*;
