//! Chrome bookmarks + browsing as MCP tools (SMOODEV-3887), thin over `crate::chrome`.
//! Destructive ops dry-run unless `confirm: true`; reads carry `read_only_hint`.

use rmcp::{handler::server::wrapper::Parameters, model::ErrorData, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::chrome;
use crate::mcp_serve::SmoothMcp;

async fn blocking(f: impl FnOnce() -> anyhow::Result<String> + Send + 'static) -> Result<String, ErrorData> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
        .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TreeArgs {
    /// Folder path ("Bookmarks bar/Work") or id. Omit for everything.
    #[serde(default)]
    pub path: Option<String>,
    /// Levels to show below the start (default all).
    #[serde(default)]
    pub depth: Option<usize>,
    /// Resolve `path` in the local (not synced) folders.
    #[serde(default)]
    pub local: bool,
    /// Connected Chrome instance (pid) when several profiles are connected.
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// Words matched against titles and URLs.
    pub query: String,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpsertArgs {
    /// Folder path, created if missing. Paths not starting with "Bookmarks bar"/"Other bookmarks"/"Mobile bookmarks" are under the bookmarks bar.
    pub path: String,
    pub title: String,
    pub url: String,
    /// Use the local (unsynced) folders instead of the account-synced ones.
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MkdirArgs {
    pub path: String,
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddArgs {
    pub title: String,
    pub url: String,
    /// Parent folder path or id (default: the synced bookmarks bar).
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub index: Option<usize>,
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MoveArgs {
    /// Bookmark/folder id or path.
    pub target: String,
    /// Destination folder path or id.
    pub to: String,
    #[serde(default)]
    pub index: Option<usize>,
    #[serde(default)]
    pub local: bool,
    /// Moving a folder needs true; otherwise you get a dry run.
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RenameArgs {
    /// Bookmark/folder id or path.
    pub target: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveArgs {
    /// Bookmark/folder id or path.
    pub target: String,
    #[serde(default)]
    pub local: bool,
    /// Must be true to actually remove; otherwise you get a dry run.
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HistoryArgs {
    /// How many snapshots (default 20).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SnapshotArgs {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RestoreArgs {
    /// A sha from bookmarks_history, HEAD~1, origin/<machine>, …
    pub rev: String,
    /// Must be true to apply; otherwise you get the plan.
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstanceArgs {
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenArgs {
    pub url: String,
    /// Navigate this tab instead of opening a new one.
    #[serde(default)]
    pub tab: Option<i64>,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TabArgs {
    /// Tab id from browse_tabs / browse_open.
    pub tab: i64,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClickArgs {
    pub tab: i64,
    /// Element ref from browse_snapshot (e.g. "e12").
    pub r#ref: String,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TypeArgs {
    pub tab: i64,
    pub r#ref: String,
    pub text: String,
    /// Submit the form (or press Enter) afterwards.
    #[serde(default)]
    pub submit: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[tool_router(router = chrome_tool_router, vis = "pub(crate)")]
impl SmoothMcp {
    #[tool(
        name = "bookmarks_tree",
        description = "Show Chrome bookmarks as a tree. Permanent folders are labelled synced (saved to the Google account) or local. Needs the Smooth Chrome extension (`th chrome install`).",
        annotations(read_only_hint = true)
    )]
    pub async fn bookmarks_tree(&self, params: Parameters<TreeArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::tree_view(a.instance.as_deref(), a.path.as_deref(), a.depth, false, a.local)).await
    }

    #[tool(
        name = "bookmarks_search",
        description = "Search Chrome bookmarks by title/URL; each hit shows its id and full path.",
        annotations(read_only_hint = true)
    )]
    pub async fn bookmarks_search(&self, params: Parameters<SearchArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::search(a.instance.as_deref(), &a.query, false)).await
    }

    #[tool(
        name = "bookmarks_upsert",
        description = "Idempotently put a bookmark in a folder path on the account-synced bookmarks bar (folders created as needed). No duplicate if the URL is already there."
    )]
    pub async fn bookmarks_upsert(&self, params: Parameters<UpsertArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::upsert(a.instance.as_deref(), &a.path, &a.title, &a.url, a.local)).await
    }

    #[tool(name = "bookmarks_mkdir", description = "Idempotently create a bookmark folder path (synced by default).")]
    pub async fn bookmarks_mkdir(&self, params: Parameters<MkdirArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::mkdir(a.instance.as_deref(), &a.path, a.local)).await
    }

    #[tool(
        name = "bookmarks_add",
        description = "Add a bookmark (always creates; prefer bookmarks_upsert). Default parent: the synced bookmarks bar."
    )]
    pub async fn bookmarks_add(&self, params: Parameters<AddArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::add(a.instance.as_deref(), a.parent.as_deref(), &a.title, &a.url, a.index, a.local)).await
    }

    #[tool(
        name = "bookmarks_move",
        description = "Move a bookmark or folder into another folder. Moving a folder needs confirm=true (otherwise a dry run)."
    )]
    pub async fn bookmarks_move(&self, params: Parameters<MoveArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::move_node(a.instance.as_deref(), &a.target, &a.to, a.index, a.local, a.confirm)).await
    }

    #[tool(name = "bookmarks_rename", description = "Change a bookmark's title and/or URL, or a folder's title.")]
    pub async fn bookmarks_rename(&self, params: Parameters<RenameArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::rename(a.instance.as_deref(), &a.target, a.title.as_deref(), a.url.as_deref(), a.local)).await
    }

    #[tool(
        name = "bookmarks_remove",
        description = "Remove a bookmark or folder (with contents). Dry run unless confirm=true; confirm only after the user agreed. A history snapshot is taken first."
    )]
    pub async fn bookmarks_remove(&self, params: Parameters<RemoveArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::remove(a.instance.as_deref(), &a.target, a.local, a.confirm)).await
    }

    #[tool(
        name = "bookmarks_history",
        description = "Recent git-backed bookmark snapshots (sha, time, what changed).",
        annotations(read_only_hint = true)
    )]
    pub async fn bookmarks_history(&self, params: Parameters<HistoryArgs>) -> Result<String, ErrorData> {
        let limit = params.0.limit.unwrap_or(20);
        blocking(move || chrome::history_log(limit)).await
    }

    #[tool(
        name = "bookmarks_snapshot",
        description = "Commit the current bookmarks to the history repo (do this before bulk changes)."
    )]
    pub async fn bookmarks_snapshot(&self, params: Parameters<SnapshotArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::snapshot_now(a.instance.as_deref(), a.message.as_deref().unwrap_or("snapshot via MCP"))).await
    }

    #[tool(
        name = "bookmarks_restore",
        description = "Make bookmarks match a history snapshot. Returns the plan unless confirm=true; confirm only after the user agreed."
    )]
    pub async fn bookmarks_restore(&self, params: Parameters<RestoreArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::restore(a.instance.as_deref(), &a.rev, a.confirm)).await
    }

    #[tool(
        name = "browse_tabs",
        description = "List open Chrome tabs (id, title, URL). Browsing must be enabled in the Smooth extension popup.",
        annotations(read_only_hint = true)
    )]
    pub async fn browse_tabs(&self, params: Parameters<InstanceArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::tabs(a.instance.as_deref())).await
    }

    #[tool(
        name = "browse_open",
        description = "Open a URL in a new tab (or navigate `tab`) and wait for it to load. Returns the tab id."
    )]
    pub async fn browse_open(&self, params: Parameters<OpenArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || {
            let v = match a.tab {
                Some(tab) => chrome::browse(a.instance.as_deref(), "tabs.navigate", json!({"tabId": tab, "url": a.url}))?,
                None => chrome::browse(a.instance.as_deref(), "tabs.open", json!({"url": a.url}))?,
            };
            Ok(serde_json::to_string_pretty(&v)?)
        })
        .await
    }

    #[tool(name = "browse_text", description = "A tab's title, URL and visible text.", annotations(read_only_hint = true))]
    pub async fn browse_text(&self, params: Parameters<TabArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::page_text(a.instance.as_deref(), a.tab)).await
    }

    #[tool(
        name = "browse_snapshot",
        description = "List a tab's visible interactive elements with refs (e1, e2, …) to pass to browse_click / browse_type.",
        annotations(read_only_hint = true)
    )]
    pub async fn browse_snapshot(&self, params: Parameters<TabArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::page_snapshot(a.instance.as_deref(), a.tab)).await
    }

    #[tool(name = "browse_click", description = "Click an element by ref from browse_snapshot.")]
    pub async fn browse_click(&self, params: Parameters<ClickArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::browse(a.instance.as_deref(), "page.click", json!({"tabId": a.tab, "ref": a.r#ref})).map(|_| format!("Clicked {}", a.r#ref)))
            .await
    }

    #[tool(
        name = "browse_type",
        description = "Type text into an input/textarea by ref from browse_snapshot; submit=true submits the form."
    )]
    pub async fn browse_type(&self, params: Parameters<TypeArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || {
            chrome::browse(
                a.instance.as_deref(),
                "page.type",
                json!({"tabId": a.tab, "ref": a.r#ref, "text": a.text, "submit": a.submit}),
            )
            .map(|_| format!("Typed into {}", a.r#ref))
        })
        .await
    }

    #[tool(
        name = "browse_screenshot",
        description = "Screenshot a tab to a PNG file and return its path (read the file to see it)."
    )]
    pub async fn browse_screenshot(&self, params: Parameters<TabArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || {
            let png = chrome::screenshot(a.instance.as_deref(), a.tab)?;
            let path = std::env::temp_dir().join(format!("smooth-tab-{}-{}.png", a.tab, chrono::Utc::now().timestamp_millis()));
            std::fs::write(&path, png)?;
            Ok(path.display().to_string())
        })
        .await
    }

    #[tool(name = "browse_close", description = "Close a tab.")]
    pub async fn browse_close(&self, params: Parameters<TabArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::browse(a.instance.as_deref(), "tabs.close", json!({"tabId": a.tab})).map(|_| format!("Closed tab {}", a.tab))).await
    }
}
