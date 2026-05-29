//! SWE-bench dataset fetch + parse.
//!
//! Isolated from the scoring pipeline so it can be unit-tested without
//! a live HuggingFace round-trip. The public surface is:
//!
//! - [`SweBenchVariant`] — `Verified` or `Lite`. Maps to a HF dataset
//!   slug + cache directory name.
//! - [`SweBenchInstance`] — a single task row, fields normalized so
//!   FAIL_TO_PASS / PASS_TO_PASS arrive as `Vec<String>` whether the
//!   upstream encodes them as JSON arrays or as JSON-encoded strings.
//! - [`fetch_instances`] — paginates the HF datasets-server rows API
//!   and caches to `~/.smooth/bench-data/swe-bench-<variant>/instances.jsonl`.
//!   Honours `SMOOTH_BENCH_DATASET_REFRESH=1`.
//! - [`parse_instances_jsonl`] — pure parser, takes a `&str` and
//!   returns `Vec<SweBenchInstance>`.
//!
//! The full SWE-bench task schema has many fields (`hints_text`,
//! `created_at`, etc.). We only deserialise what the harness needs.
//! Extra fields are ignored by serde's default.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

/// The two SWE-bench variants we know how to fetch. Each maps to a
/// `princeton-nlp/<slug>` dataset on HuggingFace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SweBenchVariant {
    /// `princeton-nlp/SWE-bench_Verified` — 500 human-validated tasks.
    Verified,
    /// `princeton-nlp/SWE-bench_Lite` — 300-task subset, cheaper to run.
    Lite,
}

impl SweBenchVariant {
    /// The HuggingFace dataset slug (the `<user>/<dataset>` form).
    #[must_use]
    pub fn hf_slug(self) -> &'static str {
        match self {
            Self::Verified => "princeton-nlp/SWE-bench_Verified",
            Self::Lite => "princeton-nlp/SWE-bench_Lite",
        }
    }

    /// Filesystem-safe variant name, used as the cache subdirectory.
    #[must_use]
    pub fn cache_dir_name(self) -> &'static str {
        match self {
            Self::Verified => "swe-bench-verified",
            Self::Lite => "swe-bench-lite",
        }
    }
}

/// One row from the SWE-bench dataset, normalised for the harness.
///
/// `fail_to_pass` and `pass_to_pass` may arrive from HF as either a
/// JSON array of strings or (more common in the official mirrors) a
/// single JSON-encoded string containing a JSON array. The custom
/// deserializer in this module handles both.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SweBenchInstance {
    pub instance_id: String,
    pub repo: String,
    pub base_commit: String,
    pub problem_statement: String,
    #[serde(rename = "FAIL_TO_PASS", alias = "fail_to_pass", deserialize_with = "deserialize_string_or_vec")]
    pub fail_to_pass: Vec<String>,
    #[serde(rename = "PASS_TO_PASS", alias = "pass_to_pass", deserialize_with = "deserialize_string_or_vec")]
    pub pass_to_pass: Vec<String>,
    #[serde(default)]
    pub environment_setup_commit: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// Deserialise a field that arrives as either:
/// 1. A JSON array of strings: `["test_a", "test_b"]`
/// 2. A JSON-encoded string containing a JSON array: `"[\"test_a\", \"test_b\"]"`
///
/// Both forms exist in the wild SWE-bench mirrors — the Lite tarball
/// uses (1), while the HF datasets-server rows API typically returns
/// (2) because the column type is `string` upstream.
fn deserialize_string_or_vec<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::Array(items) => items
            .into_iter()
            .map(|x| match x {
                serde_json::Value::String(s) => Ok(s),
                other => Err(D::Error::custom(format!("expected string in array, got {other:?}"))),
            })
            .collect(),
        serde_json::Value::String(s) => {
            // Stringified array — parse the inner JSON.
            let inner: serde_json::Value = serde_json::from_str(&s).map_err(|e| D::Error::custom(format!("failed to parse stringified JSON array: {e}")))?;
            match inner {
                serde_json::Value::Array(items) => items
                    .into_iter()
                    .map(|x| match x {
                        serde_json::Value::String(s) => Ok(s),
                        other => Err(D::Error::custom(format!("expected string inside stringified array, got {other:?}"))),
                    })
                    .collect(),
                other => Err(D::Error::custom(format!("expected array inside stringified field, got {other:?}"))),
            }
        }
        serde_json::Value::Null => Ok(Vec::new()),
        other => Err(D::Error::custom(format!("expected array or string, got {other:?}"))),
    }
}

/// Where the harness caches dataset rows on disk.
///
/// # Errors
/// Errors when there's no home directory.
pub fn cache_dir(variant: SweBenchVariant) -> Result<PathBuf> {
    let home = dirs_next::home_dir().ok_or_else(|| anyhow!("no home dir"))?;
    Ok(home.join(".smooth").join("bench-data").join(variant.cache_dir_name()))
}

