//! The bookmark model and every higher-level operation (paths, upsert, restore),
//! written against [`Api`] so the same code runs live, as a dry run, and in tests.

use std::collections::HashSet;
use std::fmt::Write as _;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_added: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_type: Option<String>,
    #[serde(default)]
    pub syncing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmodifiable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<Self>>,
}

impl Node {
    pub const fn is_folder(&self) -> bool {
        self.url.is_none()
    }

    pub fn find(&self, id: &str) -> Option<&Self> {
        if self.id == id {
            return Some(self);
        }
        self.children.as_deref().unwrap_or_default().iter().find_map(|c| c.find(id))
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut Self> {
        if self.id == id {
            return Some(self);
        }
        self.children.as_deref_mut().unwrap_or_default().iter_mut().find_map(|c| c.find_mut(id))
    }

    /// Ids from the root down to `id`, inclusive.
    pub fn ancestry(&self, id: &str) -> Option<Vec<&Self>> {
        if self.id == id {
            return Some(vec![self]);
        }
        for c in self.children.as_deref().unwrap_or_default() {
            if let Some(mut chain) = c.ancestry(id) {
                chain.insert(0, self);
                return Some(chain);
            }
        }
        None
    }

    fn kids(&self) -> &[Self] {
        self.children.as_deref().unwrap_or_default()
    }

    fn reindex(&mut self) {
        let pid = self.id.clone();
        for (i, c) in self.children.as_deref_mut().unwrap_or_default().iter_mut().enumerate() {
            c.index = Some(i);
            c.parent_id = Some(pid.clone());
        }
    }

    /// A copy without children, as `getChildren` returns.
    fn shallow(&self) -> Self {
        Self {
            children: None,
            ..self.clone()
        }
    }
}

/// What the extension exposes. Everything above it is pure.
pub trait Api {
    fn tree(&mut self) -> Result<Node>;
    fn children(&mut self, id: &str) -> Result<Vec<Node>>;
    fn create(&mut self, parent_id: &str, title: &str, url: Option<&str>, index: Option<usize>) -> Result<Node>;
    fn update(&mut self, id: &str, title: Option<&str>, url: Option<&str>) -> Result<Node>;
    fn move_to(&mut self, id: &str, parent_id: &str, index: Option<usize>) -> Result<Node>;
    fn remove(&mut self, id: &str) -> Result<()>;
    fn remove_tree(&mut self, id: &str) -> Result<()>;
}

/// An in-memory tree with Chrome's semantics: backs dry runs and tests.
#[derive(Debug, Clone)]
pub struct MemTree {
    pub root: Node,
    next_id: u64,
}

impl MemTree {
    pub fn new(root: Node) -> Self {
        let mut max = 0;
        let mut stack = vec![&root];
        while let Some(n) = stack.pop() {
            max = max.max(n.id.parse().unwrap_or(0));
            stack.extend(n.kids());
        }
        Self { root, next_id: max + 1 }
    }

    fn node_mut(&mut self, id: &str) -> Result<&mut Node> {
        self.root.find_mut(id).ok_or_else(|| anyhow!("Can't find bookmark for id."))
    }

    fn detach(&mut self, id: &str) -> Result<(Node, String, usize)> {
        let parent_id = self
            .root
            .find(id)
            .and_then(|n| n.parent_id.clone())
            .ok_or_else(|| anyhow!("Can't find bookmark for id."))?;
        let parent = self.node_mut(&parent_id)?;
        let kids = parent.children.get_or_insert_with(Vec::new);
        let pos = kids.iter().position(|c| c.id == id).context("corrupt tree")?;
        let node = kids.remove(pos);
        parent.reindex();
        Ok((node, parent_id, pos))
    }

    fn attach(&mut self, mut node: Node, parent_id: &str, index: Option<usize>) -> Result<Node> {
        let parent = self.node_mut(parent_id)?;
        if !parent.is_folder() {
            bail!("Can't add a bookmark to a non-folder");
        }
        let kids = parent.children.get_or_insert_with(Vec::new);
        let at = index.unwrap_or(kids.len()).min(kids.len());
        node.parent_id = Some(parent_id.to_string());
        kids.insert(at, node);
        parent.reindex();
        Ok(parent.kids()[at].clone())
    }
}

impl Api for MemTree {
    fn tree(&mut self) -> Result<Node> {
        Ok(self.root.clone())
    }

