//! `@`-mention search — the backend the web composer's autocomplete calls.
//!
//! Exposes a single ungated route, `GET /search?q=<query>`, merged into the
//! operator's own router via the local flavor's
//! [`serve_routes`](smooth_operator_server::local::LocalServerBuilder::serve_routes)
//! seam (so it sits alongside `/ws`, `/health`, `/admin/*` at the daemon's
//! origin). It is **ungated** on purpose — like `/admin/model-costs/health`,
//! mentions must resolve on a tokenless connection — and carries the same
//! permissive CORS as `/admin` (applied by the seam) so the cross-origin dev SPA
//! can call it.
//!
//! ## Response shape
//!
//! ```json
//! { "results": [
//!     { "kind": "file"|"path"|"pearl",
//!       "value": "<text to insert>",
//!       "label": "<display name>",
//!       "detail": "<optional: relative dir or pearl status>" }
//! ] }
//! ```
//!
//! Results are capped at [`MAX_RESULTS`], most-relevant first. An empty query
//! returns `{ "results": [] }`.
//!
//! ## What it searches (v1)
//!
//! - **files**: a pruned walk of the daemon's workspace
//!   ([`smooth_tools::walk::pruned_walk`], which skips `.git`/`node_modules`/
//!   `target`), substring/prefix-matched on the workspace-relative path. The walk
//!   is bounded by [`WALK_BUDGET`] so it stays cheap on huge trees.
//! - **paths**: `~` / `.` / `/`-anchored path expansion when the query *looks
//!   like* a path — directory listing filtered by the partial final component.
//!
//! **Pearls are deferred** for v1: `smooth-daemon` does not depend on
//! `smooth-pearls` or the registry, so wiring a pearl store in would be a new
//! dependency + a project-resolution decision. Files + paths cover the composer's
//! primary need; pearls are a documented follow-up.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use smooth_tools::walk::pruned_walk;

/// Maximum results returned for any query, across all kinds.
const MAX_RESULTS: usize = 20;

/// Upper bound on walked filesystem entries per request, so a query against a
/// pathological tree can't turn into an unbounded walk. The pruned walk already
/// skips the heavy dirs; this caps the long tail.
const WALK_BUDGET: usize = 20_000;

/// One autocomplete suggestion. `kind` is the suggestion family the composer
/// renders; `value` is the text inserted on accept; `label` is the display name;
/// `detail` is optional secondary text (relative dir for files, parent dir for
/// paths, status for pearls).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SearchResult {
    pub kind: &'static str,
    pub value: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The `{ "results": [...] }` envelope the composer expects.
#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
}

/// The `?q=` query string (defaulting to empty so a bare `/search` is valid).
#[derive(Debug, Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
}

/// A match candidate: the text ranked against the query (`haystack`) paired with
/// the [`SearchResult`] emitted if it survives ranking. Kept internal so the
/// ranking ([`rank_matches`]) is a pure function over `(query, candidates)`.
#[derive(Debug, Clone)]
struct Candidate {
    haystack: String,
    result: SearchResult,
}

/// Build the `/search` router bound to `workspace`. Returns a `Router<()>`
/// (state already applied) so it merges directly into the operator's router via
/// the `serve_routes` seam.
pub fn search_router(workspace: PathBuf) -> Router {
    Router::new().route("/search", get(search_handler)).with_state(Arc::new(workspace))
}

/// `GET /search?q=<query>` → ranked file + path suggestions.
async fn search_handler(State(workspace): State<Arc<PathBuf>>, Query(query): Query<SearchQuery>) -> Json<SearchResponse> {
    let results = search(&workspace, &query.q);
    Json(SearchResponse { results })
}

/// Resolve `query` against `workspace`: path expansions first (when the query
/// looks like a path — that's a strong, explicit intent), then ranked file
/// matches, merged and capped at [`MAX_RESULTS`]. Empty query → no results.
#[must_use]
pub fn search(workspace: &Path, query: &str) -> Vec<SearchResult> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut results = expand_path_query(trimmed);
    results.extend(rank_matches(trimmed, file_candidates(workspace, WALK_BUDGET)));
    results.truncate(MAX_RESULTS);
    results
}

/// Filter `candidates` to those whose `haystack` contains `query`
/// (case-insensitive), ordered most-relevant first, capped at [`MAX_RESULTS`].
///
/// Pure: no filesystem or network, so the ranking is unit-testable in isolation.
/// Ordering tiers (best first): basename prefix → full-path prefix → substring
/// (earlier position wins), then shorter haystack, then lexicographic for a
/// stable order.
fn rank_matches(query: &str, candidates: Vec<Candidate>) -> Vec<SearchResult> {
    let query_lc = query.trim().to_lowercase();
    if query_lc.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(MatchScore, Candidate)> = candidates
        .into_iter()
        .filter_map(|c| score_match(&query_lc, &c.haystack).map(|s| (s, c)))
        .collect();
    // Sort by score, breaking ties on the haystack for deterministic output.
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.haystack.cmp(&b.1.haystack)));
    scored.into_iter().take(MAX_RESULTS).map(|(_, c)| c.result).collect()
}

