//! `th chrome` — read and write Chrome bookmarks (account-synced included) and, when the user
//! enables it in the extension, drive tabs. Everything goes through the Smooth extension's
//! `chrome.bookmarks` / `chrome.tabs`, reached via the native-messaging host (`host.rs`) on a
//! Unix socket. The ops here back both the CLI and the MCP tools (`mcp_chrome.rs`).

pub mod client;
pub mod history;
pub mod host;
pub mod install;
pub mod native;
pub mod tree;

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use clap::Subcommand;
use serde_json::{json, Value};

use client::{Client, LiveApi};
use history::History;
use tree::{guard_parent, guard_writable, permanent, resolve_target, Api, MemTree, Node, Upserted, BAR};

/// `$SMOOTH_CHROME_DIR`, else `$SMOOTH_HOME/chrome`, else `~/.smooth/chrome`.
pub fn chrome_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("SMOOTH_CHROME_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d));
    }
    if let Some(h) = std::env::var_os("SMOOTH_HOME").filter(|h| !h.is_empty()) {
        return Ok(PathBuf::from(h).join("chrome"));
    }
    Ok(dirs_next::home_dir().context("no home dir")?.join(".smooth").join("chrome"))
}

pub fn sock_dir() -> Result<PathBuf> {
    Ok(chrome_dir()?.join("socks"))
}

fn history() -> Result<History> {
    History::open(&chrome_dir()?.join("history"))
}

// ── CLI ─────────────────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum ChromeCommands {
    /// Unpack the extension and register the native-messaging host
    Install {
        /// chrome, chrome-beta, chrome-canary, chromium, brave, edge
        #[arg(long, default_value = "chrome")]
        browser: String,
    },
    /// Remove the native-messaging host registration (history is kept)
    Uninstall {
        #[arg(long, default_value = "chrome")]
        browser: String,
    },
    /// Connected Chrome instances and whether browsing is enabled
    Status,
    /// The native-messaging host Chrome launches (not for direct use)
    #[command(hide = true)]
    Host {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Read and change bookmarks, synced ones included
    #[command(visible_alias = "bm")]
    Bookmarks {
        /// Which connected Chrome (pid from `th chrome status`) when several are
        #[arg(long, global = true)]
        instance: Option<String>,
        #[command(subcommand)]
        cmd: BookmarkCommands,
    },
    /// Drive tabs: needs "Enable browsing" in the extension popup
    Browse {
        #[arg(long, global = true)]
        instance: Option<String>,
        #[command(subcommand)]
        cmd: BrowseCommands,
    },
}

