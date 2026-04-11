//! Shared helpers for `boardroom_e2e.rs` and friends.
//!
//! Kept as a module (not a separate test file) so cargo doesn't try to
//! run it as its own test binary.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};

/// Spawn `bootstrap-bill` as a host subprocess, asking it to listen on an
/// ephemeral port on all interfaces. Returns (child, host_addr).
///
/// Bill prints `BILL_PORT=<port>` on its stdout once the listener is
/// bound. The caller gets back a `SocketAddr` pointed at
/// `127.0.0.1:<that_port>` for in-process BillClient use.
///
/// The returned `Child` should be kept alive for the full test; drop it
/// or call `kill` when done. We don't wrap in `Drop` here because tests
/// usually want explicit control over teardown order.
pub async fn spawn_bill_subprocess() -> anyhow::Result<(Child, std::net::SocketAddr)> {
    let bill_bin = find_workspace_target("release/bootstrap-bill")
        .ok_or_else(|| anyhow::anyhow!("bootstrap-bill binary not found. Run: cargo build --release --bin bootstrap-bill"))?;

    // Bind 0.0.0.0 so the Boardroom VM can reach Bill via host.containers.internal.
    let mut child = Command::new(&bill_bin)
        .arg("--listen")
        .arg("0.0.0.0:0")
        .arg("--print-port")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn bootstrap-bill: {e}"))?;

    // Read stdout lines until we see BILL_PORT=<port>.
    let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("bill: no stdout"))?;
    let (port_tx, port_rx) = tokio::sync::oneshot::channel::<u16>();
    tokio::spawn(async move {
        let mut reader = BufReader::new(tokio::process::ChildStdout::from_std(stdout).expect("wrap stdout"));
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Some(rest) = line.trim().strip_prefix("BILL_PORT=") {
                        if let Ok(port) = rest.parse::<u16>() {
                            let _ = port_tx.send(port);
                            break;
                        }
                    }
                }
            }
        }
    });

    let port = tokio::time::timeout(Duration::from_secs(5), port_rx)
        .await
        .map_err(|_| anyhow::anyhow!("bill: timeout waiting for BILL_PORT= line"))?
        .map_err(|_| anyhow::anyhow!("bill: port channel closed before port was printed"))?;
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().expect("valid addr");
    Ok((child, addr))
}

/// Walk up from `CARGO_MANIFEST_DIR` looking for `target/<relative>`. Used
/// to locate both the host `bootstrap-bill` and cross-compiled binaries.
pub fn find_workspace_target(rel: &str) -> Option<PathBuf> {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let mut dir = PathBuf::from(manifest);
    for _ in 0..5 {
        let candidate = dir.join("target").join(rel);
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Copy every file under `src` into `dst`, creating dirs as needed.
pub fn copy_tree(src: &Path, dst: &Path) {
    let mut stack = vec![src.to_path_buf()];
    while let Some(p) = stack.pop() {
        let rel = p.strip_prefix(src).unwrap();
        let target = dst.join(rel);
        if p.is_dir() {
            std::fs::create_dir_all(&target).expect("mkdir");
            for entry in std::fs::read_dir(&p).expect("read_dir").flatten() {
                stack.push(entry.path());
            }
        } else if p.is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).expect("mkdir parent");
            }
            std::fs::copy(&p, &target).expect("copy file");
        }
    }
}

/// Parse and sum every `test result: ok. N passed; M failed;` line from
/// cargo output. Handles multiple test binaries (unit + integration + doc).
pub fn parse_cargo_test_summary(output: &str) -> Option<(u32, u32)> {
    let mut total_passed = 0u32;
    let mut total_failed = 0u32;
    let mut saw_any = false;
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("test result: ") {
            saw_any = true;
            for token in rest.split(';') {
                let token = token.trim();
                if let Some(n) = token.strip_suffix(" passed") {
                    total_passed += n.trim_start_matches("ok. ").parse().unwrap_or(0);
                } else if let Some(n) = token.strip_suffix(" failed") {
                    total_failed += n.trim_start_matches("FAILED. ").parse().unwrap_or(0);
                }
            }
        }
    }
    saw_any.then_some((total_passed, total_failed))
}

/// Parse vitest's JSON reporter output (`numTotalTests`, `numPassedTests`,
/// `numFailedTests`).
pub fn parse_vitest_summary(json: &str) -> Option<(u32, u32)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let passed = v.get("numPassedTests").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let failed = v.get("numFailedTests").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    Some((passed, failed))
}

/// Poll `url` until a 200 is returned or `deadline` is hit.
pub async fn wait_for_http_ok(url: &str, deadline: Duration) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build()?;
    while start.elapsed() < deadline {
        if let Ok(resp) = client.get(url).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(anyhow::anyhow!("timeout waiting for 200 from {url}"))
}

/// Call an OpenAI-compatible chat completion endpoint as an LLM judge.
/// Returns `(verdict, score, rationale)` as parsed from a strict JSON
/// rubric response.
#[allow(clippy::too_many_arguments)]
pub async fn call_llm_judge(
    api_url: &str,
    api_key: &str,
    model: &str,
    language: &str,
    generated_code: &str,
    test_output: &str,
    passed: u32,
    failed: u32,
) -> anyhow::Result<(String, i64, String)> {
    let url = format!("{}/chat/completions", api_url.trim_end_matches('/'));
    let prompt = format!(
        "You are a strict code review judge for an autonomous agent benchmark.\n\n\
         An AI agent was given pre-written contract tests and asked to implement \
         a small API service in {language} so the tests pass.\n\n\
         OBJECTIVE RESULT: {passed} passed, {failed} failed.\n\n\
         GENERATED CODE:\n```{language}\n{generated_code}\n```\n\n\
         RELEVANT TEST OUTPUT (trimmed):\n```\n{test_output}\n```\n\n\
         Evaluate the implementation on correctness, idiomatic style, API hygiene, \
         error handling, and whether failures (if any) look like minor contract \
         mismatches or deep misunderstandings.\n\n\
         Respond with STRICT JSON only (no prose, no code fences):\n\
         {{\"verdict\": \"pass\" | \"fail\", \"score\": <integer 0-10>, \"rationale\": \"<one paragraph>\"}}"
    );
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": "You are a strict, fair code review judge. Respond with JSON only."},
            {"role": "user", "content": prompt},
        ],
        "temperature": 0.1,
    });
    let client = reqwest::Client::builder().timeout(Duration::from_secs(120)).build()?;
    let resp = client.post(&url).bearer_auth(api_key).json(&body).send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("judge HTTP {status}: {text}");
    }
    let parsed: serde_json::Value = serde_json::from_str(&text)?;
    let content = parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("no choices[0].message.content in judge response"))?;
    let stripped = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let verdict_json: serde_json::Value = serde_json::from_str(stripped)?;
    let verdict = verdict_json.get("verdict").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let score = verdict_json.get("score").and_then(serde_json::Value::as_i64).unwrap_or(-1);
    let rationale = verdict_json.get("rationale").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Ok((verdict, score, rationale))
}
