//! `smooth-credential-helper` — Docker credential-helper spec
//! implementation that brokers through Big Smooth's `/api/creds/issue`.
//!
//! Pearl th-08b65f.
//!
//! ## Wire protocol (Docker spec)
//!
//! Invoked as one of `get` / `store` / `erase` / `list`. Reads a
//! single JSON value from stdin (or nothing for `list`); writes JSON
//! to stdout.
//!
//! ### `get`
//! ```json
//! IN:  { "ServerURL": "https://github.com" }
//! OUT: { "Username": "x-access-token", "Secret": "ghs_..." }
//! ```
//!
//! Exits 0 on success, non-zero with stderr describing the failure
//! otherwise. Special exit: if the broker returns 403 (denied or
//! timed out), exit 1 with `credentials not found in native keychain`
//! on stderr — git interprets that as "no helper found, try the
//! next" rather than "fatal error", which makes the helper compose
//! cleanly with other helpers in the chain.
//!
//! ### `store`
//! ```json
//! IN: { "ServerURL": "...", "Username": "...", "Secret": "..." }
//! ```
//! No-op. The helper is read-only; secrets live on the host, not the
//! sandbox.
//!
//! ### `erase`
//! ```json
//! IN: { "ServerURL": "..." }
//! ```
//! Currently a no-op. v2 will call back to Big Smooth to invalidate
//! a cached approval at the runtime layer.
//!
//! ### `list`
//! Empty input. Writes `{}` — we don't surface a username list
//! without a user-facing action (every list call would otherwise
//! trigger a flood of Asks).
//!
//! ## Config
//!
//! - `SMOOTH_BIGSMOOTH_URL` — base URL of Big Smooth. Defaults to
//!   `http://127.0.0.1:4400`. Inside the sandbox the dispatch layer
//!   sets this to the host's routable IP.
//! - `SMOOTH_PEARL_ID` (optional) — bead_id forwarded for audit.
//! - `SMOOTH_OPERATOR_ID` (optional) — operator id forwarded for
//!   audit.

#![allow(clippy::print_stderr, clippy::print_stdout)] // intentional protocol I/O

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::process::ExitCode;

const DEFAULT_BIGSMOOTH_URL: &str = "http://127.0.0.1:4400";
/// Stderr message git interprets as "no creds here, move on" — keeps
/// the helper composable in a multi-helper chain.
const NO_CREDS: &str = "credentials not found in native keychain";

