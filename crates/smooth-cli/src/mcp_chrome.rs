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
pub struct TextArgs {
    pub tab: i64,
    /// Only the lines containing this (case-insensitive), with a line of context. Much cheaper than the whole page.
    #[serde(default)]
    pub find: Option<String>,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NavArgs {
    pub tab: i64,
    /// back, forward or reload.
    pub action: String,
    #[serde(default)]
    pub instance: Option<String>,
}

const fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClickArgs {
    pub tab: i64,
    /// Element ref from the latest browse_snapshot (e.g. "e12", or "f7:e3" inside an iframe).
    pub r#ref: String,
    /// Return a fresh snapshot with the result (default true).
    #[serde(default = "yes")]
    pub snapshot: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TypeArgs {
    pub tab: i64,
    pub r#ref: String,
    /// Replaces the field's current text.
    pub text: String,
    /// Submit the form (or press Enter) afterwards.
    #[serde(default)]
    pub submit: bool,
    #[serde(default = "yes")]
    pub snapshot: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SelectArgs {
    pub tab: i64,
    /// Ref of a <select>.
    pub r#ref: String,
    /// The option's visible text or value.
    pub option: String,
    #[serde(default = "yes")]
    pub snapshot: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct KeyArgs {
    pub tab: i64,
    /// Enter, Escape, Tab, ArrowDown, Backspace, a letter, or a combo like Control+a.
    pub key: String,
    /// Element to send it to; default is whatever has focus.
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default = "yes")]
    pub snapshot: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScrollArgs {
    pub tab: i64,
    /// up or down (default down when no text/ref is given).
    #[serde(default)]
    pub direction: Option<String>,
    /// How many viewports to scroll (default 1).
    #[serde(default)]
    pub pages: Option<f64>,
    /// Scroll this element into view, or, with direction, scroll inside it (a scrollable list or panel).
    #[serde(default)]
    pub r#ref: Option<String>,
    /// Scroll to the first visible occurrence of this text.
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default = "yes")]
    pub snapshot: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FillField {
    pub r#ref: String,
    /// Text for inputs, option text for selects, true/false for checkboxes and radios.
    pub value: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FillArgs {
    pub tab: i64,
    pub fields: Vec<FillField>,
    #[serde(default = "yes")]
    pub snapshot: bool,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitArgs {
    pub tab: i64,
    /// Resolves as soon as any of these appears in the page text.
    pub text: Vec<String>,
    /// Default 10000.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
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

    #[tool(name = "browse_nav", description = "Go back, forward or reload a tab and wait for it to load.")]
    pub async fn browse_nav(&self, params: Parameters<NavArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::nav(a.instance.as_deref(), a.tab, &a.action)).await
    }

    #[tool(
        name = "browse_text",
        description = "A tab's title, URL and visible text. Pass find to get only matching lines (cheap way to check for a word or read one part of a long page).",
        annotations(read_only_hint = true)
    )]
    pub async fn browse_text(&self, params: Parameters<TextArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::page_text(a.instance.as_deref(), a.tab, a.find.as_deref())).await
    }

    #[tool(
        name = "browse_snapshot",
        description = "Outline of a tab: its interactive elements with refs ([e12], or [f7:e3] inside an iframe), nested under headings, forms, dialogs and rows, with state (value, checked, disabled, expanded, covered, offscreen) and how much page is above/below. Prefer this over a screenshot. Refs come from the latest snapshot; action tools already return a fresh one.",
        annotations(read_only_hint = true)
    )]
    pub async fn browse_snapshot(&self, params: Parameters<TabArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::page_snapshot(a.instance.as_deref(), a.tab)).await
    }

    #[tool(
        name = "browse_click",
        description = "Click an element by ref. Waits for any navigation or for the page to settle, says if the URL changed or a tab opened, and returns a fresh snapshot."
    )]
    pub async fn browse_click(&self, params: Parameters<ClickArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::act(a.instance.as_deref(), a.tab, "click", json!({"ref": a.r#ref}), a.snapshot)).await
    }

    #[tool(
        name = "browse_type",
        description = "Replace the text of an input, textarea or rich editor by ref; submit=true submits the form. For several fields use browse_fill; for a <select> use browse_select."
    )]
    pub async fn browse_type(&self, params: Parameters<TypeArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || {
            chrome::act(
                a.instance.as_deref(),
                a.tab,
                "type",
                json!({"ref": a.r#ref, "text": a.text, "submit": a.submit}),
                a.snapshot,
            )
        })
        .await
    }

    #[tool(
        name = "browse_fill",
        description = "Fill a form in one call: text inputs, selects (by option text) and checkboxes/radios (true/false). Prefer this over several browse_type calls."
    )]
    pub async fn browse_fill(&self, params: Parameters<FillArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        let fields: Vec<_> = a.fields.iter().map(|f| json!({"ref": f.r#ref, "value": f.value})).collect();
        blocking(move || chrome::act(a.instance.as_deref(), a.tab, "fill", json!({ "fields": fields }), a.snapshot)).await
    }

    #[tool(
        name = "browse_select",
        description = "Choose an option in a <select> by its text or value. Custom dropdowns aren't <select>s: click them open, then click the option."
    )]
    pub async fn browse_select(&self, params: Parameters<SelectArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::act(a.instance.as_deref(), a.tab, "select", json!({"ref": a.r#ref, "option": a.option}), a.snapshot)).await
    }

    #[tool(
        name = "browse_key",
        description = "Press a key (Enter, Escape, Tab, ArrowDown, Control+a, …) on an element or the focused one. Use Escape to close menus and dialogs."
    )]
    pub async fn browse_key(&self, params: Parameters<KeyArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::act(a.instance.as_deref(), a.tab, "key", json!({"ref": a.r#ref, "key": a.key}), a.snapshot)).await
    }

    #[tool(name = "browse_hover", description = "Hover an element by ref, e.g. to open a hover menu or tooltip.")]
    pub async fn browse_hover(&self, params: Parameters<ClickArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::act(a.instance.as_deref(), a.tab, "hover", json!({"ref": a.r#ref}), a.snapshot)).await
    }

    #[tool(
        name = "browse_scroll",
        description = "Scroll the page by viewports (direction, pages), to some text, or an element into view; with ref and direction, scrolls inside that panel. Use it when the snapshot says there's more below.",
        annotations(read_only_hint = true)
    )]
    pub async fn browse_scroll(&self, params: Parameters<ScrollArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || {
            chrome::act(
                a.instance.as_deref(),
                a.tab,
                "scroll",
                json!({"direction": a.direction, "pages": a.pages, "ref": a.r#ref, "text": a.text}),
                a.snapshot,
            )
        })
        .await
    }

    #[tool(
        name = "browse_wait_for",
        description = "Wait until any of the given texts appears on the page (after a slow search, save or login).",
        annotations(read_only_hint = true)
    )]
    pub async fn browse_wait_for(&self, params: Parameters<WaitArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        blocking(move || chrome::wait_for(a.instance.as_deref(), a.tab, &a.text, a.timeout_ms.unwrap_or(10_000))).await
    }

    #[tool(
        name = "browse_screenshot",
        description = "Screenshot a tab to a PNG file and return its path (read the file to see it). Brings the tab to the front, so prefer browse_snapshot unless you need to see the layout."
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