    fn children(&mut self, id: &str) -> Result<Vec<Node>> {
        Ok(self.node_mut(id)?.kids().iter().map(Node::shallow).collect())
    }

    fn create(&mut self, parent_id: &str, title: &str, url: Option<&str>, index: Option<usize>) -> Result<Node> {
        let syncing = self.root.find(parent_id).is_some_and(|p| p.syncing);
        let node = Node {
            id: self.next_id.to_string(),
            parent_id: None,
            index: None,
            title: title.to_string(),
            url: url.map(str::to_string),
            date_added: None,
            folder_type: None,
            syncing,
            unmodifiable: None,
            children: url.is_none().then(Vec::new),
        };
        self.next_id += 1;
        self.attach(node, parent_id, index)
    }

    fn update(&mut self, id: &str, title: Option<&str>, url: Option<&str>) -> Result<Node> {
        let n = self.node_mut(id)?;
        if let Some(t) = title {
            t.clone_into(&mut n.title);
        }
        if let Some(u) = url {
            n.url = Some(u.to_string());
        }
        Ok(n.shallow())
    }

    fn move_to(&mut self, id: &str, parent_id: &str, index: Option<usize>) -> Result<Node> {
        let (node, old_parent, old_pos) = self.detach(id)?;
        // Chrome's index counts the moving node's old slot when it stays in the same folder.
        let index = index.map(|i| if old_parent == parent_id && i > old_pos { i - 1 } else { i });
        self.attach(node, parent_id, index)
    }

    fn remove(&mut self, id: &str) -> Result<()> {
        if self.root.find(id).is_some_and(|n| !n.kids().is_empty()) {
            bail!("Can't remove non-empty folder (use recursive interface).");
        }
        self.detach(id).map(drop)
    }

    fn remove_tree(&mut self, id: &str) -> Result<()> {
        self.detach(id).map(drop)
    }
}

// ── roots and paths ─────────────────────────────────────────────────────────

pub const BAR: &str = "bookmarks-bar";

/// Map a user-typed first path segment to a folderType.
pub fn root_alias(segment: &str) -> Option<&'static str> {
    match segment.trim().to_lowercase().as_str() {
        "bookmarks bar" | "bookmarks-bar" | "bar" => Some(BAR),
        "other bookmarks" | "other" => Some("other"),
        "mobile bookmarks" | "mobile" => Some("mobile"),
        _ => None,
    }
}

/// The permanent folder of `folder_type`: the account (synced) one, or with `local` the local one.
///
/// # Errors
/// When Chrome doesn't report one (not signed in, sync off for bookmarks, or Chrome < 134).
pub fn permanent<'a>(root: &'a Node, folder_type: &str, local: bool) -> Result<&'a Node> {
    let candidates: Vec<&Node> = root.kids().iter().filter(|n| n.folder_type.as_deref() == Some(folder_type)).collect();
    if candidates.is_empty() {
        bail!("Chrome reported no '{folder_type}' folder (needs Chrome 134+ and the extension's bookmarks permission)");
    }
    candidates.into_iter().find(|n| n.syncing != local).ok_or_else(|| {
        if local {
            anyhow!("there is no local-only '{folder_type}' folder; everything there is synced, so drop --local")
        } else {
            anyhow!("no synced '{folder_type}' folder: sign Chrome in with bookmark sync on, or pass --local to use the local one")
        }
    })
}

/// Split a path on `/`, honouring `\/` for titles that contain a slash.
pub fn split_path(path: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'/') => {
                out.last_mut().unwrap_or(&mut String::new()).push('/');
                chars.next();
            }
            '/' => out.push(String::new()),
            _ => {
                if let Some(s) = out.last_mut() {
                    s.push(c);
                }
            }
        }
    }
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// The root folder a path starts in and the segments under it. A path that doesn't
/// start with a root name ("Bookmarks bar", "Other bookmarks", "Mobile bookmarks") is under the bar.
pub fn path_start<'a>(root: &'a Node, path: &str, local: bool) -> Result<(&'a Node, Vec<String>)> {
    let mut segs = split_path(path);
    let ft = segs.first().and_then(|s| root_alias(s)).map_or(BAR, |ft| ft);
    if ft != BAR || segs.first().is_some_and(|s| root_alias(s).is_some()) {
        segs.remove(0);
    }
    Ok((permanent(root, ft, local)?, segs))
}

