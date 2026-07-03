//! SEP hosting for Big Smooth's OWN chat loop (pearl th-6d8606).
//!
//! Dispatched operatives host extensions (th-70cd08) and the smooth-code TUI
//! hosts them for local sessions (Phase 3) — but the personal assistant's own
//! chat agent did not. This module closes that gap: the daemon loads the user's
//! PRE-trusted extensions once at startup into a daemon-lifetime
//! [`ExtensionHost`], every chat turn attaches it (extension tools register
//! into the same `ToolRegistry` the AutoMode + Narc hooks gate), and `ui/*`
//! requests route straight onto the existing [`crate::ui_relay`] machinery —
//! no HTTP-to-self.
//!
//! **Host lifetime: daemon-lifetime, shared across chat sessions.** pi scopes
//! hosts per session, but Big Smooth constructs a fresh `Agent` per chat TURN
//! (three call sites in `server.rs`) — a per-turn host would re-spawn every
//! extension subprocess on every message. Per-turn attach + shared host means
//! a `th ext reload` between turns is picked up naturally (the next turn pulls
//! `host.tools()` fresh); an in-flight turn keeps its pre-reload tool proxies,
//! whose stale epoch the engine fences off.
//! ponytail: shared host = extension in-process state spans chat sessions;
//! move to per-session hosts when the daemon epic's durable sessions land.

use std::sync::Arc;

use async_trait::async_trait;
use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};
use smooth_operator::agent::Agent;
use smooth_operator::extension::manifest::{default_global_dir, project_dir};
use smooth_operator::extension::protocol::{HostInfo, RpcError, WorkspaceInfo};
use smooth_operator::extension::{discover, ExtensionHost, HostDelegate};
use smooth_policy::ext_trust::{hash_extension, TrustStore};

use crate::server::AppState;

/// The `task_id` the chat loop's `ui/*` requests carry on the [`ServerEvent`]
/// broadcast — frontends scope dialogs by it the same way they scope a
/// dispatched operative's.
pub const CHAT_TASK_ID: &str = "big-smooth-chat";

// ---------------------------------------------------------------------------
// DaemonUiProvider — ui/* straight onto the local relay (no HTTP-to-self)
// ---------------------------------------------------------------------------

/// [`HostDelegate`] for extensions living IN the daemon. Where the operative's
/// `HttpUiProvider` POSTs `/api/ui/request` over the callback channel, this
/// calls the same relay logic in-process ([`crate::ui_relay::relay`]): broadcast
/// a `UiRequest` to connected frontends, park interactive kinds until a human
/// answers via `/api/ui/answer`, share the timeout + auto-mode auto-confirm +
/// audit path. kv/exec/session keep the engine's headless defaults.
pub struct DaemonUiProvider {
    state: AppState,
}

#[async_trait]
impl HostDelegate for DaemonUiProvider {
    async fn ui_request(&self, ext: &str, params: Value) -> Result<Value, RpcError> {
        let kind = params.get("kind").and_then(Value::as_str).unwrap_or_default().to_string();
        Ok(crate::ui_relay::relay(&self.state, CHAT_TASK_ID, ext, &kind, params).await)
    }
}

