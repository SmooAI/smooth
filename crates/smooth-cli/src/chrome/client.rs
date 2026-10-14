//! Talks to a running `th chrome host` over its Unix socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use super::tree::{Api, Node};

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    /// Connect to one host socket.
    ///
    /// # Errors
    /// When nothing is listening there.
    pub fn at(path: &Path) -> Result<Self> {
        let s = UnixStream::connect(path).with_context(|| format!("connect {}", path.display()))?;
        s.set_read_timeout(Some(Duration::from_secs(120)))?;
        Ok(Self {
            reader: BufReader::new(s.try_clone()?),
            writer: s,
        })
    }

    /// Connect to the only running host, or the one whose pid is `instance`.
    ///
    /// # Errors
    /// No host running (extension not loaded / Chrome closed), or several and no `instance`.
    pub fn connect(instance: Option<&str>) -> Result<Self> {
        let live = instances(&super::sock_dir()?);
        if let Some(want) = instance {
            let p = live
                .iter()
                .find(|p| p.file_stem().is_some_and(|s| s == want))
                .ok_or_else(|| anyhow!("no Chrome instance {want}; see `th chrome status`"))?;
            return Self::at(p);
        }
        match live.as_slice() {
            [] => bail!(
                "the Smooth Chrome extension isn't connected. Is Chrome running with it loaded? Set it up with `th chrome install`, then load the unpacked extension it prints"
            ),
            [one] => Self::at(one),
            many => bail!(
                "{} Chrome profiles have the extension connected; pick one with --instance <{}> (see `th chrome status`)",
                many.len(),
                many.iter().filter_map(|p| p.file_stem()?.to_str()).collect::<Vec<_>>().join("|")
            ),
        }
    }

    /// One request/response round trip.
    ///
    /// # Errors
    /// Transport failure or the extension's error message.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let mut req = json!({ "method": method });
        req["params"] = params;
        writeln!(self.writer, "{req}")?;
        self.writer.flush()?;
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            bail!("the Chrome host closed the connection");
        }
        let mut reply: Value = serde_json::from_str(&line).context("bad reply from the Chrome host")?;
        if let Some(e) = reply.get("error") {
            bail!("{}", e.get("message").and_then(Value::as_str).unwrap_or("unknown error"));
        }
        Ok(reply.get_mut("result").map(Value::take).unwrap_or_default())
    }
}

/// Sockets with a live host behind them. Stale ones (host killed) are removed.
pub fn instances(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sock"))
        .filter(|p| {
            let ok = UnixStream::connect(p).is_ok();
            if !ok {
                let _ = std::fs::remove_file(p);
            }
            ok
        })
        .collect();
    out.sort();
    out
}

/// [`Api`] over the live extension.
pub struct LiveApi(pub Client);

fn node(v: Value) -> Result<Node> {
    serde_json::from_value(v).context("unexpected bookmark shape from Chrome")
}

fn nodes(v: Value) -> Result<Vec<Node>> {
    serde_json::from_value(v).context("unexpected bookmark shape from Chrome")
}

impl Api for LiveApi {
    fn tree(&mut self) -> Result<Node> {
        nodes(self.0.call("bookmarks.getTree", Value::Null)?)?
            .into_iter()
            .next()
            .context("Chrome returned an empty tree")
    }

    fn children(&mut self, id: &str) -> Result<Vec<Node>> {
        nodes(self.0.call("bookmarks.getChildren", json!({"id": id}))?)
    }

    fn create(&mut self, parent_id: &str, title: &str, url: Option<&str>, index: Option<usize>) -> Result<Node> {
        node(
            self.0
                .call("bookmarks.create", json!({"parentId": parent_id, "title": title, "url": url, "index": index}))?,
        )
    }

    fn update(&mut self, id: &str, title: Option<&str>, url: Option<&str>) -> Result<Node> {
        node(self.0.call("bookmarks.update", json!({"id": id, "title": title, "url": url}))?)
    }

    fn move_to(&mut self, id: &str, parent_id: &str, index: Option<usize>) -> Result<Node> {
        node(self.0.call("bookmarks.move", json!({"id": id, "parentId": parent_id, "index": index}))?)
    }

    fn remove(&mut self, id: &str) -> Result<()> {
        self.0.call("bookmarks.remove", json!({"id": id })).map(drop)
    }

    fn remove_tree(&mut self, id: &str) -> Result<()> {
        self.0.call("bookmarks.removeTree", json!({"id": id})).map(drop)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test idiom")]
mod tests {
    use super::*;

    #[test]
    fn instances_prunes_stale_sockets() {
        let dir = tempfile::Builder::new().prefix("sc").tempdir_in("/tmp").unwrap();
        let live = dir.path().join("1.sock");
        let _keep = std::os::unix::net::UnixListener::bind(&live).unwrap();
        let stale = dir.path().join("2.sock");
        drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        assert_eq!(instances(dir.path()), vec![live]);
        assert!(!stale.exists());
        assert!(instances(&dir.path().join("missing")).is_empty());
    }
}
