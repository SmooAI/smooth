pub mod alert;
pub mod detectors;
pub mod hook;
pub mod judge;

pub use alert::{Alert, Severity};
pub use detectors::{DetectorResult, SecretDetector, WriteGuard};
pub use hook::NarcHook;
pub use judge::{
    rule_engine_decide, Decision, DecisionCache, JudgeDecision, JudgeKind, JudgeRequest, DANGEROUS_CLI_SUBSTRINGS, DANGEROUS_DOMAIN_SUFFIXES,
    OBVIOUSLY_SAFE_DOMAIN_SUFFIXES,
};