/// The `ui/*` kinds the daemon-relayed frontends (smooth-web) render — same
/// set the operative's `HttpUiProvider` declares.
#[must_use]
pub fn ui_capabilities() -> Vec<String> {
    ["select", "confirm", "input", "notify", "set_status", "set_widget", "set_title"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Host loading — once, at daemon startup
// ---------------------------------------------------------------------------

/// Discover extensions (global + the daemon's cwd project), keep only the
/// PRE-trusted ones (the daemon is unattended — never a trust prompt; run
/// `th ext trust` first), load them into a shared [`ExtensionHost`] with the
/// [`DaemonUiProvider`], and store it on [`AppState`]. Dispatches
/// `session_start` so subscribed extensions know the assistant is up.
pub async fn init_chat_extension_host(state: &AppState) {
    let workspace_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
    let global = default_global_dir();
    let project = project_dir(&workspace_root);
    let (discovered, disc_failures) = discover(global.as_deref(), Some(project.as_path()));
    for (src, err) in &disc_failures {
        tracing::warn!(%src, %err, "sep: extension manifest failed to parse");
    }

    let trust = TrustStore::load();
    let trusted: Vec<_> = discovered
        .into_iter()
        .filter(|ext| {
            let hash = hash_extension(&ext.root).unwrap_or_default();
            let ok = trust.is_trusted(&ext.manifest.name, &hash);
            if !ok {
                tracing::info!(name = %ext.manifest.name, "sep: skipping untrusted extension in chat loop (run `th ext trust`)");
            }
            ok
        })
        .collect();
    if trusted.is_empty() {
        return;
    }

    let host_info = HostInfo {
        name: "big-smooth".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    let workspace = WorkspaceInfo {
        root: workspace_root.to_string_lossy().into_owned(),
        trusted: true,
    };
    let delegate: Arc<dyn HostDelegate> = Arc::new(DaemonUiProvider { state: state.clone() });
    let (host, load_failures) = ExtensionHost::load(trusted, host_info, workspace, "web", ui_capabilities(), delegate).await;
    for (name, err) in &load_failures {
        tracing::warn!(%name, %err, "sep: extension failed to load in chat loop");
    }
    if host.is_empty() {
        return;
    }
    tracing::info!(count = host.len(), extensions = ?host.names(), "sep: chat loop hosting extensions");
    host.dispatch_event("session_start", json!({ "host": "big-smooth" }));
    *state.ext_host.write().expect("ext_host lock") = Some(Arc::new(host));
}

/// Attach the shared chat [`ExtensionHost`] to a freshly-built chat [`Agent`].
/// Registers the host's tools into the agent's registry — where the AutoMode +
/// Narc hooks installed by `build_chat_tools` gate them like any native chat
/// tool — and wires the engine's turn/message/tool event dispatch. No-op when
/// no extensions are loaded, so the chat loop is unchanged for most users.
pub fn attach_chat_ext_host(agent: Agent, state: &AppState) -> Agent {
    let host = state.ext_host.read().expect("ext_host lock").clone();
    match host {
        Some(host) => agent.with_extension_host(host),
        None => agent,
    }
}

// ---------------------------------------------------------------------------
// Slash commands in chat
// ---------------------------------------------------------------------------

/// Parse `/cmd rest…` or `/ext:cmd rest…` into (ext, cmd, rest). `None` when
/// the input isn't a slash command shape.
fn parse_slash(input: &str) -> Option<(Option<&str>, &str, &str)> {
    let input = input.trim();
    let rest = input.strip_prefix('/')?;
    if rest.is_empty() || rest.starts_with('/') {
        return None;
    }
    let (head, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    if head.is_empty() || !head.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ':') {
        return None;
    }
    match head.split_once(':') {
        Some((ext, cmd)) if !ext.is_empty() && !cmd.is_empty() => Some((Some(ext), cmd, args.trim())),
        Some(_) => None,
        None => Some((None, head, args.trim())),
    }
}

/// If `input` is a slash command registered by a loaded extension, execute it
/// (command tier) and return its text output — the chat handlers return that
/// as the assistant reply without an LLM turn. `None` → not an extension
/// command; the input goes to the agent as usual (so `/unknown` degrades to a
/// normal chat message rather than an error).
///
/// Arguments reach the extension as `{ "args": "<raw remainder>" }` — a raw
/// line can't be split into named parameters host-side.
pub async fn try_ext_command(state: &AppState, input: &str) -> Option<String> {
    let (ext, cmd, args) = parse_slash(input)?;
    let host = state.ext_host.read().expect("ext_host lock").clone()?;
    // Only intercept commands an extension actually registered.
    if !host.commands().iter().any(|(e, c)| c.name == cmd && ext.is_none_or(|x| x == e)) {
        return None;
    }
    match host.run_command(ext, cmd, json!({ "args": args })).await {
        Ok(result) => Some(result.content.unwrap_or_else(|| format!("/{cmd}: done"))),
        Err(e) => Some(format!("/{cmd} failed: {}", e.message)),
    }
}

// ---------------------------------------------------------------------------
// Routes — reload + list (th ext reload / frontends)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct ExtReloadBody {
    pub name: String,
}

/// `POST /api/ext/reload` — hot-reload one live extension by name (`th ext
/// reload` calls this after re-trusting). The engine fences stale contexts via
/// the epoch bump; the NEXT chat turn pulls fresh tool proxies. 404 when the
/// extension isn't loaded (a newly installed extension needs a daemon restart —
/// the host discovers at startup).
pub async fn ext_reload_handler(State(state): State<AppState>, Json(body): Json<ExtReloadBody>) -> Result<Json<Value>, (StatusCode, String)> {
    state.touch();
    let host = state.ext_host.read().expect("ext_host lock").clone();
    let Some(host) = host else {
        return Err((StatusCode::NOT_FOUND, "no extensions loaded in the chat loop".into()));
    };
    if !host.names().iter().any(|n| *n == body.name) {
        return Err((
            StatusCode::NOT_FOUND,
            format!("extension `{}` is not loaded (new installs need a daemon restart)", body.name),
        ));
    }
    host.reload(&body.name).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tracing::info!(name = %body.name, "sep: hot-reloaded chat-loop extension");
    Ok(Json(json!({ "reloaded": body.name })))
}

/// `GET /api/ext` — the loaded extensions and their slash commands, for
/// frontends (autocomplete) and `th ext list --live`.
pub async fn ext_list_handler(State(state): State<AppState>) -> Json<Value> {
    state.touch();
    let host = state.ext_host.read().expect("ext_host lock").clone();
    let (names, commands): (Vec<String>, Vec<Value>) = match &host {
        Some(host) => (
            host.names().iter().map(|s| (*s).to_string()).collect(),
            host.commands()
                .into_iter()
                .map(|(ext, c)| json!({ "ext": ext, "name": c.name, "description": c.description }))
                .collect(),
        ),
        None => (Vec::new(), Vec::new()),
    };
    Json(json!({ "extensions": names, "commands": commands }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_slash_shapes() {
        assert_eq!(parse_slash("/todo add milk"), Some((None, "todo", "add milk")));
        assert_eq!(parse_slash("/todo:list"), Some((Some("todo"), "list", "")));
        assert_eq!(parse_slash("  /echo   hi there "), Some((None, "echo", "hi there")));
        assert_eq!(parse_slash("plain message"), None);
        assert_eq!(parse_slash("/"), None);
        assert_eq!(parse_slash("//not-a-command"), None);
        assert_eq!(parse_slash("/:bad"), None);
        // A path-looking token is not a command name.
        assert_eq!(parse_slash("/usr/bin/ls"), None);
    }

    #[test]
    fn ui_capabilities_match_relay_kinds() {
        // The declared capabilities must be exactly what the relay + smooth-web
        // render — an extension gates on these via `hasUI`.
        let caps = ui_capabilities();
        for k in ["select", "confirm", "input", "notify", "set_status", "set_widget", "set_title"] {
            assert!(caps.contains(&k.to_string()), "missing {k}");
        }
        assert_eq!(caps.len(), 7);
    }

    // ── DaemonUiProvider routes through the local relay ──────────────────
    // Needs a PearlStore-backed AppState; skipped where smooth-dolt is absent
    // (same pattern as ui_relay.rs tests).

    fn test_state() -> Option<AppState> {
        let tmp = tempfile::tempdir().ok()?;
        let store = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")).ok()?;
        std::mem::forget(tmp);
        Some(AppState::new(store))
    }

    #[tokio::test]
    async fn provider_one_way_kind_resolves_immediately() {
        let Some(state) = test_state() else { return };
        let provider = DaemonUiProvider { state };
        let out = provider
            .ui_request("todo", json!({ "kind": "set_widget", "widget": { "kind": "keyvalue", "text": "x" } }))
            .await
            .unwrap();
        assert_eq!(out, json!({}));
    }

    #[tokio::test]
    async fn provider_interactive_unattended_cancels() {
        let Some(state) = test_state() else { return };
        // No frontend subscribed → the relay short-circuits to cancelled
        // rather than hanging the extension.
        let provider = DaemonUiProvider { state };
        let out = provider.ui_request("todo", json!({ "kind": "confirm", "prompt": "sure?" })).await.unwrap();
        assert_eq!(out, json!({ "cancelled": true }));
    }

    #[tokio::test]
    async fn provider_broadcasts_with_chat_task_id() {
        let Some(state) = test_state() else { return };
        let mut rx = state.event_tx.subscribe();
        let provider = DaemonUiProvider { state: state.clone() };
        provider.ui_request("todo", json!({ "kind": "notify", "message": "hi" })).await.unwrap();
        match rx.recv().await.unwrap() {
            crate::events::ServerEvent::UiRequest { task_id, ext, kind, .. } => {
                assert_eq!(task_id, CHAT_TASK_ID);
                assert_eq!(ext, "todo");
                assert_eq!(kind, "notify");
            }
            other => panic!("expected UiRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn try_ext_command_without_host_is_none() {
        let Some(state) = test_state() else { return };
        assert!(try_ext_command(&state, "/todo add milk").await.is_none());
        assert!(try_ext_command(&state, "regular chat").await.is_none());
    }

    #[tokio::test]
    async fn ext_reload_without_host_is_404() {
        let Some(state) = test_state() else { return };
        let err = ext_reload_handler(State(state), Json(ExtReloadBody { name: "todo".into() })).await.unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn ext_list_without_host_is_empty() {
        let Some(state) = test_state() else { return };
        let out = ext_list_handler(State(state)).await;
        assert_eq!(out.0, json!({ "extensions": [], "commands": [] }));
    }

    // ── End-to-end wiring against a real subprocess echo peer ────────────
    //
    // A /bin/sh ndjson JSON-RPC responder: replies to initialize with a tool
    // + a command registration, echoes tool/execute + command/execute, and
    // ignores id-less notifications (events, shutdown notify). This is the
    // full chat-loop path minus the LLM: discover → load (DaemonUiProvider)
    // → dotted tool proxy → slash command → hot reload.

    const ECHO_SH: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocol_version":1,"extension":{"name":"echo","version":"0.0.1"},"registrations":{"tools":[{"name":"echo","description":"echo back","parameters":{"type":"object"}}],"commands":[{"name":"echo","description":"echo command"}]}}}\n' "$id" ;;
    *'"tool/execute"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":"echoed"}}\n' "$id" ;;
    *'"command/execute"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":"cmd-echoed"}}\n' "$id" ;;
    *'"shutdown"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
  esac
done
"#;

    #[tokio::test]
    async fn echo_peer_end_to_end_wiring() {
        let Some(state) = test_state() else { return };

        // A discoverable extension dir with a real subprocess peer.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("echo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("extension.toml"),
            "name = \"echo\"\nversion = \"0.0.1\"\n\n[run]\ncommand = \"/bin/sh\"\nargs = [\"echo.sh\"]\n\n[capabilities]\ntools = true\ncommands = true\n",
        )
        .unwrap();
        std::fs::write(root.join("echo.sh"), ECHO_SH).unwrap();

        let (discovered, disc_failures) = discover(Some(tmp.path()), None);
        assert!(disc_failures.is_empty(), "{disc_failures:?}");
        assert_eq!(discovered.len(), 1);

        // Load with the daemon's real delegate (trust is bypassed on purpose —
        // this tests wiring; trust gating is init_chat_extension_host's job
        // and is covered by the operative + TrustStore suites).
        let delegate: Arc<dyn HostDelegate> = Arc::new(DaemonUiProvider { state: state.clone() });
        let host_info = HostInfo {
            name: "big-smooth-test".into(),
            version: "0.0.0".into(),
        };
        let workspace = WorkspaceInfo {
            root: tmp.path().to_string_lossy().into_owned(),
            trusted: true,
        };
        let (host, load_failures) = ExtensionHost::load(discovered, host_info, workspace, "web", ui_capabilities(), delegate).await;
        assert!(load_failures.is_empty(), "{load_failures:?}");
        assert_eq!(host.names(), vec!["echo"]);

        // The tool proxy registers under the dotted name and round-trips.
        let tools = host.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].schema().name, "echo.echo");
        let out = tools[0].execute(json!({ "text": "hi" })).await.unwrap();
        assert_eq!(out, "echoed");

        // Slash command through the chat interception path.
        *state.ext_host.write().unwrap() = Some(Arc::new(host));
        assert_eq!(try_ext_command(&state, "/echo hi").await.as_deref(), Some("cmd-echoed"));
        assert_eq!(try_ext_command(&state, "/echo:echo scoped").await.as_deref(), Some("cmd-echoed"));
        // Unregistered command falls through to the LLM.
        assert!(try_ext_command(&state, "/nosuch thing").await.is_none());

        // Hot reload (what /api/ext/reload + `th ext reload` drive) respawns
        // the subprocess; the command still answers afterwards.
        let host = state.ext_host.read().unwrap().clone().unwrap();
        host.reload("echo").await.unwrap();
        assert_eq!(try_ext_command(&state, "/echo again").await.as_deref(), Some("cmd-echoed"));
    }
}