/// Cached `instances.jsonl` path for `variant`.
///
/// # Errors
/// Errors when there's no home directory.
pub fn cache_file(variant: SweBenchVariant) -> Result<PathBuf> {
    Ok(cache_dir(variant)?.join("instances.jsonl"))
}

/// Parse a JSONL blob into a `Vec<SweBenchInstance>`. Blank lines are
/// skipped. Lines that fail to parse return an error annotated with
/// the line number for fast debugging of mirror drift.
///
/// # Errors
/// Errors on the first non-blank line that doesn't deserialise into
/// `SweBenchInstance`. Caller decides whether to retry, skip, or
/// surface the error.
pub fn parse_instances_jsonl(blob: &str) -> Result<Vec<SweBenchInstance>> {
    let mut out = Vec::new();
    for (idx, line) in blob.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let inst: SweBenchInstance = serde_json::from_str(trimmed).with_context(|| format!("parse instances.jsonl line {}", idx + 1))?;
        out.push(inst);
    }
    Ok(out)
}

/// Fetch instances for `variant`, paginating the HuggingFace
/// datasets-server rows API and writing the result atomically to the
/// cache file. On subsequent calls, the cache short-circuits the
/// network. Set `SMOOTH_BENCH_DATASET_REFRESH=1` to bust the cache.
///
/// `limit` caps the total number of rows fetched + cached. `None` =
/// fetch everything (~500 for Verified, ~300 for Lite).
///
/// # Errors
/// Errors on network failure, on a non-2xx HTTP response, or on a
/// response body that can't be parsed as JSON.
pub async fn fetch_instances(variant: SweBenchVariant, limit: Option<usize>) -> Result<Vec<SweBenchInstance>> {
    let cache = cache_file(variant)?;
    let refresh = std::env::var("SMOOTH_BENCH_DATASET_REFRESH")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if !refresh {
        if let Ok(body) = std::fs::read_to_string(&cache) {
            if !body.trim().is_empty() {
                let mut parsed = parse_instances_jsonl(&body)?;
                if let Some(n) = limit {
                    parsed.truncate(n);
                }
                return Ok(parsed);
            }
        }
    }

    let mut instances = Vec::new();
    let mut offset: usize = 0;
    let page = 100usize;
    let client = reqwest::Client::builder().user_agent("smooth-bench/1").build()?;
    loop {
        if let Some(n) = limit {
            if instances.len() >= n {
                break;
            }
        }
        let url = format!(
            "https://datasets-server.huggingface.co/rows?dataset={slug}&config=default&split=test&offset={offset}&length={page}",
            slug = urlencoded(variant.hf_slug()),
            offset = offset,
            page = page,
        );
        let resp = client.get(&url).send().await.with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            return Err(anyhow!("HF datasets-server returned {} for {url}", resp.status()));
        }
        let json: serde_json::Value = resp.json().await.context("decode HF response body")?;
        let rows = json.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        if rows.is_empty() {
            break;
        }
        let n_received = rows.len();
        for row in rows {
            let row_obj = row.get("row").cloned().unwrap_or(serde_json::Value::Null);
            let inst: SweBenchInstance = serde_json::from_value(row_obj).context("decode SweBenchInstance from HF row")?;
            instances.push(inst);
            if let Some(n) = limit {
                if instances.len() >= n {
                    break;
                }
            }
        }
        offset += n_received;
        if n_received < page {
            break;
        }
    }

    // Atomically write the cache: write to a tmp sibling, fsync, rename.
    let dir = cache.parent().ok_or_else(|| anyhow!("cache file has no parent"))?;
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let tmp = dir.join("instances.jsonl.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        for inst in &instances {
            let line = serde_json::to_string(inst)?;
            f.write_all(line.as_bytes())?;
            f.write_all(b"\n")?;
        }
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, &cache).with_context(|| format!("rename {} -> {}", tmp.display(), cache.display()))?;

    Ok(instances)
}

