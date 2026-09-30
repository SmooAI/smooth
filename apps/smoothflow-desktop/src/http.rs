//! The two HTTP reads the New Session sheet makes (spec §2, §6):
//! `GET /api/flow/repos?q=` and `GET /api/flow/infer?cwd=`, with the local
//! token in `X-Smooth-Token`. The daemon is plain HTTP on localhost (or a
//! WSL/remote address the user pointed us at), so this is a small blocking
//! HTTP/1.1 client run off the UI thread — no TLS, no pooling.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::discovery::Endpoint;

const TIMEOUT: Duration = Duration::from_secs(5);

/// A repo index row (spec §3).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Repo {
    pub path: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub branch: Option<String>,
    /// For a linked worktree: its main checkout.
    #[serde(default)]
    pub main: Option<String>,
}

/// `GET /api/flow/repos`'s body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RepoList {
    #[serde(default)]
    pub repos: Vec<Repo>,
    /// A scan is running; more rows may appear.
    #[serde(default)]
    pub scanning: bool,
    #[serde(default)]
    pub indexed: bool,
}

/// `GET /api/flow/infer`'s body (spec §3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Inferred {
    #[serde(default)]
    pub worktree: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub is_git: bool,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub pearl_id: Option<String>,
    #[serde(default)]
    pub pearl_title: Option<String>,
    #[serde(default)]
    pub jira_key: Option<String>,
    #[serde(default)]
    pub title: String,
}

/// The request line's target for the repo search.
#[must_use]
pub fn repos_path(query: &str, limit: usize) -> String {
    format!("/api/flow/repos?q={}&limit={limit}", urlencoding::encode(query))
}

/// The request line's target for inference; no `cwd` means the daemon's workspace.
#[must_use]
pub fn infer_path(cwd: Option<&str>) -> String {
    match cwd.filter(|c| !c.trim().is_empty()) {
        Some(c) => format!("/api/flow/infer?cwd={}", urlencoding::encode(c)),
        None => "/api/flow/infer".to_string(),
    }
}

/// The request bytes.
#[must_use]
pub fn request(endpoint: &Endpoint, path: &str) -> String {
    let token = endpoint.token.as_deref().map(|t| format!("X-Smooth-Token: {t}\r\n")).unwrap_or_default();
    format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\n{token}Connection: close\r\n\r\n",
        endpoint.addr
    )
}

/// Split a raw response into status and body, undoing chunked encoding.
///
/// # Errors
/// When the response is not HTTP.
pub fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("no header terminator")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or("no status line")?;
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    Ok((status, if chunked { dechunk(body) } else { body.to_vec() }))
}

/// Undo `Transfer-Encoding: chunked`; stops at the terminal chunk or at
/// whatever is malformed.
#[must_use]
pub fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(eol) = body.windows(2).position(|w| w == b"\r\n") {
        let size_text = String::from_utf8_lossy(&body[..eol]);
        let Ok(size) = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16) else {
            break;
        };
        let start = eol + 2;
        if size == 0 || start + size > body.len() {
            break;
        }
        out.extend_from_slice(&body[start..start + size]);
        body = body.get(start + size + 2..).unwrap_or(&[]);
    }
    out
}

/// GET `path` and parse the JSON body.
///
/// # Errors
/// Connection, HTTP status or JSON failures, as text for the sheet to show.
pub fn get_json(endpoint: &Endpoint, path: &str) -> Result<Value, String> {
    let addr = endpoint
        .addr
        .to_socket_addrs()
        .map_err(|e| format!("{}: {e}", endpoint.addr))?
        .next()
        .ok_or_else(|| format!("{}: no address", endpoint.addr))?;
    let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
    stream.write_all(request(endpoint, path).as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let (status, body) = parse_response(&raw)?;
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}"));
    }
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

/// Search the repo index.
///
/// # Errors
/// As [`get_json`].
pub fn repos(endpoint: &Endpoint, query: &str) -> Result<RepoList, String> {
    serde_json::from_value(get_json(endpoint, &repos_path(query, 20))?).map_err(|e| e.to_string())
}

/// Infer the session context of `cwd`.
///
/// # Errors
/// As [`get_json`].
pub fn infer(endpoint: &Endpoint, cwd: Option<&str>) -> Result<Inferred, String> {
    serde_json::from_value(get_json(endpoint, &infer_path(cwd))?).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_carry_the_token_and_encode_the_query() {
        let e = Endpoint {
            addr: "127.0.0.1:9".into(),
            token: Some("tok".into()),
        };
        let r = request(&e, &repos_path("my repo", 20));
        assert!(r.starts_with("GET /api/flow/repos?q=my%20repo&limit=20 HTTP/1.1\r\n"), "{r}");
        assert!(r.contains("X-Smooth-Token: tok\r\n") && r.ends_with("\r\n\r\n"));
        let open = Endpoint {
            addr: "h:1".into(),
            token: None,
        };
        assert!(!request(&open, "/x").contains("X-Smooth-Token"));
        assert_eq!(infer_path(Some("/w/a b")), "/api/flow/infer?cwd=%2Fw%2Fa%20b");
        assert_eq!(infer_path(Some(" ")), "/api/flow/infer");
    }

    #[test]
    fn responses_parse_plain_and_chunked() {
        let plain = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}";
        assert_eq!(parse_response(plain), Ok((200, b"{}".to_vec())));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked), Ok((200, b"{\"a\":1}".to_vec())));
        assert_eq!(parse_response(b"HTTP/1.1 401 Unauthorized\r\n\r\n").map(|r| r.0), Ok(401));
        assert!(parse_response(b"garbage").is_err());
        assert_eq!(dechunk(b"zz\r\nabc"), Vec::<u8>::new(), "malformed stops");
    }

    #[test]
    fn bodies_deserialize_from_the_daemon_shapes() {
        let list: RepoList = serde_json::from_str(
            r#"{"repos":[{"path":"/w/smooth","name":"smooth","branch":"main","touched":1},{"path":"/w/smooth-x","name":"smooth-x","main":"/w/smooth","touched":2}],"scanning":true,"indexed":true}"#,
        )
        .unwrap_or_default();
        assert_eq!(list.repos.len(), 2);
        assert_eq!(list.repos[1].main.as_deref(), Some("/w/smooth"));
        assert!(list.scanning);
        let inf: Inferred =
            serde_json::from_str(r#"{"cwd":"/w/x","worktree":"/w/x","project":"/w/x","is_git":true,"branch":"th-1-x","pearl_id":"th-1","title":"Fix it"}"#)
                .unwrap_or_default();
        assert_eq!((inf.pearl_id.as_deref(), inf.title.as_str(), inf.is_git), (Some("th-1"), "Fix it", true));
    }
}