/// Resolve a path to a node. Intermediate segments match folders; the last matches anything (folders first).
pub fn resolve<'a>(root: &'a Node, path: &str, local: bool) -> Result<Option<&'a Node>> {
    let (mut cur, segs) = path_start(root, path, local)?;
    for (i, seg) in segs.iter().enumerate() {
        let last = i + 1 == segs.len();
        let kids = cur.kids();
        let hit = kids
            .iter()
            .find(|c| c.is_folder() && &c.title == seg)
            .or_else(|| if last { kids.iter().find(|c| &c.title == seg) } else { None });
        match hit {
            Some(n) => cur = n,
            None => return Ok(None),
        }
    }
    Ok(Some(cur))
}

/// A bookmark id, or a path.
///
/// # Errors
/// When nothing matches.
pub fn resolve_target<'a>(root: &'a Node, target: &str, local: bool) -> Result<&'a Node> {
    if target.chars().all(|c| c.is_ascii_digit()) {
        if let Some(n) = root.find(target) {
            return Ok(n);
        }
    }
    resolve(root, target, local)?.ok_or_else(|| anyhow!("no bookmark or folder at '{target}'"))
}

/// Refuse writes to admin-managed nodes, permanent folders and the root.
///
/// # Errors
/// When `id` is protected.
pub fn guard_writable(root: &Node, id: &str, what: &str) -> Result<()> {
    let chain = root.ancestry(id).ok_or_else(|| anyhow!("no bookmark with id {id}"))?;
    if chain.iter().any(|n| n.unmodifiable.is_some() || n.folder_type.as_deref() == Some("managed")) {
        bail!(
            "{what}: '{}' is managed by your administrator and read-only",
            chain.last().map_or("", |n| n.title.as_str())
        );
    }
    let n = chain.last().context("empty chain")?;
    if n.id == root.id || n.folder_type.is_some() {
        bail!("{what}: '{}' is a permanent Chrome folder", n.title);
    }
    Ok(())
}

/// Refuse to put things inside a managed folder or into the root.
///
/// # Errors
/// When `parent_id` can't take children.
pub fn guard_parent(root: &Node, parent_id: &str) -> Result<()> {
    let chain = root.ancestry(parent_id).ok_or_else(|| anyhow!("no folder with id {parent_id}"))?;
    if chain.iter().any(|n| n.unmodifiable.is_some() || n.folder_type.as_deref() == Some("managed")) {
        bail!("can't add to a folder managed by your administrator");
    }
    if parent_id == root.id {
        bail!("can't add to Chrome's root; use a folder like 'Bookmarks bar'");
    }
    if chain.last().is_some_and(|n| !n.is_folder()) {
        bail!("'{}' is a bookmark, not a folder", chain.last().map_or("", |n| n.title.as_str()));
    }
    Ok(())
}

// ── idempotent writes ───────────────────────────────────────────────────────

pub struct Ensured {
    pub id: String,
    pub created: Vec<String>,
    pub syncing: bool,
}

