//! Curated task list for `smooth-bench score`.
//!
//! The list lives in `curated-tasks.toml` in the crate root — edit
//! the TOML (no recompile) to change the sweep. `CuratedList::load`
//! reads and validates it; validation enforces the "exactly 20
//! tasks per language, no duplicates" invariant.
//!
//! The TOML is embedded at build time via `include_str!` so a
//! `smooth-bench` binary run from anywhere still has a working
//! default list — edits to the source file only take effect on
//! rebuild. Operators who want to point at a different curated list
//! can use `CuratedList::from_toml_str` or
//! `CuratedList::from_toml_path` (future: `--tasks-from <path>` CLI
//! flag — out of scope for th-0465bb).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Context};
use serde::Deserialize;

use crate::PolyglotLang;

/// Required number of tasks per language. Anchored in a const so the
/// validation message and the curation target stay in sync.
pub const TASKS_PER_LANGUAGE: usize = 20;

/// Raw TOML shape — one array per language, keyed by the lowercase
/// language name. Unknown keys are rejected so a typo in the TOML
/// (e.g. `javasript`) fails loud instead of silently producing a
/// 5-language sweep.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CuratedToml {
    python: Vec<String>,
    rust: Vec<String>,
    go: Vec<String>,
    javascript: Vec<String>,
    java: Vec<String>,
    cpp: Vec<String>,
}

/// Parsed + validated curated task list. Indexed by `PolyglotLang`.
#[derive(Debug, Clone)]
pub struct CuratedList {
    tasks: BTreeMap<PolyglotLang, Vec<String>>,
}

impl CuratedList {
    /// The default list baked into the binary at build time. Falls
    /// back to this when no path override is supplied.
    ///
    /// # Errors
    /// Returns an error if the embedded TOML fails to parse or
    /// validate. In practice this only fires when an engineer edits
    /// `curated-tasks.toml` into an invalid state — the unit tests
    /// catch that before merge.
    pub fn default_embedded() -> anyhow::Result<Self> {
        Self::from_toml_str(include_str!("../curated-tasks.toml"))
    }

    /// Parse + validate a TOML string. Validation rules:
    /// - Every language MUST have exactly `TASKS_PER_LANGUAGE` entries.
    /// - No duplicates within a language.
    /// - No empty task names.
    ///
    /// Duplicates across languages are allowed (e.g. `bowling` is
    /// fine in both python and rust; the same exercise exists
    /// independently in each language's corpus).
    ///
    /// # Errors
    /// Returns an error if the TOML doesn't parse or any validation
    /// rule fails. Error messages name the offending language so the
    /// curator can find the line quickly.
    pub fn from_toml_str(s: &str) -> anyhow::Result<Self> {
        let raw: CuratedToml = toml::from_str(s).context("parsing curated tasks TOML")?;
        let by_lang = [
            (PolyglotLang::Python, raw.python),
            (PolyglotLang::Rust, raw.rust),
            (PolyglotLang::Go, raw.go),
            (PolyglotLang::Javascript, raw.javascript),
            (PolyglotLang::Java, raw.java),
            (PolyglotLang::Cpp, raw.cpp),
        ];

        let mut tasks = BTreeMap::new();
        for (lang, list) in by_lang {
            validate_language_list(lang, &list)?;
            tasks.insert(lang, list);
        }

        Ok(Self { tasks })
    }

    /// Read a curated list from disk. Thin wrapper around
    /// `from_toml_str` — kept separate so future `--tasks-from
    /// <path>` CLI wiring has one call site.
    ///
    /// # Errors
    /// Returns an error if the file can't be read or fails validation.
    pub fn from_toml_path(path: &Path) -> anyhow::Result<Self> {
        let body = std::fs::read_to_string(path).with_context(|| format!("reading curated tasks file {}", path.display()))?;
        Self::from_toml_str(&body)
    }

    /// Tasks for a language. Returns the slice directly so callers
    /// can iterate without cloning.
    #[must_use]
    pub fn tasks_for(&self, lang: PolyglotLang) -> &[String] {
        self.tasks.get(&lang).map_or(&[], Vec::as_slice)
    }

    /// Every `(language, task)` pair in a stable ordering (language
    /// alphabetical, task as curated). This is the iteration order
    /// for a `--release` run — stable ordering means two runs on the
    /// same code produce byte-identical JSON except for timings.
    pub fn iter_all(&self) -> impl Iterator<Item = (PolyglotLang, &str)> {
        self.tasks.iter().flat_map(|(lang, tasks)| tasks.iter().map(move |t| (*lang, t.as_str())))
    }

    /// Total number of `(lang, task)` pairs. Convenience for
    /// progress display.
    #[must_use]
    pub fn total(&self) -> usize {
        self.tasks.values().map(Vec::len).sum()
    }
}