/// Minimal URL-safe encoder for path-style segments. The dataset slug
/// has a `/`, which `reqwest` will pass through fine, but we still
/// percent-encode the rest of the unsafe set.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(b as char),
            other => {
                use std::fmt::Write;
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// Lightweight check whether the cache file for `variant` already
/// exists and is non-empty. Used by tests and by the runner to avoid a
/// blocking HF call on warm runs.
#[must_use]
pub fn cache_present(variant: SweBenchVariant) -> bool {
    cache_file(variant).map(|p| is_nonempty(&p)).unwrap_or(false)
}

fn is_nonempty(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture: a single instance where FAIL_TO_PASS / PASS_TO_PASS
    /// arrive as a `Vec<String>` (the tarball form).
    const FIXTURE_ARRAY: &str = r#"{"instance_id":"foo__bar-1","repo":"foo/bar","base_commit":"abc123","problem_statement":"Fix the bug.","FAIL_TO_PASS":["tests/test_foo.py::test_a"],"PASS_TO_PASS":["tests/test_foo.py::test_b","tests/test_foo.py::test_c"],"environment_setup_commit":"def456","version":"1.0"}"#;

    /// Fixture: the same instance but with FAIL_TO_PASS / PASS_TO_PASS
    /// arriving as JSON-encoded strings (the HF rows-API form).
    const FIXTURE_STRINGIFIED: &str = r#"{"instance_id":"foo__bar-1","repo":"foo/bar","base_commit":"abc123","problem_statement":"Fix the bug.","FAIL_TO_PASS":"[\"tests/test_foo.py::test_a\"]","PASS_TO_PASS":"[\"tests/test_foo.py::test_b\", \"tests/test_foo.py::test_c\"]","environment_setup_commit":"def456","version":"1.0"}"#;

    #[test]
    fn parses_vec_string_form() {
        let parsed = parse_instances_jsonl(FIXTURE_ARRAY).unwrap();
        assert_eq!(parsed.len(), 1);
        let inst = &parsed[0];
        assert_eq!(inst.instance_id, "foo__bar-1");
        assert_eq!(inst.repo, "foo/bar");
        assert_eq!(inst.fail_to_pass, vec!["tests/test_foo.py::test_a".to_string()]);
        assert_eq!(
            inst.pass_to_pass,
            vec!["tests/test_foo.py::test_b".to_string(), "tests/test_foo.py::test_c".to_string()],
        );
        assert_eq!(inst.environment_setup_commit.as_deref(), Some("def456"));
        assert_eq!(inst.version.as_deref(), Some("1.0"));
    }

    #[test]
    fn parses_stringified_array_form() {
        let parsed = parse_instances_jsonl(FIXTURE_STRINGIFIED).unwrap();
        assert_eq!(parsed.len(), 1);
        let inst = &parsed[0];
        assert_eq!(inst.fail_to_pass, vec!["tests/test_foo.py::test_a".to_string()]);
        assert_eq!(
            inst.pass_to_pass,
            vec!["tests/test_foo.py::test_b".to_string(), "tests/test_foo.py::test_c".to_string()],
        );
    }

    #[test]
    fn parses_jsonl_with_blank_lines() {
        let jsonl = format!("\n{FIXTURE_ARRAY}\n\n{FIXTURE_STRINGIFIED}\n");
        let parsed = parse_instances_jsonl(&jsonl).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].instance_id, "foo__bar-1");
        assert_eq!(parsed[1].instance_id, "foo__bar-1");
    }

    #[test]
    fn empty_jsonl_returns_empty_vec() {
        let parsed = parse_instances_jsonl("").unwrap();
        assert!(parsed.is_empty());
        let parsed = parse_instances_jsonl("\n\n\n").unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn malformed_line_errors_with_line_number() {
        let blob = format!("{FIXTURE_ARRAY}\nnot-json\n");
        let err = parse_instances_jsonl(&blob).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("line 2"), "{msg}");
    }

    #[test]
    fn variant_slugs_are_correct() {
        assert_eq!(SweBenchVariant::Verified.hf_slug(), "princeton-nlp/SWE-bench_Verified");
        assert_eq!(SweBenchVariant::Lite.hf_slug(), "princeton-nlp/SWE-bench_Lite");
        assert_eq!(SweBenchVariant::Verified.cache_dir_name(), "swe-bench-verified");
        assert_eq!(SweBenchVariant::Lite.cache_dir_name(), "swe-bench-lite");
    }

    #[test]
    fn cache_file_paths_are_under_smooth_bench_data() {
        let p = cache_file(SweBenchVariant::Verified).unwrap();
        let s = p.to_string_lossy().to_string();
        assert!(s.contains(".smooth"), "{s}");
        assert!(s.contains("bench-data"), "{s}");
        assert!(s.contains("swe-bench-verified"), "{s}");
        assert!(s.ends_with("instances.jsonl"), "{s}");
    }

    #[test]
    fn missing_optional_fields_default_to_none() {
        let blob = r#"{"instance_id":"x","repo":"a/b","base_commit":"c","problem_statement":"p","FAIL_TO_PASS":[],"PASS_TO_PASS":[]}"#;
        let parsed = parse_instances_jsonl(blob).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].environment_setup_commit, None);
        assert_eq!(parsed[0].version, None);
        assert!(parsed[0].fail_to_pass.is_empty());
        assert!(parsed[0].pass_to_pass.is_empty());
    }

    #[test]
    fn null_string_field_decodes_to_empty_vec() {
        let blob = r#"{"instance_id":"x","repo":"a/b","base_commit":"c","problem_statement":"p","FAIL_TO_PASS":null,"PASS_TO_PASS":null}"#;
        let parsed = parse_instances_jsonl(blob).unwrap();
        assert!(parsed[0].fail_to_pass.is_empty());
        assert!(parsed[0].pass_to_pass.is_empty());
    }

    #[test]
    fn urlencoded_preserves_slug_separator() {
        assert_eq!(urlencoded("princeton-nlp/SWE-bench_Verified"), "princeton-nlp/SWE-bench_Verified");
        assert_eq!(urlencoded("a b"), "a%20b");
        assert_eq!(urlencoded("plain"), "plain");
    }
}