#[derive(Subcommand, Debug)]
pub enum BookmarkCommands {
    /// Print the tree (or the subtree at PATH), labelled synced/local
    Tree {
        path: Option<String>,
        #[arg(long)]
        depth: Option<usize>,
        #[arg(long)]
        json: bool,
        /// Resolve PATH in the local (not synced) folders
        #[arg(long)]
        local: bool,
    },
    /// Search titles and URLs
    Search {
        query: String,
        #[arg(long)]
        json: bool,
    },
    /// Add a bookmark (default parent: the synced bookmarks bar)
    Add {
        title: String,
        url: String,
        /// Folder path, e.g. "Bookmarks bar/Work"
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        index: Option<usize>,
        #[arg(long)]
        local: bool,
    },
    /// Create a folder path (idempotent)
    Mkdir {
        path: String,
        #[arg(long)]
        local: bool,
    },
    /// Add a bookmark to a folder path unless that URL is already there (idempotent)
    Upsert {
        #[arg(long)]
        path: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        url: String,
        #[arg(long)]
        local: bool,
    },
    /// Move a bookmark or folder (id or path) into a folder
    Move {
        target: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        index: Option<usize>,
        #[arg(long)]
        local: bool,
        /// Required to move a folder
        #[arg(long)]
        yes: bool,
    },
    /// Change a bookmark's title and/or URL
    Rename {
        target: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        local: bool,
    },
    /// Remove a bookmark or folder (dry run without --yes)
    Rm {
        target: String,
        #[arg(long)]
        local: bool,
        #[arg(long)]
        yes: bool,
    },
    /// Dump everything as JSON (default) or Netscape HTML
    Export {
        #[arg(long)]
        html: bool,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Git-backed bookmark history
    History {
        #[command(subcommand)]
        cmd: HistoryCommands,
    },
    /// Make bookmarks match a history snapshot (dry run without --yes)
    Restore {
        /// A sha from `history log`, HEAD~1, origin/<machine>, …
        rev: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum HistoryCommands {
    /// Recent snapshots
    Log {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Snapshot now
    Snapshot {
        #[arg(short, long, default_value = "manual snapshot")]
        message: String,
    },
    /// Set the git remote history pushes to
    Remote {
        url: String,
        /// Push after every snapshot
        #[arg(long)]
        auto_push: bool,
    },
    /// Push to origin/<this machine>
    Push,
    /// Fetch every machine's history from origin
    Fetch,
    /// Where the history repo lives
    Path,
}

#[derive(Subcommand, Debug)]
pub enum BrowseCommands {
    /// List open tabs
    Tabs,
    /// Open a URL in a new tab and wait for it to load
    Open { url: String },
    /// Point a tab at a URL
    Navigate { tab: i64, url: String },
    /// A tab's visible text
    Text { tab: i64 },
    /// Interactive elements with refs for click/type
    Snapshot { tab: i64 },
    /// Click an element from `snapshot`
    Click { tab: i64, r#ref: String },
    /// Type into an element from `snapshot`
    Type {
        tab: i64,
        r#ref: String,
        text: String,
        #[arg(long)]
        submit: bool,
    },
    /// Save a PNG of the tab
    Screenshot {
        tab: i64,
        #[arg(short, long, default_value = "screenshot.png")]
        output: PathBuf,
    },
    /// Close a tab
    Close { tab: i64 },
}

/// Dispatch `th chrome …`.
///
/// # Errors
/// Whatever the op reports.
pub fn dispatch(cmd: ChromeCommands) -> Result<()> {
    let out = match cmd {
        ChromeCommands::Install { browser } => install_text(&browser)?,
        ChromeCommands::Uninstall { browser } => format!("Removed {}", install::uninstall(&browser)?.display()),
        ChromeCommands::Status => status()?,
        ChromeCommands::Host { .. } => return host::run(),
        ChromeCommands::Bookmarks { instance, cmd } => bookmarks_cli(instance.as_deref(), cmd)?,
        ChromeCommands::Browse { instance, cmd } => browse_cli(instance.as_deref(), cmd)?,
    };
    println!("{}", out.trim_end());
    Ok(())
}

fn bookmarks_cli(inst: Option<&str>, cmd: BookmarkCommands) -> Result<String> {
    match cmd {
        BookmarkCommands::Tree { path, depth, json, local } => tree_view(inst, path.as_deref(), depth, json, local),
        BookmarkCommands::Search { query, json } => search(inst, &query, json),
        BookmarkCommands::Add {
            title,
            url,
            parent,
            index,
            local,
        } => add(inst, parent.as_deref(), &title, &url, index, local),
        BookmarkCommands::Mkdir { path, local } => mkdir(inst, &path, local),
        BookmarkCommands::Upsert { path, title, url, local } => upsert(inst, &path, &title, &url, local),
        BookmarkCommands::Move { target, to, index, local, yes } => move_node(inst, &target, &to, index, local, yes),
        BookmarkCommands::Rename { target, title, url, local } => rename(inst, &target, title.as_deref(), url.as_deref(), local),
        BookmarkCommands::Rm { target, local, yes } => remove(inst, &target, local, yes),
        BookmarkCommands::Export { html, output } => {
            let body = export(inst, html)?;
            match output {
                Some(p) => {
                    std::fs::write(&p, body)?;
                    Ok(format!("Wrote {}", p.display()))
                }
                None => Ok(body),
            }
        }
        BookmarkCommands::History { cmd } => history_cli(inst, cmd),
        BookmarkCommands::Restore { rev, yes } => restore(inst, &rev, yes),
    }
}

fn history_cli(inst: Option<&str>, cmd: HistoryCommands) -> Result<String> {
    let h = history()?;
    match cmd {
        HistoryCommands::Log { limit } => Ok(h.log(limit).map(|l| if l.is_empty() { "No snapshots yet.".into() } else { l })?),
        HistoryCommands::Snapshot { message } => snapshot_now(inst, &message),
        HistoryCommands::Remote { url, auto_push } => {
            h.set_remote(&url, auto_push)?;
            Ok(format!(
                "History remote set to {url}{}; this machine pushes to branch '{}'",
                if auto_push { " with auto-push" } else { "" },
                History::branch()
            ))
        }
        HistoryCommands::Push => Ok(format!("Pushed to origin/{}", h.push()?)),
        HistoryCommands::Fetch => Ok(format!("Fetched. Restore from any of:\n{}", h.fetch()?)),
        HistoryCommands::Path => Ok(chrome_dir()?.join("history").display().to_string()),
    }
}

fn browse_cli(inst: Option<&str>, cmd: BrowseCommands) -> Result<String> {
    let pretty = |v: Value| serde_json::to_string_pretty(&v).unwrap_or_default();
    match cmd {
        BrowseCommands::Tabs => tabs(inst),
        BrowseCommands::Open { url } => browse(inst, "tabs.open", json!({"url": url})).map(pretty),
        BrowseCommands::Navigate { tab, url } => browse(inst, "tabs.navigate", json!({"tabId": tab, "url": url})).map(pretty),
        BrowseCommands::Text { tab } => page_text(inst, tab),
        BrowseCommands::Snapshot { tab } => page_snapshot(inst, tab),
        BrowseCommands::Click { tab, r#ref: el } => browse(inst, "page.click", json!({"tabId": tab, "ref": el})).map(|_| format!("Clicked {el}")),
        BrowseCommands::Type { tab, r#ref: el, text, submit } => {
            browse(inst, "page.type", json!({"tabId": tab, "ref": el, "text": text, "submit": submit})).map(|_| format!("Typed into {el}"))
        }
        BrowseCommands::Screenshot { tab, output } => {
            std::fs::write(&output, screenshot(inst, tab)?)?;
            Ok(format!("Wrote {}", output.display()))
        }
        BrowseCommands::Close { tab } => browse(inst, "tabs.close", json!({"tabId": tab})).map(|_| format!("Closed tab {tab}")),
    }
}

// ── ops shared with MCP ─────────────────────────────────────────────────────

fn api(inst: Option<&str>) -> Result<LiveApi> {
    Ok(LiveApi(Client::connect(inst)?))
}

fn install_text(browser: &str) -> Result<String> {
    let i = install::install(browser)?;
    Ok(format!(
        "Registered native host {} → {}\n  (launches {} chrome host)\nUnpacked the extension to {}\n\n\
         Load it once per Chrome profile:\n  1. Open chrome://extensions in the profile whose bookmarks you want\n  \
         2. Turn on Developer mode (top right)\n  3. Load unpacked → {}\n  4. Check its id is {}\n\n\
         Then: th chrome status · th chrome bookmarks tree --depth 1\n\
         After upgrading th, run `th chrome install` again and press reload on the extension.",
        install::HOST_NAME,
        i.manifest.display(),
        i.th.display(),
        i.extension_dir.display(),
        i.extension_dir.display(),
        install::EXTENSION_ID
    ))
}

fn status() -> Result<String> {
    let socks = client::instances(&sock_dir()?);
    if socks.is_empty() {
        return Ok("No Chrome instance connected. Run `th chrome install`, load the extension, and keep Chrome open.".into());
    }
    let mut out = String::new();
    for s in socks {
        let pid = s.file_stem().and_then(|p| p.to_str()).unwrap_or_default().to_string();
        let mut c = Client::at(&s)?;
        let info = c.call("host.info", Value::Null)?;
        let ext = &info["extension"];
        let bar = LiveApi(c).tree().ok().and_then(|t| {
            let b = permanent(&t, BAR, false).or_else(|_| permanent(&t, BAR, true)).ok()?;
            Some(
                b.children
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .take(4)
                    .map(|n| n.title.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        });
        let _ = writeln!(
            out,
            "instance {pid}: extension {} · browsing {} · bar: {}",
            ext["version"].as_str().unwrap_or("?"),
            if ext["browsing"].as_bool() == Some(true) { "on" } else { "off" },
            bar.unwrap_or_else(|| "?".into())
        );
    }
    Ok(out)
}

/// Where a node lives, e.g. "Bookmarks bar (synced)/Work/Jira".
fn path_of(root: &Node, id: &str) -> String {
    let Some(chain) = root.ancestry(id) else { return id.to_string() };
    chain
        .iter()
        .skip(1)
        .map(|n| match &n.folder_type {
            Some(_) => format!("{} ({})", n.title, if n.syncing { "synced" } else { "local" }),
            None => n.title.clone(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn where_note(syncing: bool) -> &'static str {
    if syncing {
        "synced to your Google account"
    } else {
        "LOCAL ONLY, not synced"
    }
}

/// Commit the live tree to history. Before destructive ops this must succeed.
fn record(api: &mut dyn Api, message: &str) -> Result<Option<String>> {
    history()?.snapshot(&api.tree()?, message)
}

fn record_after(api: &mut dyn Api, message: &str) -> String {
    match record(api, message) {
        Ok(Some(sha)) => format!("\nhistory: {sha}"),
        Ok(None) => String::new(),
        Err(e) => format!("\n(warning: history snapshot failed: {e})"),
    }
}

pub fn tree_view(inst: Option<&str>, path: Option<&str>, depth: Option<usize>, as_json: bool, local: bool) -> Result<String> {
    let root = api(inst)?.tree()?;
    let node = match path {
        Some(p) => resolve_target(&root, p, local)?,
        None => &root,
    };
    if as_json {
        return Ok(serde_json::to_string_pretty(node)?);
    }
    Ok(tree::render(node, depth))
}

pub fn search(inst: Option<&str>, query: &str, as_json: bool) -> Result<String> {
    let mut c = Client::connect(inst)?;
    let hits: Vec<Node> = serde_json::from_value(c.call("bookmarks.search", json!({"query": query}))?)?;
    let root = LiveApi(c).tree()?;
    if as_json {
        let rows: Vec<Value> = hits
            .iter()
            .map(|n| json!({"id": n.id, "title": n.title, "url": n.url, "syncing": n.syncing, "path": path_of(&root, &n.id)}))
            .collect();
        return Ok(serde_json::to_string_pretty(&rows)?);
    }
    if hits.is_empty() {
        return Ok(format!("No bookmarks match '{query}'."));
    }
    let mut out = String::new();
    for n in &hits {
        let _ = writeln!(
            out,
            "#{}  {}{}",
            n.id,
            path_of(&root, &n.id),
            n.url.as_deref().map(|u| format!("  <{u}>")).unwrap_or_default()
        );
    }
    Ok(out)
}

pub fn add(inst: Option<&str>, parent: Option<&str>, title: &str, url: &str, index: Option<usize>, local: bool) -> Result<String> {
    let mut a = api(inst)?;
    let root = a.tree()?;
    let p = match parent {
        Some(p) => resolve_target(&root, p, local)?,
        None => permanent(&root, BAR, local)?,
    };
    guard_parent(&root, &p.id)?;
    let syncing = p.syncing;
    let n = a.create(&p.id.clone(), title, Some(url), index)?;
    let root = a.tree()?;
    Ok(format!("Added #{} {} ({})", n.id, path_of(&root, &n.id), where_note(syncing)) + &record_after(&mut a, &format!("add {title}")))
}

pub fn mkdir(inst: Option<&str>, path: &str, local: bool) -> Result<String> {
    let mut a = api(inst)?;
    let e = tree::ensure_folder(&mut a, path, local)?;
    let root = a.tree()?;
    let what = if e.created.is_empty() { "Already there" } else { "Created" };
    let mut out = format!("{what}: #{} {} ({})", e.id, path_of(&root, &e.id), where_note(e.syncing));
    if !e.created.is_empty() {
        out += &record_after(&mut a, &format!("mkdir {path}"));
    }
    Ok(out)
}

pub fn upsert(inst: Option<&str>, path: &str, title: &str, url: &str, local: bool) -> Result<String> {
    let mut a = api(inst)?;
    let (r, folder) = tree::upsert(&mut a, path, title, url, local)?;
    let root = a.tree()?;
    let note = where_note(folder.syncing);
    Ok(match r {
        Upserted::Unchanged(id) => format!("Unchanged: #{id} {} ({note})", path_of(&root, &id)),
        Upserted::Retitled(id) => format!("Retitled: #{id} {} ({note})", path_of(&root, &id)) + &record_after(&mut a, &format!("upsert {title}")),
        Upserted::Created(id) => format!("Created: #{id} {} ({note})", path_of(&root, &id)) + &record_after(&mut a, &format!("upsert {title}")),
    })
}

fn describe(n: &Node) -> String {
    if n.is_folder() {
        let mut count = 0;
        let mut stack: Vec<&Node> = n.children.as_deref().unwrap_or_default().iter().collect();
        while let Some(c) = stack.pop() {
            count += 1;
            stack.extend(c.children.as_deref().unwrap_or_default());
        }
        format!("folder '{}' ({count} item{} inside)", n.title, if count == 1 { "" } else { "s" })
    } else {
        format!("bookmark '{}' <{}>", n.title, n.url.as_deref().unwrap_or_default())
    }
}

pub fn move_node(inst: Option<&str>, target: &str, to: &str, index: Option<usize>, local: bool, confirm: bool) -> Result<String> {
    let mut a = api(inst)?;
    let root = a.tree()?;
    let n = resolve_target(&root, target, local)?;
    let dest = resolve_target(&root, to, local)?;
    guard_writable(&root, &n.id, "move")?;
    guard_parent(&root, &dest.id)?;
    if root.ancestry(&dest.id).is_some_and(|c| c.iter().any(|x| x.id == n.id)) {
        bail!("can't move a folder into itself");
    }
    let (id, folder_id, summary, dest_path) = (n.id.clone(), dest.id.clone(), describe(n), path_of(&root, &dest.id));
    if n.is_folder() {
        if !confirm {
            return Ok(format!(
                "Dry run: would move {summary} into {dest_path}. Re-run with --yes (MCP: confirm=true)."
            ));
        }
        record(&mut a, &format!("before move {}", n.title))?;
    }
    a.move_to(&id, &folder_id, index)?;
    let note = where_note(dest.syncing);
    Ok(format!("Moved {summary} into {dest_path} ({note})") + &record_after(&mut a, &format!("move #{id}")))
}

pub fn rename(inst: Option<&str>, target: &str, title: Option<&str>, url: Option<&str>, local: bool) -> Result<String> {
    if title.is_none() && url.is_none() {
        bail!("give --title and/or --url");
    }
    let mut a = api(inst)?;
    let root = a.tree()?;
    let n = resolve_target(&root, target, local)?;
    guard_writable(&root, &n.id, "rename")?;
    if url.is_some() && n.is_folder() {
        bail!("'{}' is a folder; folders have no URL", n.title);
    }
    let id = n.id.clone();
    a.update(&id, title, url)?;
    let root = a.tree()?;
    Ok(format!("Updated #{id} {}", path_of(&root, &id)) + &record_after(&mut a, &format!("rename #{id}")))
}

pub fn remove(inst: Option<&str>, target: &str, local: bool, confirm: bool) -> Result<String> {
    let mut a = api(inst)?;
    let root = a.tree()?;
    let n = resolve_target(&root, target, local)?;
    guard_writable(&root, &n.id, "remove")?;
    let (desc, path, id, folder) = (describe(n), path_of(&root, &n.id), n.id.clone(), n.is_folder());
    if !confirm {
        return Ok(format!("Dry run: would remove {desc} at {path}. Re-run with --yes (MCP: confirm=true)."));
    }
    let before = record(&mut a, &format!("before remove {path}"))?;
    if folder {
        a.remove_tree(&id)?;
    } else {
        a.remove(&id)?;
    }
    let undo = before.map(|_| " Undo: th chrome bookmarks restore HEAD~1 --yes").unwrap_or_default();
    Ok(format!("Removed {desc} from {path}.{undo}") + &record_after(&mut a, &format!("remove {path}")))
}

pub fn export(inst: Option<&str>, html: bool) -> Result<String> {
    let root = api(inst)?.tree()?;
    Ok(if html {
        tree::to_netscape_html(&root)
    } else {
        serde_json::to_string_pretty(&root)?
    })
}

pub fn snapshot_now(inst: Option<&str>, message: &str) -> Result<String> {
    let mut a = api(inst)?;
    Ok(record(&mut a, message)?.map_or_else(|| "No changes since the last snapshot.".into(), |sha| format!("Snapshot {sha}")))
}

pub fn history_log(limit: usize) -> Result<String> {
    let l = history()?.log(limit)?;
    Ok(if l.is_empty() { "No snapshots yet.".into() } else { l })
}

pub fn restore(inst: Option<&str>, rev: &str, confirm: bool) -> Result<String> {
    let snap = history()?.at(rev)?;
    let mut a = api(inst)?;
    let mut dry = MemTree::new(a.tree()?);
    let plan = tree::restore(&mut dry, &snap)?;
    if plan.is_empty() {
        return Ok(format!("Bookmarks already match {rev}."));
    }
    let list = plan.iter().map(|o| format!("  {o}")).collect::<Vec<_>>().join("\n");
    if !confirm {
        return Ok(format!("Dry run: restoring {rev} would:\n{list}\nRe-run with --yes (MCP: confirm=true)."));
    }
    record(&mut a, &format!("before restore {rev}"))?;
    let done = tree::restore(&mut a, &snap)?;
    Ok(format!("Restored {rev} ({} change{}).", done.len(), if done.len() == 1 { "" } else { "s" }) + &record_after(&mut a, &format!("restore {rev}")))
}

pub fn browse(inst: Option<&str>, method: &str, params: Value) -> Result<Value> {
    Client::connect(inst)?.call(method, params)
}

pub fn tabs(inst: Option<&str>) -> Result<String> {
    let v = browse(inst, "tabs.list", Value::Null)?;
    let mut out = String::new();
    for t in v.as_array().map(Vec::as_slice).unwrap_or_default() {
        let _ = writeln!(
            out,
            "{}{}  {}  <{}>",
            t["tabId"],
            if t["active"] == true { "*" } else { "" },
            t["title"].as_str().unwrap_or_default(),
            t["url"].as_str().unwrap_or_default()
        );
    }
    Ok(out)
}

pub fn page_text(inst: Option<&str>, tab: i64) -> Result<String> {
    let v = browse(inst, "page.text", json!({"tabId": tab}))?;
    Ok(format!(
        "{}\n<{}>\n\n{}",
        v["title"].as_str().unwrap_or_default(),
        v["url"].as_str().unwrap_or_default(),
        v["text"].as_str().unwrap_or_default()
    ))
}

pub fn page_snapshot(inst: Option<&str>, tab: i64) -> Result<String> {
    let v = browse(inst, "page.snapshot", json!({"tabId": tab}))?;
    let mut out = format!("{}\n<{}>\n", v["title"].as_str().unwrap_or_default(), v["url"].as_str().unwrap_or_default());
    for e in v["elements"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let kind = e["role"]
            .as_str()
            .or_else(|| e["type"].as_str())
            .unwrap_or_else(|| e["tag"].as_str().unwrap_or_default());
        let _ = writeln!(
            out,
            "[{}] {kind} \"{}\"{}",
            e["ref"].as_str().unwrap_or_default(),
            e["name"].as_str().unwrap_or_default(),
            e["href"].as_str().map(|h| format!(" → {h}")).unwrap_or_default()
        );
    }
    Ok(out)
}

pub fn screenshot(inst: Option<&str>, tab: i64) -> Result<Vec<u8>> {
    let v = browse(inst, "page.screenshot", json!({"tabId": tab}))?;
    let data = v["dataUrl"]
        .as_str()
        .and_then(|d| d.split_once(','))
        .map(|(_, b)| b)
        .context("no image from Chrome")?;
    Ok(base64::engine::general_purpose::STANDARD.decode(data)?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test idiom")]
mod tests {
    use super::*;
    use tree::tests::sample;

    #[test]
    fn paths_label_synced_and_local_roots() {
        let t = sample();
        assert_eq!(path_of(&t.root, "31"), "Bookmarks bar (synced)/Work/Jira");
        assert_eq!(path_of(&t.root, "10"), "Bookmarks bar (local)/Local thing");
        assert_eq!(path_of(&t.root, "nope"), "nope");
    }

    #[test]
    fn describes_folders_with_counts() {
        let t = sample();
        assert_eq!(describe(t.root.find("30").unwrap()), "folder 'Work' (1 item inside)");
        assert_eq!(describe(t.root.find("31").unwrap()), "bookmark 'Jira' <https://jira.example>");
    }

    #[test]
    fn chrome_dir_honours_overrides() {
        std::env::set_var("SMOOTH_CHROME_DIR", "/tmp/x-chrome");
        assert_eq!(chrome_dir().unwrap(), PathBuf::from("/tmp/x-chrome"));
        assert_eq!(sock_dir().unwrap(), PathBuf::from("/tmp/x-chrome/socks"));
        std::env::remove_var("SMOOTH_CHROME_DIR");
        assert!(chrome_dir().unwrap().ends_with("chrome"));
    }
}