fn validate_language_list(lang: PolyglotLang, list: &[String]) -> anyhow::Result<()> {
    let name = lang.dataset_dir();
    if list.len() != TASKS_PER_LANGUAGE {
        return Err(anyhow!(
            "curated list for `{name}` has {} tasks, expected exactly {TASKS_PER_LANGUAGE}",
            list.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for task in list {
        if task.trim().is_empty() {
            return Err(anyhow!("curated list for `{name}` contains an empty task name"));
        }
        if !seen.insert(task) {
            return Err(anyhow!("curated list for `{name}` contains duplicate task `{task}`"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_list_loads_and_validates() {
        let list = CuratedList::default_embedded().expect("embedded list parses");
        for lang in [
            PolyglotLang::Python,
            PolyglotLang::Rust,
            PolyglotLang::Go,
            PolyglotLang::Javascript,
            PolyglotLang::Java,
            PolyglotLang::Cpp,
        ] {
            assert_eq!(
                list.tasks_for(lang).len(),
                TASKS_PER_LANGUAGE,
                "language {} should have exactly {TASKS_PER_LANGUAGE} tasks",
                lang.dataset_dir()
            );
        }
        // 6 langs × 20 tasks = 120 pairs.
        assert_eq!(list.total(), 120);
    }

    #[test]
    fn curated_list_has_exactly_20_per_language() {
        // Explicit per-language check so test names make the
        // invariant obvious when a future curator breaks it.
        let list = CuratedList::default_embedded().unwrap();
        assert_eq!(list.tasks_for(PolyglotLang::Python).len(), 20);
        assert_eq!(list.tasks_for(PolyglotLang::Rust).len(), 20);
        assert_eq!(list.tasks_for(PolyglotLang::Go).len(), 20);
        assert_eq!(list.tasks_for(PolyglotLang::Javascript).len(), 20);
        assert_eq!(list.tasks_for(PolyglotLang::Java).len(), 20);
        assert_eq!(list.tasks_for(PolyglotLang::Cpp).len(), 20);
    }

    #[test]
    fn curated_list_has_no_duplicates_within_a_language() {
        let list = CuratedList::default_embedded().unwrap();
        for lang in [
            PolyglotLang::Python,
            PolyglotLang::Rust,
            PolyglotLang::Go,
            PolyglotLang::Javascript,
            PolyglotLang::Java,
            PolyglotLang::Cpp,
        ] {
            let tasks = list.tasks_for(lang);
            let unique: std::collections::HashSet<&String> = tasks.iter().collect();
            assert_eq!(tasks.len(), unique.len(), "language {} has duplicate tasks", lang.dataset_dir());
        }
    }

    #[test]
    fn rejects_wrong_task_count() {
        let body = r#"
python = ["bowling"]
rust = ["bowling"]
go = ["bowling"]
javascript = ["bowling"]
java = ["bowling"]
cpp = ["bowling"]
"#;
        let err = CuratedList::from_toml_str(body).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("expected exactly 20"), "got: {msg}");
    }

    #[test]
    fn rejects_duplicate_task_within_language() {
        let mut py = vec!["bowling".to_string(); 19];
        py.push("bowling".to_string()); // 20 entries, but duplicated
        let body = format!(
            "python = {py:?}\nrust = {r:?}\ngo = {g:?}\njavascript = {js:?}\njava = {jv:?}\ncpp = {c:?}\n",
            r = twenty_of("r"),
            g = twenty_of("g"),
            js = twenty_of("js"),
            jv = twenty_of("jv"),
            c = twenty_of("c"),
        );
        let err = CuratedList::from_toml_str(&body).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("duplicate task"), "got: {msg}");
        assert!(msg.contains("python"), "got: {msg}");
    }

    #[test]
    fn rejects_empty_task_name() {
        let mut py = twenty_of("py");
        py[3] = String::new();
        let body = format!(
            "python = {py:?}\nrust = {r:?}\ngo = {g:?}\njavascript = {js:?}\njava = {jv:?}\ncpp = {c:?}\n",
            r = twenty_of("r"),
            g = twenty_of("g"),
            js = twenty_of("js"),
            jv = twenty_of("jv"),
            c = twenty_of("c"),
        );
        let err = CuratedList::from_toml_str(&body).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("empty task name"), "got: {msg}");
    }

    #[test]
    fn rejects_unknown_top_level_key() {
        // A typo like `javasript = [...]` must fail loud — otherwise
        // we silently run a 5-language sweep.
        let body = format!(
            "python = {py:?}\nrust = {r:?}\ngo = {g:?}\njavasript = {js:?}\njava = {jv:?}\ncpp = {c:?}\n",
            py = twenty_of("py"),
            r = twenty_of("r"),
            g = twenty_of("g"),
            js = twenty_of("js"),
            jv = twenty_of("jv"),
            c = twenty_of("c"),
        );
        let err = CuratedList::from_toml_str(&body).unwrap_err();
        let msg = format!("{err:#}");
        // `deny_unknown_fields` surfaces via toml's error message.
        assert!(msg.to_lowercase().contains("unknown") || msg.contains("javasript"), "got: {msg}");
    }

    #[test]
    fn iter_all_yields_every_pair_in_stable_order() {
        let list = CuratedList::default_embedded().unwrap();
        let pairs: Vec<(PolyglotLang, &str)> = list.iter_all().collect();
        assert_eq!(pairs.len(), 120);

        // Stable order = language alphabetical (cpp, go, java,
        // javascript, python, rust — BTreeMap<PolyglotLang, _>
        // orders by the enum's derived Ord, which follows variant
        // declaration order: Python, Rust, Go, Javascript, Java, Cpp).
        let first_langs: Vec<_> = pairs.iter().take(20).map(|(l, _)| *l).collect();
        let first = pairs[0].0;
        assert!(first_langs.iter().all(|l| *l == first));
    }

    /// Helper: 20 distinct strings with the given prefix.
    fn twenty_of(prefix: &str) -> Vec<String> {
        (0..20).map(|i| format!("{prefix}-{i}")).collect()
    }
}