/// Relevance score for a candidate, lower = better. The tuple orders by
/// `(tier, match position, key length)`.
type MatchScore = (u8, usize, usize);

/// Score `key` against an already-lowercased `query_lc`, or `None` if it doesn't
/// match. Lower scores rank first.
fn score_match(query_lc: &str, key: &str) -> Option<MatchScore> {
    let key_lc = key.to_lowercase();
    let pos = key_lc.find(query_lc)?;
    let basename = key_lc.rsplit('/').next().unwrap_or(key_lc.as_str());
    let tier = if basename.starts_with(query_lc) {
        0
    } else if key_lc.starts_with(query_lc) {
        1
    } else {
        2
    };
    Some((tier, pos, key_lc.len()))
}

/// Walk `workspace` (pruned of `.git`/`node_modules`/`target`/...) up to
/// `budget` entries, building a [`Candidate`] per regular file: `haystack` and
/// `value` are the workspace-relative path, `label` is the file name, `detail`
/// is the relative parent directory (omitted at the root).
fn file_candidates(workspace: &Path, budget: usize) -> Vec<Candidate> {
    let mut out = Vec::new();
    for entry in pruned_walk(workspace).flatten().take(budget) {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(workspace).unwrap_or(path);
        let rel_str = rel.to_string_lossy().into_owned();
        if rel_str.is_empty() {
            continue;
        }
        let label = path.file_name().map_or_else(|| rel_str.clone(), |n| n.to_string_lossy().into_owned());
        let detail = rel.parent().map(|p| p.to_string_lossy().into_owned()).filter(|s| !s.is_empty());
        out.push(Candidate {
            haystack: rel_str.clone(),
            result: SearchResult {
                kind: "file",
                value: rel_str,
                label,
                detail,
            },
        });
    }
    out
}

/// Expand `query` as a filesystem path when it *looks* like one (anchored at
/// `/`, `~`, `./`, or `../`): list the typed directory and keep entries whose
/// name starts with the partial final component. `value` preserves the typed
/// form (so a `~`-anchored query inserts a `~`-anchored path); directories get a
/// trailing slash. Returns empty for non-path queries or an unreadable dir.
fn expand_path_query(query: &str) -> Vec<SearchResult> {
    if !(query.starts_with('/') || query.starts_with('~') || query.starts_with("./") || query.starts_with("../")) {
        return Vec::new();
    }
    // Need a directory boundary to know what to list and what to complete.
    let Some(slash) = query.rfind('/') else {
        return Vec::new();
    };
    let typed_dir = &query[..=slash]; // includes the trailing '/', preserves the ~ form
    let partial = &query[slash + 1..];
    let partial_lc = partial.to_lowercase();

    let Ok(entries) = std::fs::read_dir(expand_tilde(typed_dir)) else {
        return Vec::new();
    };

    let mut out: Vec<SearchResult> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !partial.is_empty() && !name.to_lowercase().starts_with(&partial_lc) {
                return None;
            }
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            let mut value = format!("{typed_dir}{name}");
            let label = if is_dir {
                value.push('/');
                format!("{name}/")
            } else {
                name
            };
            Some(SearchResult {
                kind: "path",
                value,
                label,
                detail: Some(typed_dir.trim_end_matches('/').to_string()).filter(|s| !s.is_empty()),
            })
        })
        .collect();
    out.sort_by(|a, b| a.value.cmp(&b.value));
    out.truncate(MAX_RESULTS);
    out
}

