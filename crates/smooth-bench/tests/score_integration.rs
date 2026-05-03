//! Integration test for the `smooth-bench score` sweep runner.
//!
//! This test hits the real LLM + Big Smooth stack — it is
//! `#[ignore]`-flagged so `cargo test` stays offline-safe. Run
//! manually via:
//!
//!     cargo test -p smooai-smooth-bench --test score_integration -- --ignored
//!
//! The test validates:
//!   - `run_sweep` produces a `Score` of the documented shape.
//!   - `by_language` has one entry per language exercised by the
//!     PR gate.
//!   - JSON serialization is round-trippable.
//!
//! It does NOT assert on pass rate or cost — those are meaningful
//! only against a specific routing config + model.

use async_trait::async_trait;
use smooth_bench::curated::CuratedList;
use smooth_bench::sweep::{run_sweep, StdoutObserver, SweepConfig, SweepGate, TaskOutcome, TaskRunner};
use smooth_bench::{BenchOpts, PolyglotLang};

/// An "always passes" stub runner that simulates a successful task
/// in constant time with zero cost. Lets the integration test
/// exercise the full sweep pipeline end-to-end without hitting the
/// network.
///
/// The "real" integration test — against an actual LLM — is out of
/// scope for unit tests here. Run the binary:
///
///     cargo run -p smooai-smooth-bench -- score --pr
struct AlwaysPassRunner;

#[async_trait]
impl TaskRunner for AlwaysPassRunner {
    async fn run_one(&self, _lang: PolyglotLang, _task: &str, _opts: &BenchOpts) -> anyhow::Result<TaskOutcome> {
        Ok(TaskOutcome {
            solved: true,
            cost_usd: 0.0,
            duration_ms: 10,
            inconclusive: false,
        })
    }
}

#[tokio::test]
async fn pr_sweep_produces_well_formed_score() {
    let curated = CuratedList::default_embedded().expect("embedded curated list");
    let cfg = SweepConfig {
        gate: SweepGate::Pr { tasks_per_language: 2 },
        budget_usd_cap: 10.0,
        smooth_version: "integration-test".to_string(),
        commit_sha: "test".to_string(),
        task_opts: BenchOpts::default(),
    };

    let runner = AlwaysPassRunner;
    let mut obs = StdoutObserver;
    let run = run_sweep(&curated, &runner, &cfg, &mut obs).await.expect("sweep runs");

    // 2 tasks × 6 langs = 12 attempts, all pass.
    assert_eq!(run.score.tasks_attempted, 12);
    assert_eq!(run.score.tasks_green, 12);
    assert!((run.score.overall_pass_rate - 1.0).abs() < 1e-9);
    assert!(!run.score.budget_usd_hit);
    assert_eq!(run.score.by_language.len(), 6);

    // JSON round-trip.
    let json = serde_json::to_string_pretty(&run.score).expect("to_json");
    let decoded: smooth_bench::score::Score = serde_json::from_str(&json).expect("from_json");
    assert_eq!(decoded, run.score);
}

/// Real LLM integration: runs a single-task PR sweep against Big
/// Smooth at http://localhost:4400. Gated behind `#[ignore]` so
/// `cargo test` stays offline; run manually with `--ignored` when
/// exercising the full stack.
#[tokio::test]
#[ignore = "hits live LLM via Big Smooth at localhost:4400"]
async fn pr_sweep_against_live_llm_emits_score() {
    let curated = CuratedList::default_embedded().expect("embedded curated list");
    let cfg = SweepConfig {
        gate: SweepGate::Pr { tasks_per_language: 1 },
        budget_usd_cap: 2.0,
        smooth_version: "integration-test".to_string(),
        commit_sha: smooth_bench::sweep::current_commit_sha(),
        task_opts: BenchOpts::default(),
    };
    let runner = smooth_bench::sweep::PolyglotTaskRunner;
    let mut obs = StdoutObserver;
    let run = run_sweep(&curated, &runner, &cfg, &mut obs).await.expect("sweep runs");

    // We only assert on shape — pass rate is model-dependent.
    assert_eq!(run.score.tasks_attempted, 6);
    assert_eq!(run.score.by_language.len(), 6);
    // Every language appears.
    for lang_name in ["python", "rust", "go", "javascript", "java", "cpp"] {
        assert!(
            run.score.by_language.contains_key(lang_name),
            "missing {lang_name} in by_language: {:?}",
            run.score.by_language.keys().collect::<Vec<_>>()
        );
    }
}
