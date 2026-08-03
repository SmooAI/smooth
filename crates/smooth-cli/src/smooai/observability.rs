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
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// List sourcemaps registered for a (release, environment).
    SourcemapsList {
        /// Release identifier to list sourcemaps for (matches `service.version`).
        #[arg(long)]
        release: String,
        /// Deployment environment (`production`, `staging`, `development`).
        #[arg(long)]
        environment: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Error tracking — list and triage the error groups the dashboard
    /// dedups by fingerprint. `list` to see what's firing, `show` for the
    /// rich detail + recent events, `ok`/`not-ok`/`mute` to set the review
    /// status (resolved / unresolved / muted).
    Errors {
        #[command(subcommand)]
        cmd: ErrorsCmd,
    },
}

#[derive(Subcommand)]
pub enum ErrorsCmd {
    /// List error groups, most-recently-seen first (the triage view).
    List {
        /// Only show groups with this review status: `unresolved` (not-ok),
        /// `resolved` (ok), or `muted`. Default: all.
        #[arg(long)]
        status: Option<String>,
        /// Filter to one environment (`production`, `staging`, `development`).
        #[arg(long)]
        environment: Option<String>,
        /// Max groups to return.
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Pagination cursor (`nextCursor` from a previous page).
        #[arg(long)]
        cursor: Option<String>,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Show one error group + its recent events (full JSON detail).
    Show {
        /// Error group id (the `id` from `errors list`).
        group_id: String,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Mark a group OK — you've reviewed it and it's fine (status → resolved).
    Ok {
        /// Error group id (the `id` from `errors list`).
        group_id: String,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Mark a group NOT OK — it needs attention (status → unresolved).
    #[command(name = "not-ok")]
    NotOk {
        /// Error group id (the `id` from `errors list`).
        group_id: String,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Mute a group — known/accepted, hidden from the default triage view
    /// (status → muted).
    Mute {
        /// Error group id (the `id` from `errors list`).
        group_id: String,
        #[arg(long = "org-id", visible_alias = "org")]
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
                };
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

        Cmd::Errors { cmd } => errors_cmd(&client, cmd).await?,
    }
    Ok(())
}

/// Map the friendly review verbs to the backend `error_group_status` enum.
fn status_for_verb(verb: &str) -> &'static str {
    match verb {
        "ok" => "resolved",
        "not-ok" => "unresolved",
        "mute" => "muted",
        _ => unreachable!("status_for_verb called with {verb}"),
    }
}

async fn errors_cmd(client: &smooth_api_client::SmoothApiClient, cmd: ErrorsCmd) -> Result<()> {
    match cmd {
        ErrorsCmd::List {
            status,
            environment,
            limit,
            cursor,
            json,
            org,
        } => {
            let org = require_active_org(client, org)?;
            let mut qs = format!("limit={limit}");
            if let Some(s) = &status {
                qs.push_str(&format!("&status={}", urlencoding::encode(s)));
            }
            if let Some(e) = &environment {
                qs.push_str(&format!("&environment={}", urlencoding::encode(e)));
            }
            if let Some(c) = &cursor {
                qs.push_str(&format!("&cursor={}", urlencoding::encode(c)));
            }
            let resp = client
                .get(&format!("/organizations/{org}/observability/errors?{qs}"))
                .await
                .context("GET observability/errors")?;
            if json {
                super::print_json(&resp);
                return Ok(());
            }
            print_error_groups(&resp);
        }

        ErrorsCmd::Show { group_id, org } => {
            let org = require_active_org(client, org)?;
            let resp = client
                .get(&format!("/organizations/{org}/observability/errors/{group_id}"))
                .await
                .context("GET observability/errors/:id")?;
            super::print_json(&resp);
        }

        // ok / not-ok / mute all PATCH the same status field — the only
        // difference is the target enum value.
        ErrorsCmd::Ok { group_id, org } => set_status(client, org, &group_id, "ok").await?,
        ErrorsCmd::NotOk { group_id, org } => set_status(client, org, &group_id, "not-ok").await?,
        ErrorsCmd::Mute { group_id, org } => set_status(client, org, &group_id, "mute").await?,
    }
    Ok(())
}

async fn set_status(client: &smooth_api_client::SmoothApiClient, org: Option<String>, group_id: &str, verb: &str) -> Result<()> {
    let org = require_active_org(client, org)?;
    let status = status_for_verb(verb);
    let updated = client
        .patch(
            &format!("/organizations/{org}/observability/errors/{group_id}"),
            &serde_json::json!({ "status": status }),
        )
        .await
        .with_context(|| format!("PATCH observability/errors/{group_id}"))?;
    let title = updated.get("title").and_then(|v| v.as_str()).unwrap_or("(unknown)");
    println!();
    println!("  {} {} → {}", "✓".green(), title.bold(), status.cyan());
    println!("    {} {}", "group:".dimmed(), group_id.dimmed());
    println!();
    Ok(())
}

/// Render the `{ groups: [...], nextCursor }` list as a compact triage table.
fn print_error_groups(resp: &serde_json::Value) {
    let groups = resp.get("groups").and_then(|g| g.as_array()).cloned().unwrap_or_default();
    println!();
    if groups.is_empty() {
        println!("  {} no error groups", "●".dimmed());
        println!();
        return;
    }
    for g in &groups {
        let id = g.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let status = g.get("status").and_then(|v| v.as_str()).unwrap_or("?");
        let level = g.get("level").and_then(|v| v.as_str()).unwrap_or("?");
        let env = g.get("environment").and_then(|v| v.as_str()).unwrap_or("?");
        let count = g.get("eventCount").and_then(|v| v.as_i64()).unwrap_or(0);
        let users = g.get("userCount").and_then(|v| v.as_i64()).unwrap_or(0);
        let last_seen = g.get("lastSeenAt").and_then(|v| v.as_str()).unwrap_or("");
        let title = g.get("title").and_then(|v| v.as_str()).unwrap_or("(untitled)");
        let culprit = g.get("culprit").and_then(|v| v.as_str()).unwrap_or("");

        // status glyph: unresolved = needs attention (red), resolved = ok
        // (green), muted = dim.
        let dot = match status {
            "resolved" => "●".green().to_string(),
            "muted" => "●".dimmed().to_string(),
            _ => "●".red().to_string(),
        };
        println!("  {dot} {}", title.bold());
        println!(
            "    {} {}  {} {}  {} {}  {} {}  {} {}",
            "id:".dimmed(),
            id.dimmed(),
            "status:".dimmed(),
            status,
            "level:".dimmed(),
            level,
            "env:".dimmed(),
            env,
            "seen:".dimmed(),
            format!("{count}× / {users} users").dimmed(),
        );
        if !culprit.is_empty() {
            println!("    {} {}", "at:".dimmed(), culprit.dimmed());
        }
        if !last_seen.is_empty() {
            println!("    {} {}", "last:".dimmed(), last_seen.dimmed());
        }
        println!();
    }
    println!("  {} {} group{}", "●".cyan(), groups.len().bold(), if groups.len() == 1 { "" } else { "s" });
    if let Some(next) = resp.get("nextCursor").and_then(|v| v.as_str()) {
        println!("    {} --cursor {}", "more:".dimmed(), next.dimmed());
    }
    println!();
}

/// Walk `root` and return every file ending in `.map` whose stripped
/// extension corresponds to a JavaScript bundle (`.js`, `.mjs`, `.cjs`).
fn find_sourcemaps(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
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

#[cfg(test)]
mod tests {
    use super::status_for_verb;

    // The friendly review verbs must map to the exact backend
    // `error_group_status` enum values the PATCH endpoint validates —
    // a drift here would send an invalid status and 400.
    #[test]
    fn verbs_map_to_backend_status() {
        assert_eq!(status_for_verb("ok"), "resolved");
        assert_eq!(status_for_verb("not-ok"), "unresolved");
        assert_eq!(status_for_verb("mute"), "muted");
    }
}