/// Expand a leading `~` / `~/` to the user's home directory; everything else is
/// taken verbatim. Used only to *resolve* a typed path for listing — the result
/// `value` keeps the `~` form the user typed.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs_next::home_dir() {
            return home.join(rest);
        }
    } else if path == "~" {
        if let Some(home) = dirs_next::home_dir() {
            return home;
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn cand(path: &str) -> Candidate {
        Candidate {
            haystack: path.to_string(),
            result: SearchResult {
                kind: "file",
                value: path.to_string(),
                label: path.rsplit('/').next().unwrap_or(path).to_string(),
                detail: None,
            },
        }
    }

    #[test]
    fn rank_matches_drops_non_matches() {
        let out = rank_matches("zzz", vec![cand("src/main.rs"), cand("README.md")]);
        assert!(out.is_empty(), "no candidate contains the query");
    }

    #[test]
    fn rank_matches_empty_query_is_empty() {
        assert!(rank_matches("   ", vec![cand("src/main.rs")]).is_empty());
    }

    #[test]
    fn rank_matches_is_case_insensitive() {
        let out = rank_matches("MAIN", vec![cand("src/main.rs")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].value, "src/main.rs");
    }

    #[test]
    fn rank_matches_prefers_basename_prefix_over_substring() {
        // "search" matches the basename of search.rs (tier 0) and appears mid-path
        // in server/research_notes.txt (tier 2). Basename-prefix wins.
        let out = rank_matches("search", vec![cand("server/research_notes.txt"), cand("crates/search.rs")]);
        assert_eq!(out[0].value, "crates/search.rs", "basename prefix ranks first: {out:?}");
    }

    #[test]
    fn rank_matches_prefers_full_path_prefix_over_deep_substring() {
        // Both match "src"; the full-path prefix (tier 1) beats the mid-path
        // substring (tier 2).
        let out = rank_matches("src", vec![cand("app/src/lib.rs"), cand("src/main.rs")]);
        assert_eq!(out[0].value, "src/main.rs", "full-path prefix first: {out:?}");
    }

    #[test]
    fn rank_matches_caps_at_max_results() {
        let candidates: Vec<Candidate> = (0..50).map(|i| cand(&format!("file_{i:03}_match.rs"))).collect();
        let out = rank_matches("match", candidates);
        assert_eq!(out.len(), MAX_RESULTS, "result set is capped");
    }

    #[test]
    fn score_match_orders_tiers() {
        // basename prefix < full-path prefix < interior substring.
        let basename = score_match("main", "src/main.rs").unwrap();
        let full = score_match("src", "src/main.rs").unwrap();
        let interior = score_match("main", "src/domain/x.rs").unwrap();
        assert!(basename.0 < interior.0);
        assert!(full.0 < interior.0);
        assert!(score_match("nope", "src/main.rs").is_none());
    }

    #[test]
    fn empty_query_returns_no_results() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main(){}").unwrap();
        assert!(search(dir.path(), "").is_empty());
        assert!(search(dir.path(), "   ").is_empty());
    }

    #[test]
    fn search_finds_workspace_files_and_skips_pruned_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/server.rs"), "// server").unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join("node_modules/pkg/server.js"), "x").unwrap();

        let out = search(root, "server");
        assert!(
            out.iter().any(|r| r.kind == "file" && r.value == "src/server.rs"),
            "workspace file found: {out:?}"
        );
        assert!(!out.iter().any(|r| r.value.contains("node_modules")), "pruned dir excluded: {out:?}");
        // The matched file carries its name as label and its dir as detail.
        let hit = out.iter().find(|r| r.value == "src/server.rs").unwrap();
        assert_eq!(hit.label, "server.rs");
        assert_eq!(hit.detail.as_deref(), Some("src"));
    }

    #[test]
    fn file_candidates_respects_budget() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
        }
        // Budget counts walked entries (root dir included), so a tiny budget
        // yields fewer candidates than files on disk.
        assert!(file_candidates(dir.path(), 3).len() <= 3);
    }

    #[test]
    fn expand_path_query_lists_matching_entries() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("alpha")).unwrap();
        std::fs::write(root.join("apple.txt"), "x").unwrap();
        std::fs::write(root.join("banana.txt"), "x").unwrap();

        // Absolute path query: dir + partial "a" → alpha/ and apple.txt only.
        let query = format!("{}/a", root.display());
        let out = expand_path_query(&query);
        assert!(out.iter().all(|r| r.kind == "path"), "all path-kind: {out:?}");
        let labels: Vec<&str> = out.iter().map(|r| r.label.as_str()).collect();
        assert!(labels.contains(&"alpha/"), "dir gets trailing slash: {labels:?}");
        assert!(labels.contains(&"apple.txt"), "file matches prefix: {labels:?}");
        assert!(!labels.contains(&"banana.txt"), "non-prefix excluded: {labels:?}");
        // The directory entry inserts a trailing-slash value preserving the typed dir.
        let alpha = out.iter().find(|r| r.label == "alpha/").unwrap();
        assert!(alpha.value.ends_with("/alpha/"), "dir value keeps typed dir + slash: {}", alpha.value);
    }

    #[test]
    fn expand_path_query_ignores_non_path_queries() {
        assert!(expand_path_query("server").is_empty(), "plain word is not a path");
        assert!(expand_path_query("foo bar").is_empty());
    }

    #[test]
    fn expand_path_query_unreadable_dir_is_empty() {
        assert!(expand_path_query("/no/such/path/here/x").is_empty());
    }

    #[test]
    fn expand_tilde_resolves_home_only_for_tilde_prefix() {
        if let Some(home) = dirs_next::home_dir() {
            assert_eq!(expand_tilde("~/sub"), home.join("sub"));
            assert_eq!(expand_tilde("~"), home);
        }
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
        assert_eq!(expand_tilde("./rel"), PathBuf::from("./rel"));
    }

    #[tokio::test]
    async fn search_router_serves_results_as_json() {
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("widget.tsx"), "x").unwrap();
        let app = search_router(dir.path().to_path_buf());

        let res = app
            .oneshot(axum::http::Request::builder().uri("/search?q=widget").body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let results = parsed["results"].as_array().unwrap();
        assert!(
            results.iter().any(|r| r["value"] == "widget.tsx" && r["kind"] == "file"),
            "router returns the JSON envelope with the file hit: {parsed}"
        );
    }

    #[tokio::test]
    async fn search_router_empty_query_returns_empty_envelope() {
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let dir = tempfile::tempdir().unwrap();
        let app = search_router(dir.path().to_path_buf());
        let res = app
            .oneshot(axum::http::Request::builder().uri("/search").body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["results"].as_array().unwrap().len(), 0, "empty query → empty results");
    }
}
