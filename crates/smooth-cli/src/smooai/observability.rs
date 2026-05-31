//! `th observability …` — observability dashboard commands.
//!
//! Today: just `sourcemaps upload <dir>` for the Error Tracking
//! symbolication path (SMOODEV-1164). The dashboard de-obfuscates stack
//! frames by joining `error_sourcemaps` on `(release_id, file_path)`,
//! so this CLI walks a build directory, finds every `.js`/`.mjs` paired
//! with a `.map`, registers each map via the backend API, and PUTs the
//! bytes to the presigned S3 URL the API returns.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use walkdir::WalkDir;

use super::{require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// Upload all .map files under `<dir>` to the Observability sourcemaps
    /// store, registered against (release, environment) so the Error
    /// Tracking dashboard can symbolicate stack frames.
    SourcemapsUpload {
        /// Directory to walk for .js/.mjs + .map pairs (e.g.
        /// `apps/web/.next/static`, `dist/`, `.open-next/server-function`).
        dir: PathBuf,
        /// Release identifier — must match `service.version` in your
        /// observability config (typically the git sha or semver tag).
        #[arg(long)]
        release: String,
        /// Deployment environment (`production`, `staging`, `development`).
        #[arg(long)]
        environment: String,
        /// Optional git sha; stored on the release row for cross-reference.
        #[arg(long)]
        git_sha: Option<String>,
        /// Strip this prefix from each file path before registering — so
        /// `.next/static/chunks/main.js.map` ends up as `static/chunks/
        /// main.js`. Optional; defaults to the directory you uploaded.
        #[arg(long)]
        strip_prefix: Option<PathBuf>,
        /// Don't upload — just print the file list + computed paths.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        org: Option<String>,
    },
    /// List sourcemaps registered for a (release, environment).
    SourcemapsList {
        #[arg(long)]
        release: String,
        #[arg(long)]
        environment: String,
        #[arg(long)]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::SourcemapsUpload {
            dir,
            release,
            environment,
            git_sha,
            strip_prefix,
            dry_run,
            org,
        } => {
            let org = require_active_org(&client, org)?;
            let dir = dir.canonicalize().with_context(|| format!("canonicalize {}", dir.display()))?;
            let strip = strip_prefix.unwrap_or_else(|| dir.clone());

            let maps = find_sourcemaps(&dir)?;
            if maps.is_empty() {
                println!();
                println!("  {} no .map files found under {}", "●".dimmed(), dir.display().to_string().dimmed());
                println!();
                return Ok(());
            }

            println!();
            println!("  {} {} sourcemap{}", "●".cyan(), maps.len().bold(), if maps.len() == 1 { "" } else { "s" });
            println!("    {} {}", "release:".dimmed(), release);
            println!("    {} {}", "environment:".dimmed(), environment);
            println!("    {} {}", "dir:".dimmed(), dir.display());
            println!();

            let http = reqwest::Client::new();
            let mut uploaded = 0usize;
            let mut failed: Vec<(PathBuf, String)> = Vec::new();

            for map_path in &maps {
                // Derive the file path the backend stores against. We strip
                // the `.map` suffix so it matches the JS path the runtime
                // sees, then trim `strip_prefix` so the stored path is
                // build-relative (not absolute).
                let js_path = map_path.with_extension("");
                let stored = js_path.strip_prefix(&strip).unwrap_or(js_path.as_path()).to_string_lossy().into_owned();

                if dry_run {
                    println!("  [dry-run] {}", stored.dimmed());
                    continue;
                }

                let body = serde_json::json!({
                    "releaseId": release,
                    "environment": environment,
                    "gitSha": git_sha,
                    "filePath": stored,
                });
                let resp = match client.post(&format!("/organizations/{org}/observability/sourcemaps/upload"), Some(&body)).await {
                    Ok(v) => v,
                    Err(e) => {
                        failed.push((map_path.clone(), format!("API: {e}")));
                        continue;
                    }
                };
                let Some(upload_url) = resp.get("uploadUrl").and_then(|v| v.as_str()) else {
                    failed.push((map_path.clone(), "API returned no uploadUrl".to_owned()));
                    continue;
                };

                let bytes = match std::fs::read(map_path) {
                    Ok(b) => b,
                    Err(e) => {
                        failed.push((map_path.clone(), format!("read map: {e}")));
                        continue;
                    }
                };

                let put = http.put(upload_url).header("content-type", "application/json").body(bytes).send().await;
                match put {
                    Ok(r) if r.status().is_success() => {
                        uploaded += 1;
                        println!("  {} {}", "✓".green(), stored);
                    }
                    Ok(r) => {
                        let status = r.status();
                        let body = r.text().await.unwrap_or_default();
                        failed.push((map_path.clone(), format!("S3 PUT {status}: {body}")));
                    }
                    Err(e) => failed.push((map_path.clone(), format!("S3 PUT: {e}"))),
                }
            }

            println!();
            println!(
                "  {} {} uploaded · {} failed",
                if failed.is_empty() {
                    "✓".green().to_string()
                } else {
                    "!".yellow().to_string()
                },
                uploaded.bold(),
                failed.len().bold(),
            );
            for (path, err) in &failed {
                println!("    {} {} — {}", "✗".red(), path.display().to_string().dimmed(), err);
            }
            println!();

            if !failed.is_empty() {
                anyhow::bail!("{} sourcemap upload(s) failed", failed.len());
            }
        }

        Cmd::SourcemapsList { release, environment, org } => {
            let org = require_active_org(&client, org)?;
            let path = format!(
                "/organizations/{org}/observability/sourcemaps/list?releaseId={}&environment={}",
                urlencoding::encode(&release),
                urlencoding::encode(&environment),
            );
            let resp = client.get(&path).await.context("GET sourcemaps/list")?;
            super::print_json(&resp);
        }
    }
    Ok(())
}

/// Walk `root` and return every file ending in `.map` whose stripped
/// extension corresponds to a JavaScript bundle (`.js`, `.mjs`, `.cjs`).
fn find_sourcemaps(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(std::result::Result::ok) {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "map" {
            continue;
        }
        let stripped = p.with_extension("");
        let Some(inner_ext) = stripped.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if matches!(inner_ext, "js" | "mjs" | "cjs") {
            out.push(p.to_path_buf());
        }
    }
    out.sort();
    Ok(out)
}