#[derive(Deserialize, Debug)]
struct GetInput {
    // Docker credential-helper spec is literal `ServerURL` (all caps
    // at the end). serde's `rename_all = "PascalCase"` would produce
    // `ServerUrl` which the spec doesn't recognize, so we use an
    // explicit `rename`.
    #[serde(rename = "ServerURL")]
    server_url: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct GetOutput {
    #[serde(rename = "Username")]
    username: String,
    #[serde(rename = "Secret")]
    secret: String,
}

#[derive(Serialize, Debug)]
struct IssueBody<'a> {
    #[serde(rename = "ServerURL")]
    server_url: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    bead_id: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    operator_id: &'a str,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(String::as_str).unwrap_or("");
    match command {
        "get" => match run_get() {
            Ok(()) => ExitCode::SUCCESS,
            Err(GetError::NotFound) => {
                // Compose cleanly with other helpers — write the
                // standard "not found" line and exit 1 so git falls
                // through to its next configured helper.
                eprintln!("{NO_CREDS}");
                ExitCode::from(1)
            }
            Err(GetError::Fatal(msg)) => {
                eprintln!("smooth-credential-helper: {msg}");
                ExitCode::from(2)
            }
        },
        "store" | "erase" => {
            // Drain stdin and ignore — the spec requires we consume
            // the JSON payload even though we don't act on it.
            let _ = drain_stdin();
            ExitCode::SUCCESS
        }
        "list" => {
            // Empty map — we don't surface known servers without an
            // explicit action.
            println!("{{}}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("smooth-credential-helper: unknown command '{other}' (expected one of: get, store, erase, list)");
            ExitCode::from(2)
        }
    }
}

#[derive(Debug)]
enum GetError {
    NotFound,
    Fatal(String),
}

fn run_get() -> Result<(), GetError> {
    let input = read_stdin_json::<GetInput>().map_err(|e| GetError::Fatal(format!("parse stdin: {e}")))?;

    let base = std::env::var("SMOOTH_BIGSMOOTH_URL").unwrap_or_else(|_| DEFAULT_BIGSMOOTH_URL.into());
    let bead_id = std::env::var("SMOOTH_PEARL_ID").unwrap_or_default();
    let operator_id = std::env::var("SMOOTH_OPERATOR_ID").unwrap_or_default();

    let url = format!("{}/api/creds/issue", base.trim_end_matches('/'));
    let body = IssueBody {
        server_url: &input.server_url,
        bead_id: &bead_id,
        operator_id: &operator_id,
    };

    // Block on a single request — the helper exits as soon as it
    // returns, no point in a runtime longer than this.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| GetError::Fatal(format!("tokio init: {e}")))?;
    let url_for_post = url.clone();
    let resp = rt.block_on(async move {
        reqwest::Client::new()
            .post(&url_for_post)
            .json(&body)
            // Generous timeout — the human may take time to react.
            // 90s leaves margin past Big Smooth's 60s hold.
            .timeout(std::time::Duration::from_secs(90))
            .send()
            .await
    });

    let resp = resp.map_err(|e| GetError::Fatal(format!("POST {url}: {e}")))?;
    let status = resp.status();
    let body_text = rt.block_on(async { resp.text().await }).unwrap_or_default();

    if status.as_u16() == 403 {
        // Denied or timed out. Compose with other helpers.
        return Err(GetError::NotFound);
    }
    if !status.is_success() {
        return Err(GetError::Fatal(format!("HTTP {status}: {body_text}")));
    }

    let cred: GetOutput = serde_json::from_str(&body_text).map_err(|e| GetError::Fatal(format!("parse credential JSON: {e}; body: {body_text}")))?;
    // Serialize back out — round-trip ensures the casing is right
    // (Username/Secret in PascalCase, per spec).
    let out_json = serde_json::to_string(&cred).map_err(|e| GetError::Fatal(format!("serialize output: {e}")))?;
    let mut stdout = std::io::stdout();
    stdout
        .write_all(out_json.as_bytes())
        .map_err(|e| GetError::Fatal(format!("write stdout: {e}")))?;
    Ok(())
}

fn read_stdin_json<T: serde::de::DeserializeOwned>() -> std::io::Result<T> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    serde_json::from_str(&buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn drain_stdin() -> std::io::Result<()> {
    let mut sink = String::new();
    std::io::stdin().read_to_string(&mut sink)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_round_trip_uses_pascal_case() {
        let raw = r#"{"ServerURL":"https://github.com"}"#;
        let input: GetInput = serde_json::from_str(raw).expect("parse");
        assert_eq!(input.server_url, "https://github.com");
    }

    #[test]
    fn output_round_trip_uses_pascal_case() {
        let out = GetOutput {
            username: "x-access-token".into(),
            secret: "ghs_aaaa".into(),
        };
        let json = serde_json::to_string(&out).expect("serialize");
        assert!(json.contains("\"Username\""));
        assert!(json.contains("\"Secret\""));
        // Reparse round-trips.
        let back: GetOutput = serde_json::from_str(&json).expect("reparse");
        assert_eq!(back.username, "x-access-token");
        assert_eq!(back.secret, "ghs_aaaa");
    }

    #[test]
    fn issue_body_omits_empty_audit_fields() {
        // bead_id / operator_id are optional — they should be skipped
        // when empty so the server side doesn't get tripped by stray
        // empty strings.
        let b = IssueBody {
            server_url: "https://github.com",
            bead_id: "",
            operator_id: "",
        };
        let json = serde_json::to_string(&b).unwrap();
        assert!(!json.contains("bead_id"));
        assert!(!json.contains("operator_id"));
        assert!(json.contains("\"ServerURL\":\"https://github.com\""));

        // Populated fields come through.
        let b = IssueBody {
            server_url: "https://github.com",
            bead_id: "pearl-1",
            operator_id: "op",
        };
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains("\"bead_id\":\"pearl-1\""));
        assert!(json.contains("\"operator_id\":\"op\""));
    }

    #[test]
    fn no_creds_message_matches_git_convention() {
        // Git's credential framework recognizes this exact string
        // as a "fall through to the next helper" signal.
        assert_eq!(NO_CREDS, "credentials not found in native keychain");
    }
}
