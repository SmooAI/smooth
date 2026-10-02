//! Which files are noise a reviewer skips.
//!
//! Lockfiles, generated and vendored code, minified bundles, and anything
//! huge. Noise starts
//! collapsed and its hunks are left out of the full-diff frame until a
//! client asks for that one file.

use super::model::Noise;

/// More changed lines than this and a file is `large` noise.
pub const LARGE_FILE_LINES: u32 = 1500;

const LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "Gemfile.lock",
    "poetry.lock",
    "uv.lock",
    "Pipfile.lock",
    "composer.lock",
    "Podfile.lock",
    "Package.resolved",
    "go.sum",
    "flake.lock",
    "mix.lock",
    "pubspec.lock",
    "gradle.lockfile",
    "deno.lock",
    "packages.lock.json",
];

const MINIFIED_SUFFIXES: &[&str] = &[".min.js", ".min.css", ".min.mjs", ".map"];

const VENDORED_DIRS: &[&str] = &["vendor", "vendored", "third_party", "third-party", "node_modules", "bower_components", "Pods"];

const GENERATED_DIRS: &[&str] = &["generated", "__generated__", "gen", "__snapshots__"];

const GENERATED_SUFFIXES: &[&str] = &[
    ".pb.go",
    "_pb2.py",
    "_pb2_grpc.py",
    ".pb.rs",
    ".g.dart",
    ".freezed.dart",
    ".snap",
    ".designer.cs",
];

/// Classify by path, then by the added lines' shape (`minified`), then by size.
#[must_use]
pub fn classify(path: &str, added: u32, deleted: u32, added_lines: &[&str]) -> Option<Noise> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let dirs: Vec<&str> = path.split('/').collect();
    let dirs = &dirs[..dirs.len().saturating_sub(1)];
    let lower = name.to_ascii_lowercase();
    if LOCKFILES.contains(&name) || lower.ends_with(".lockfile") {
        return Some(Noise::Lockfile);
    }
    if dirs.iter().any(|d| VENDORED_DIRS.iter().any(|v| v.eq_ignore_ascii_case(d))) {
        return Some(Noise::Vendored);
    }
    if lower.contains(".generated.")
        || lower.contains("_generated.")
        || lower.contains(".gen.")
        || GENERATED_SUFFIXES.iter().any(|s| lower.ends_with(s))
        || dirs.iter().any(|d| GENERATED_DIRS.contains(d))
    {
        return Some(Noise::Generated);
    }
    if MINIFIED_SUFFIXES.iter().any(|s| lower.ends_with(s)) {
        return Some(Noise::Minified);
    }
    // Content: a few very long added lines is a bundle, not source.
    let long = added_lines.iter().filter(|l| l.len() > 500).count();
    if !added_lines.is_empty() && long > 0 && long * 2 >= added_lines.len() {
        return Some(Noise::Minified);
    }
    if added.saturating_add(deleted) > LARGE_FILE_LINES {
        return Some(Noise::Large);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_path() {
        assert_eq!(classify("Cargo.lock", 1, 1, &[]), Some(Noise::Lockfile));
        assert_eq!(classify("apps/web/pnpm-lock.yaml", 1, 1, &[]), Some(Noise::Lockfile));
        assert_eq!(classify("android/app/gradle.lockfile", 1, 0, &[]), Some(Noise::Lockfile));
        assert_eq!(classify("vendor/github.com/x/y.go", 1, 0, &[]), Some(Noise::Vendored));
        assert_eq!(classify("apps/smoothflow/Vendor/thing.h", 1, 0, &[]), Some(Noise::Vendored));
        assert_eq!(classify("src/api.generated.ts", 1, 0, &[]), Some(Noise::Generated));
        assert_eq!(classify("proto/x.pb.go", 1, 0, &[]), Some(Noise::Generated));
        assert_eq!(classify("src/__generated__/schema.ts", 1, 0, &[]), Some(Noise::Generated));
        assert_eq!(classify("tests/__snapshots__/a.snap", 1, 0, &[]), Some(Noise::Generated));
        assert_eq!(classify("dist/app.min.js", 1, 0, &[]), Some(Noise::Minified));
        assert_eq!(classify("src/main.rs", 10, 2, &["fn main() {}"]), None);
        assert_eq!(classify("lockfile.rs", 1, 0, &[]), None, "a name that merely mentions lock");
        assert_eq!(classify("src/generator.rs", 1, 0, &[]), None);
    }

    #[test]
    fn classifies_by_content_and_size() {
        let bundle = "x".repeat(600);
        assert_eq!(classify("public/app.js", 1, 0, &[bundle.as_str()]), Some(Noise::Minified));
        assert_eq!(
            classify("public/app.js", 3, 0, &[bundle.as_str(), "a", "b", "c"]),
            None,
            "one long line among many"
        );
        assert_eq!(classify("src/big.rs", LARGE_FILE_LINES, 1, &[]), Some(Noise::Large));
        assert_eq!(classify("src/big.rs", LARGE_FILE_LINES, 0, &[]), None);
    }
}
