pub mod access_wire;
pub mod alert;
pub mod detectors;
pub mod hook;
pub mod judge;

pub use access_wire::{AccessEvent, AccessKind, AccessResolution, NewAccessRequest, PendingAccessRequest, ResolutionVerdict};
pub use alert::{Alert, Severity};
pub use detectors::{detect_dangerous_cli, CliGuard, DetectorResult, SecretDetector, WriteGuard};
pub use hook::NarcHook;
pub use judge::{
    rule_engine_decide, Decision, DecisionCache, JudgeDecision, JudgeKind, JudgeRequest, Scope, DANGEROUS_CLI_SUBSTRINGS, DANGEROUS_DOMAIN_SUFFIXES,
    OBVIOUSLY_SAFE_DOMAIN_SUFFIXES,
};