/// Create every missing folder along `path`.
///
/// # Errors
/// API failure, a bookmark where a folder is needed, or a managed parent.
pub fn ensure_folder(api: &mut dyn Api, path: &str, local: bool) -> Result<Ensured> {
    let root = api.tree()?;
    let (start, segs) = path_start(&root, path, local)?;
    let syncing = start.syncing;
    let mut id = start.id.clone();
    let mut created = Vec::new();
    for seg in segs {
        let kids = api.children(&id)?;
        id = if let Some(f) = kids.iter().find(|c| c.is_folder() && c.title == seg) {
            f.id.clone()
        } else {
            guard_parent(&api.tree()?, &id)?;
            created.push(seg.clone());
            api.create(&id, &seg, None, None)?.id
        };
    }
    Ok(Ensured { id, created, syncing })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Upserted {
    Created(String),
    Retitled(String),
    Unchanged(String),
}

fn same_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// Put a bookmark in the folder at `path` unless one with that URL is already there.
///
/// # Errors
/// API failure or a protected folder.
pub fn upsert(api: &mut dyn Api, path: &str, title: &str, url: &str, local: bool) -> Result<(Upserted, Ensured)> {
    let folder = ensure_folder(api, path, local)?;
    let existing = api
        .children(&folder.id)?
        .into_iter()
        .find(|c| c.url.as_deref().is_some_and(|u| same_url(u, url)));
    let out = match existing {
        Some(b) if b.title == title => Upserted::Unchanged(b.id),
        Some(b) => {
            guard_writable(&api.tree()?, &b.id, "rename")?;
            Upserted::Retitled(api.update(&b.id, Some(title), None)?.id)
        }
        None => Upserted::Created(api.create(&folder.id, title, Some(url), None)?.id),
    };
    Ok((out, folder))
}

// ── restore ─────────────────────────────────────────────────────────────────

fn same_item(a: &Node, b: &Node) -> bool {
    a.title == b.title && a.is_folder() == b.is_folder() && a.url.as_deref().zip(b.url.as_deref()).is_none_or(|(x, y)| same_url(x, y))
}

/// Make the live tree match `snapshot`: every non-managed permanent folder is reconciled
/// with its live counterpart (same folderType and syncing). Returns what it did.
///
/// # Errors
/// API failure.
pub fn restore(api: &mut dyn Api, snapshot: &Node) -> Result<Vec<String>> {
    let live = api.tree()?;
    let mut ops = Vec::new();
    for snap_root in snapshot.kids() {
        let Some(ft) = snap_root.folder_type.as_deref() else { continue };
        if ft == "managed" {
            continue;
        }
        match live
            .kids()
            .iter()
            .find(|n| n.folder_type.as_deref() == Some(ft) && n.syncing == snap_root.syncing)
        {
            Some(target) => reconcile(api, snap_root, &target.id, &label(snap_root), &mut ops)?,
            None => ops.push(format!("skip {}: not present in this Chrome", label(snap_root))),
        }
    }
    Ok(ops)
}

fn label(n: &Node) -> String {
    format!("{}{}", n.title, if n.syncing { "" } else { " (local)" })
}

fn reconcile(api: &mut dyn Api, snap: &Node, live_id: &str, path: &str, ops: &mut Vec<String>) -> Result<()> {
    let live = api.children(live_id)?;
    let mut used = HashSet::new();
    let mut want = Vec::new();
    for s in snap.kids() {
        let id = if let Some(l) = live.iter().find(|l| !used.contains(&l.id) && same_item(s, l)) {
            l.id.clone()
        } else {
            ops.push(format!(
                "create {path}/{}{}",
                s.title,
                s.url.as_deref().map(|u| format!(" → {u}")).unwrap_or_default()
            ));
            api.create(live_id, &s.title, s.url.as_deref(), None)?.id
        };
        used.insert(id.clone());
        want.push(id);
    }
    for l in live.iter().filter(|l| !used.contains(&l.id)) {
        if l.unmodifiable.is_some() {
            continue;
        }
        ops.push(format!("remove {path}/{}{}", l.title, if l.is_folder() { "/" } else { "" }));
        if l.is_folder() {
            api.remove_tree(&l.id)?;
        } else {
            api.remove(&l.id)?;
        }
    }
    let mut cur: Vec<String> = api.children(live_id)?.into_iter().map(|c| c.id).collect();
    let mut reordered = false;
    for (i, id) in want.iter().enumerate() {
        if cur.get(i) != Some(id) {
            api.move_to(id, live_id, Some(i))?;
            cur.retain(|c| c != id);
            cur.insert(i.min(cur.len()), id.clone());
            reordered = true;
        }
    }
    if reordered {
        ops.push(format!("reorder {path}"));
    }
    for (s, id) in snap.kids().iter().zip(&want) {
        if s.is_folder() {
            reconcile(api, s, id, &format!("{path}/{}", s.title), ops)?;
        }
    }
    Ok(())
}

// ── rendering ───────────────────────────────────────────────────────────────

/// Indented text tree, `depth` levels below `node` (None = all).
pub fn render(node: &Node, depth: Option<usize>) -> String {
    let mut out = String::new();
    render_into(node, 0, depth, &mut out);
    out
}

fn render_into(n: &Node, level: usize, depth: Option<usize>, out: &mut String) {
    let pad = "  ".repeat(level);
    let tag = match (&n.folder_type, n.syncing) {
        (Some(ft), true) => format!("  [{ft}, synced]"),
        (Some(ft), false) => format!("  [{ft}, local]"),
        _ => String::new(),
    };
    if n.id == "0" || (n.parent_id.is_none() && n.title.is_empty()) {
        let _ = writeln!(out, "{pad}(root)");
    } else if let Some(u) = &n.url {
        let _ = writeln!(out, "{pad}{}  <{u}>  #{}", n.title, n.id);
    } else {
        let _ = writeln!(out, "{pad}{}/{tag}  #{}", n.title, n.id);
    }
    if depth.is_some_and(|d| level >= d) {
        if !n.kids().is_empty() {
            let _ = writeln!(out, "{pad}  … {} more", n.kids().len());
        }
        return;
    }
    for c in n.kids() {
        render_into(c, level + 1, depth, out);
    }
}

/// Netscape bookmark HTML, the format every browser imports.
pub fn to_netscape_html(root: &Node) -> String {
    let mut out = String::from(
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n<TITLE>Bookmarks</TITLE>\n<H1>Bookmarks</H1>\n<DL><p>\n",
    );
    for c in root.kids() {
        html_into(c, 1, &mut out);
    }
    out.push_str("</DL><p>\n");
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn html_into(n: &Node, level: usize, out: &mut String) {
    let pad = "    ".repeat(level);
    #[allow(clippy::cast_possible_truncation)]
    let added = n.date_added.map(|d| format!(" ADD_DATE=\"{}\"", (d / 1000.0) as i64)).unwrap_or_default();
    if let Some(u) = &n.url {
        let _ = writeln!(out, "{pad}<DT><A HREF=\"{}\"{added}>{}</A>", esc(u), esc(&n.title));
    } else {
        let bar = if n.folder_type.as_deref() == Some(BAR) {
            " PERSONAL_TOOLBAR_FOLDER=\"true\""
        } else {
            ""
        };
        let _ = writeln!(out, "{pad}<DT><H3{added}{bar}>{}</H3>\n{pad}<DL><p>", esc(&n.title));
        for c in n.kids() {
            html_into(c, level + 1, out);
        }
        let _ = writeln!(out, "{pad}</DL><p>");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::redundant_pub_crate, reason = "test idiom; shared fixtures")]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// A Chrome 134+ tree with both local and account permanent folders, plus a managed one.
    pub fn sample() -> MemTree {
        let root: Node = serde_json::from_value(json!({
            "id": "0", "title": "", "children": [
                {"id": "1", "parentId": "0", "index": 0, "title": "Bookmarks bar", "folderType": "bookmarks-bar", "syncing": false, "children": [
                    {"id": "10", "parentId": "1", "index": 0, "title": "Local thing", "url": "https://local.example"}
                ]},
                {"id": "2", "parentId": "0", "index": 1, "title": "Other bookmarks", "folderType": "other", "syncing": false, "children": []},
                {"id": "3", "parentId": "0", "index": 2, "title": "Bookmarks bar", "folderType": "bookmarks-bar", "syncing": true, "children": [
                    {"id": "30", "parentId": "3", "index": 0, "title": "Work", "syncing": true, "children": [
                        {"id": "31", "parentId": "30", "index": 0, "title": "Jira", "url": "https://jira.example", "syncing": true}
                    ]}
                ]},
                {"id": "4", "parentId": "0", "index": 3, "title": "Other bookmarks", "folderType": "other", "syncing": true, "children": []},
                {"id": "5", "parentId": "0", "index": 4, "title": "Managed", "folderType": "managed", "unmodifiable": "managed", "children": [
                    {"id": "50", "parentId": "5", "index": 0, "title": "Policy link", "url": "https://corp.example", "unmodifiable": "managed"}
                ]}
            ]
        }))
        .unwrap();
        MemTree::new(root)
    }

    #[test]
    fn picks_synced_bar_by_folder_type_not_id_or_title() {
        let t = sample();
        assert_eq!(permanent(&t.root, BAR, false).unwrap().id, "3");
        assert_eq!(permanent(&t.root, BAR, true).unwrap().id, "1");
        assert!(permanent(&t.root, "mobile", false).unwrap_err().to_string().contains("no 'mobile'"));
    }

    #[test]
    fn legacy_single_synced_bar_rejects_local() {
        let root: Node = serde_json::from_value(
            json!({"id":"0","children":[{"id":"1","parentId":"0","title":"Bookmarks bar","folderType":"bookmarks-bar","syncing":true,"children":[]}]}),
        )
        .unwrap();
        assert_eq!(permanent(&root, BAR, false).unwrap().id, "1");
        assert!(permanent(&root, BAR, true).unwrap_err().to_string().contains("drop --local"));
        let unsynced: Node = serde_json::from_value(
            json!({"id":"0","children":[{"id":"1","parentId":"0","title":"Bookmarks bar","folderType":"bookmarks-bar","syncing":false,"children":[]}]}),
        )
        .unwrap();
        assert!(permanent(&unsynced, BAR, false).unwrap_err().to_string().contains("--local"));
    }

    #[test]
    fn splits_paths_with_escapes() {
        assert_eq!(split_path("Bookmarks bar/ A \\/ B /C/"), vec!["Bookmarks bar", "A / B", "C"]);
        assert!(split_path("").is_empty());
    }

    #[test]
    fn resolves_paths_and_ids() {
        let t = sample();
        assert_eq!(resolve(&t.root, "Bookmarks bar/Work/Jira", false).unwrap().unwrap().id, "31");
        assert_eq!(resolve(&t.root, "Work", false).unwrap().unwrap().id, "30");
        assert_eq!(resolve(&t.root, "bar", true).unwrap().unwrap().id, "1");
        assert_eq!(resolve(&t.root, "Bookmarks bar/Local thing", true).unwrap().unwrap().id, "10");
        assert!(resolve(&t.root, "Bookmarks bar/Local thing", false).unwrap().is_none());
        assert_eq!(resolve_target(&t.root, "31", false).unwrap().title, "Jira");
        assert!(resolve_target(&t.root, "Nope", false).is_err());
    }

    #[test]
    fn guards_managed_permanent_and_root() {
        let t = sample();
        assert!(guard_writable(&t.root, "50", "remove").unwrap_err().to_string().contains("administrator"));
        assert!(guard_writable(&t.root, "3", "remove").unwrap_err().to_string().contains("permanent"));
        assert!(guard_writable(&t.root, "0", "remove").is_err());
        guard_writable(&t.root, "31", "remove").unwrap();
        assert!(guard_parent(&t.root, "5").is_err());
        assert!(guard_parent(&t.root, "0").is_err());
        assert!(guard_parent(&t.root, "31").unwrap_err().to_string().contains("not a folder"));
        guard_parent(&t.root, "30").unwrap();
    }

    #[test]
    fn upsert_creates_on_synced_bar_then_is_idempotent() {
        let mut t = sample();
        let url = "https://website.test.topstepbrokerage.com";
        let (first, folder) = upsert(&mut t, "Bookmarks bar/Brokerage Test", "Brokerage Test - Trader", url, false).unwrap();
        assert!(matches!(first, Upserted::Created(_)));
        assert_eq!(folder.created, vec!["Brokerage Test"]);
        assert!(folder.syncing);
        let f = resolve(&t.root, "Bookmarks bar/Brokerage Test", false).unwrap().unwrap();
        assert_eq!(f.parent_id.as_deref(), Some("3"));
        assert_eq!(f.kids().len(), 1);

        let (again, folder) = upsert(&mut t, "Bookmarks bar/Brokerage Test", "Brokerage Test - Trader", &format!("{url}/"), false).unwrap();
        assert!(matches!(again, Upserted::Unchanged(_)));
        assert!(folder.created.is_empty());
        let (retitled, _) = upsert(&mut t, "Bookmarks bar/Brokerage Test", "Trader", url, false).unwrap();
        assert!(matches!(retitled, Upserted::Retitled(_)));
        assert_eq!(resolve(&t.root, "Bookmarks bar/Brokerage Test", false).unwrap().unwrap().kids().len(), 1);
    }

    #[test]
    fn upsert_local_goes_to_local_bar() {
        let mut t = sample();
        let (_, folder) = upsert(&mut t, "X", "x", "https://x.example", true).unwrap();
        assert!(!folder.syncing);
        assert_eq!(resolve(&t.root, "X", true).unwrap().unwrap().parent_id.as_deref(), Some("1"));
    }

    #[test]
    fn ensure_folder_refuses_to_descend_through_a_bookmark() {
        let mut t = sample();
        // "Jira" is a bookmark, so a folder named Jira gets created next to it rather than inside it.
        let e = ensure_folder(&mut t, "Work/Jira/Sub", false).unwrap();
        assert_eq!(e.created, vec!["Jira", "Sub"]);
    }

    #[test]
    fn mem_tree_matches_chrome_move_and_remove_semantics() {
        let mut t = sample();
        let a = t.create("4", "a", Some("https://a"), None).unwrap().id;
        let b = t.create("4", "b", Some("https://b"), None).unwrap().id;
        let c = t.create("4", "c", Some("https://c"), None).unwrap().id;
        t.move_to(&a, "4", Some(3)).unwrap();
        let order: Vec<String> = t.children("4").unwrap().into_iter().map(|n| n.id).collect();
        assert_eq!(order, vec![b, c, a]);
        assert!(t.remove("30").is_err());
        t.remove_tree("30").unwrap();
        assert!(t.root.find("31").is_none());
    }

    #[test]
    fn restore_round_trips_and_leaves_managed_alone() {
        let mut t = sample();
        let before = t.root.clone();
        upsert(&mut t, "Bookmarks bar/Brokerage Test", "Trader", "https://trader.example", false).unwrap();
        t.remove_tree("30").unwrap();
        t.create("4", "z", Some("https://z"), Some(0)).unwrap();

        let mut dry = t.clone();
        let ops = restore(&mut dry, &before).unwrap();
        assert!(ops.iter().any(|o| o.starts_with("create Bookmarks bar/Work")));
        assert!(ops.iter().any(|o| o.starts_with("remove Bookmarks bar/Brokerage Test/")));
        assert!(ops.iter().any(|o| o.starts_with("remove Other bookmarks/z")));
        assert!(!ops.iter().any(|o| o.contains("Managed")));

        restore(&mut t, &before).unwrap();
        let shape = |n: &Node| {
            render(n, None)
                .lines()
                .map(|l| l.split("  #").next().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(&t.root), shape(&before));
        assert!(restore(&mut t, &before).unwrap().is_empty(), "a second restore is a no-op");
    }

    #[test]
    fn restore_fixes_order() {
        let mut t = sample();
        for (i, name) in ["a", "b", "c"].iter().enumerate() {
            t.create("4", name, Some(&format!("https://{name}")), Some(i)).unwrap();
        }
        let before = t.root.clone();
        let last = t.children("4").unwrap()[2].id.clone();
        t.move_to(&last, "4", Some(0)).unwrap();
        let ops = restore(&mut t, &before).unwrap();
        assert_eq!(ops, vec!["reorder Other bookmarks"]);
        let titles: Vec<String> = t.children("4").unwrap().into_iter().map(|n| n.title).collect();
        assert_eq!(titles, vec!["a", "b", "c"]);
    }

    #[test]
    fn renders_text_and_html() {
        let t = sample();
        let text = render(&t.root, Some(1));
        assert!(text.contains("Bookmarks bar/  [bookmarks-bar, synced]  #3"));
        assert!(text.contains("Bookmarks bar/  [bookmarks-bar, local]  #1"));
        assert!(text.contains("… 1 more"));
        let html = to_netscape_html(&t.root);
        assert!(html.contains("<DT><A HREF=\"https://jira.example\">Jira</A>"));
        assert!(html.contains("PERSONAL_TOOLBAR_FOLDER=\"true\""));
        assert_eq!(esc("<a & \"b\">"), "&lt;a &amp; &quot;b&quot;&gt;");
    }
}
