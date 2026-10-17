//! The LLM provider gate (pearl th-473294): does this daemon have model
//! credentials at all, and if not, what are the two ways to get some?
//!
//! `GET /api/llm/provider` answers for clients (`th harness add --agentic`
//! asks before it drives a turn, and prompts for a provider when the answer
//! is "none"); [`status`] is the same answer for the `add_harness` tool, which
//! returns a structured `needs_provider` result instead of failing mid-run.
//!
//! Resolution mirrors `operator::resolve_gateway_config`: the operator's env
//! gateway (`SMOOAI_GATEWAY_KEY`) first, else the `coding` slot of
//! `~/.smooth/providers.json`. Model name + gateway host only — never a key.
//!
//! The daemon reads its gateway ONCE at boot (the operator's `ServerConfig`
//! is fixed for the server's lifetime), so a provider added — or switched to a
//! different host — while it runs is reported as `configured` **with
//! `restart_required: true`** — the honest answer, and the reason the CLI tells
//! the user to bounce Big Smooth. (Hot reload was evaluated for SMOODEV-3918
//! and is not possible yet: `LocalServer`'s builder takes the config by value
//! into an `Arc` and exposes no per-turn gateway-key resolver.)
//!
//! Two token-gated writers back the web SPA's first-run card (SMOODEV-3918):
//! `POST /api/llm/provider` (your own key — validated, probed, saved; the same
//! core as the `configure_provider` tool) and `POST /api/llm/provider/family`
//! (mint this machine's gateway key with the signed-in Smoo session; a 402
//! comes back as `plan_required` + the checkout URL, which the card opens and
//! then polls this route until the trial is active).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use smooth_cast::family_plan::{self, MintOutcome};

/// One way to get a provider, offered when there is none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderOption {
    /// `smoo-gateway` | `byo-key`.
    pub id: String,
    pub title: String,
    pub description: String,
    /// What to run.
    pub command: String,
}

/// The gate's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub configured: bool,
    /// `env` | `providers.json` when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Host of the gateway URL (`llm.smoo.ai`), never the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_host: Option<String>,
    /// Credentials exist on disk but the running daemon booted without any —
    /// restart it before a turn can use them.
    #[serde(default)]
    pub restart_required: bool,
    /// How to get one, when `configured` is false.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ProviderOption>,
}

/// The two offers, the Family plan first.
#[must_use]
pub fn options() -> Vec<ProviderOption> {
    vec![
        ProviderOption {
            id: "smoo-gateway".into(),
            title: "Family plan (recommended)".into(),
            description: format!(
                "$9/month with a 14-day free trial: hosted models through the Smoo AI gateway, no keys to manage. Sign in with Smoo and start the trial. {}.",
                smooth_cast::ai_credits::top_up_line(" during the trial")
            ),
            command: "th model setup   # choose “Family plan”".into(),
        },
        ProviderOption {
            id: "byo-key".into(),
            title: "Your own key".into(),
            description:
                "Any OpenAI-compatible or Anthropic endpoint (OpenAI, Anthropic, OpenRouter, Groq, Ollama, LM Studio, …) saved to ~/.smooth/providers.json."
                    .into(),
            command: "th providers add <id> --url <base-url> --api-key <key> --format openai|anthropic --model <model>   (or: th model setup)".into(),
        },
    ]
}

static BOOT_CONFIGURED: OnceLock<bool> = OnceLock::new();
static BOOT_HOST: OnceLock<Option<String>> = OnceLock::new();

/// Record whether the daemon booted with a gateway key, and which host it
/// talks to (called once from `serve_local_flavor`). Later calls are ignored.
pub fn mark_boot(configured: bool, gateway_url: &str) {
    let _ = BOOT_CONFIGURED.set(configured);
    let _ = BOOT_HOST.set(if configured { host_of(gateway_url) } else { None });
}

/// A provider switched to a different host since boot also needs a restart.
#[must_use]
pub fn switched_since_boot(boot_host: Option<&str>, status: &ProviderStatus) -> bool {
    match (boot_host, status.gateway_host.as_deref()) {
        (Some(boot), Some(now)) => status.configured && !boot.eq_ignore_ascii_case(now),
        _ => false,
    }
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split('/').next().map(str::to_string).filter(|h| !h.is_empty())
}

/// Pure core of [`status`].
///
/// `env_key` is `SMOOAI_GATEWAY_KEY`, `env_url` its URL, `providers` the
/// providers.json path, `boot_configured` what the daemon booted with
/// (`None` outside a running daemon ⇒ no restart advice).
#[must_use]
pub fn status_from(
    env_url: Option<&str>,
    env_key: Option<&str>,
    env_model: Option<&str>,
    providers: Option<&Path>,
    boot_configured: Option<bool>,
) -> ProviderStatus {
    let (configured, source, model, host) = if env_key.is_some_and(|k| !k.trim().is_empty()) {
        (true, Some("env".to_string()), env_model.map(str::to_string), env_url.and_then(host_of))
    } else if let Some((url, _key, model)) = providers.and_then(|p| crate::operator::gateway_from_providers_at(p, "coding")) {
        (true, Some("providers.json".to_string()), Some(model), host_of(&url))
    } else {
        (false, None, None, None)
    };
    ProviderStatus {
        configured,
        source,
        model,
        gateway_host: host,
        restart_required: configured && boot_configured == Some(false),
        options: if configured { Vec::new() } else { options() },
    }
}

fn providers_path() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".smooth").join("providers.json"))
}

/// The live answer: env + `~/.smooth/providers.json` + the boot marker.
#[must_use]
pub fn status() -> ProviderStatus {
    let mut s = status_from_live();
    if switched_since_boot(BOOT_HOST.get().and_then(Option::as_deref), &s) {
        s.restart_required = true;
    }
    s
}

fn status_from_live() -> ProviderStatus {
    status_from(
        std::env::var("SMOOAI_GATEWAY_URL").ok().as_deref(),
        std::env::var("SMOOAI_GATEWAY_KEY").ok().as_deref(),
        // `SMOOTH_AGENT_MODEL`, or `th settings set model …` (th-f95ecf).
        smooth_policy::settings::raw("model").as_deref(),
        providers_path().as_deref(),
        BOOT_CONFIGURED.get().copied(),
    )
}

#[derive(Clone)]
struct ProviderRouteState {
    token: Option<String>,
    providers: Option<PathBuf>,
}

/// `GET /api/llm/provider` (ungated like `/api/mode`: a model name and a
/// gateway host must render on a tokenless connection, and neither is a
/// secret) plus the two token-gated first-run writers.
pub fn provider_router(token: Option<String>) -> Router {
    provider_router_at(token, providers_path())
}

fn provider_router_at(token: Option<String>, providers: Option<PathBuf>) -> Router {
    Router::new()
        .route("/api/llm/provider", get(provider_handler).post(byo_handler))
        .route("/api/llm/provider/family", post(family_handler))
        .with_state(ProviderRouteState { token, providers })
}

async fn provider_handler() -> Json<ProviderStatus> {
    let s = tokio::task::spawn_blocking(status)
        .await
        .unwrap_or_else(|_| status_from(None, None, None, None, None));
    Json(s)
}

type Reply = (StatusCode, Json<Value>);

fn gate(state: &ProviderRouteState, headers: &HeaderMap, query: &HashMap<String, String>) -> Result<PathBuf, Reply> {
    if !crate::flow_route::authorized(state.token.as_deref(), headers, query) {
        return Err((StatusCode::UNAUTHORIZED, Json(json!({ "error": "missing or invalid local token" }))));
    }
    state
        .providers
        .clone()
        .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "no home directory" }))))
}

/// `POST /api/llm/provider` — the web card's "your own key": the
/// `configure_provider` core (validate → one test request → save).
async fn byo_handler(
    State(state): State<ProviderRouteState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> Reply {
    let path = match gate(&state, &headers, &query) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let report = crate::configure_provider::configure(&body, &path, |p| async move { smooth_cast::provider_setup::probe(&p).await }).await;
    let code = if report.status == "configured" {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    (code, Json(serde_json::to_value(&report).unwrap_or_default()))
}

/// The Family plan step, testable: `session` is `(access_token, active org)`
/// from the signed-in Smoo session (`None` ⇒ `signed_out`). With no active org
/// the user's orgs are listed every call — a brand-new user gets a personal org
/// only once checkout starts, so it must be re-discovered while the card polls.
pub async fn family_setup(http: &reqwest::Client, api_base: &str, session: Option<(String, Option<String>)>, key_name: &str, path: &Path) -> Reply {
    let Some((bearer, org)) = session else {
        return (
            StatusCode::OK,
            Json(json!({ "status": "signed_out", "message": "Sign in with Smoo first — the Family plan lives on your Smoo account." })),
        );
    };
    let org = match org {
        Some(o) => Ok(Some(o)),
        None => family_plan::discover_org(http, api_base, &bearer).await,
    };
    let outcome = match org {
        Ok(Some(org)) => family_plan::mint_or_rotate(http, api_base, &bearer, &org, key_name).await,
        Ok(None) => Ok(family_plan::no_org_yet()),
        Err(e) => Err(e),
    };
    match outcome {
        Ok(MintOutcome::Minted(key)) => match family_plan::save_gateway_key(path, &key) {
            Ok(()) => (
                StatusCode::OK,
                Json(json!({
                    "status": "saved",
                    "keyName": key_name,
                    "restart_required": true,
                    "message": "Family plan active. Big Smooth reads its model at startup — restart it (th down && th up, or quit and reopen the app) to start using it.",
                    "topUp": family_plan::top_up_note(),
                })),
            ),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "status": "error", "message": format!("saving the key failed: {e:#}") })),
            ),
        },
        Ok(MintOutcome::PlanRequired { upgrade_url, message }) => (
            StatusCode::OK,
            Json(
                json!({ "status": "plan_required", "upgradeUrl": upgrade_url, "message": message, "pitch": family_plan::FAMILY_PLAN_PITCH, "topUp": family_plan::top_up_note() }),
            ),
        ),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "status": "error", "message": format!("{e:#}") }))),
    }
}

/// `POST /api/llm/provider/family` — mint (or 402 → `plan_required`).
async fn family_handler(State(state): State<ProviderRouteState>, headers: HeaderMap, Query(query): Query<HashMap<String, String>>) -> Reply {
    let path = match gate(&state, &headers, &query) {
        Ok(p) => p,
        Err(r) => return r,
    };
    // Fresh read every call — the credential heartbeat rotates the session.
    let (session, host) = tokio::task::spawn_blocking(|| {
        let creds = smooai_client_shared::auth::storage::CredentialsStore::default_user()
            .ok()
            .and_then(|s| s.load().ok().flatten());
        let session = creds
            .filter(|c| !c.access_token.is_empty())
            .map(|c| (c.access_token, c.active_org_id.filter(|o| !o.trim().is_empty())));
        (session, family_plan::this_host())
    })
    .await
    .unwrap_or((None, String::new()));
    let api_base = std::env::var("SMOOAI_API_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://api.smoo.ai".into());
    family_setup(
        &crate::auth_login::bounded_http_client(),
        &api_base,
        session,
        &family_plan::key_name(&host),
        &path,
    )
    .await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn env_key_wins_and_reports_the_host_not_the_key() {
        let s = status_from(Some("https://llm.smoo.ai/v1"), Some("sk-secret"), Some("m1"), None, Some(true));
        assert!(s.configured);
        assert_eq!(s.source.as_deref(), Some("env"));
        assert_eq!(s.gateway_host.as_deref(), Some("llm.smoo.ai"));
        assert_eq!(s.model.as_deref(), Some("m1"));
        assert!(!s.restart_required);
        assert!(s.options.is_empty());
        let j = serde_json::to_string(&s).unwrap();
        assert!(!j.contains("sk-secret"), "{j}");
    }

    #[test]
    fn blank_env_key_falls_through_to_providers_json() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("providers.json");
        std::fs::write(
            &p,
            r#"{"providers":[{"id":"smooai-gateway","api_url":"https://llm.smoo.ai/v1","api_key":"k","api_format":"openai","default_model":"d"}],"routing":{"coding":{"provider":"smooai-gateway","model":"deepseek-v4-flash"}}}"#,
        )
        .unwrap();
        let s = status_from(None, Some("   "), None, Some(&p), Some(false));
        assert!(s.configured);
        assert_eq!(s.source.as_deref(), Some("providers.json"));
        assert_eq!(s.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(s.gateway_host.as_deref(), Some("llm.smoo.ai"));
        assert!(s.restart_required, "booted without a key, now has one ⇒ restart");
        assert!(!serde_json::to_string(&s).unwrap().contains("\"k\""));
    }

    #[test]
    fn nothing_configured_offers_smoo_first_then_byo() {
        let s = status_from(None, None, None, Some(Path::new("/nonexistent/providers.json")), None);
        assert!(!s.configured);
        assert!(!s.restart_required);
        assert_eq!(s.options.len(), 2);
        assert_eq!(s.options[0].id, "smoo-gateway");
        assert_eq!(s.options[1].id, "byo-key");
        assert!(s.options[0].title.contains("Family plan"));
        assert!(s.options[0].description.contains("$9/month") && s.options[0].description.contains("14-day"));
        assert!(
            s.options[0].description.contains("Top up $5 or $10 anytime during the trial"),
            "SMOODEV-3969: onboarding names top-ups"
        );
        assert!(s.options[0].command.contains("th model setup"));
        assert!(s.options[1].command.contains("--format openai|anthropic"));
    }

    #[test]
    fn restart_advice_only_when_the_boot_marker_says_unconfigured() {
        let s = status_from(Some("https://x/v1"), Some("k"), None, None, None);
        assert!(!s.restart_required, "no marker (not inside a daemon) ⇒ no advice");
        let s = status_from(Some("https://x/v1"), Some("k"), None, None, Some(true));
        assert!(!s.restart_required);
    }

    #[test]
    fn host_of_handles_bare_and_pathed_urls() {
        assert_eq!(host_of("https://llm.smoo.ai/v1").as_deref(), Some("llm.smoo.ai"));
        assert_eq!(host_of("localhost:11434").as_deref(), Some("localhost:11434"));
        assert_eq!(host_of("").as_deref(), None);
    }

    #[tokio::test]
    async fn handler_returns_the_envelope_without_secrets() {
        let resp = provider_handler().await;
        let j = serde_json::to_value(&resp.0).unwrap();
        assert!(j.get("configured").is_some());
        for forbidden in ["api_key", "key\":", "sk-"] {
            assert!(!j.to_string().contains(forbidden), "{j}");
        }
    }
    #[test]
    fn switching_hosts_after_boot_needs_a_restart() {
        let mut s = status_from(Some("https://api.openai.com/v1"), Some("k"), None, None, Some(true));
        assert!(switched_since_boot(Some("llm.smoo.ai"), &s));
        assert!(!switched_since_boot(Some("API.OPENAI.COM"), &s), "same host, case-insensitive");
        assert!(!switched_since_boot(None, &s), "outside a daemon ⇒ no advice");
        s.configured = false;
        assert!(!switched_since_boot(Some("llm.smoo.ai"), &s));
    }

    fn post_req(uri: &str, token: Option<&str>, body: &str) -> axum::http::Request<axum::body::Body> {
        let mut b = axum::http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(axum::body::Body::from(body.to_string())).unwrap()
    }

    async fn json_of(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn writers_require_the_local_token() {
        use tower::ServiceExt as _;
        let dir = tempfile::tempdir().unwrap();
        let app = provider_router_at(Some("tok".into()), Some(dir.path().join("providers.json")));
        let resp = app.clone().oneshot(post_req("/api/llm/provider", None, "{}")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = app.oneshot(post_req("/api/llm/provider/family", Some("wrong"), "{}")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn byo_route_rejects_bad_input_without_writing() {
        use tower::ServiceExt as _;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("providers.json");
        let app = provider_router_at(Some("tok".into()), Some(p.clone()));
        let resp = app
            .oneshot(post_req(
                "/api/llm/provider",
                Some("tok"),
                r#"{"format":"openai","base_url":"ftp://x","api_key":"k","model":"m"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let j = json_of(resp).await;
        assert_eq!(j["status"], "invalid");
        assert!(!p.exists());
    }

    /// A mock api.smoo.ai answering the key mint with `(status, body)`.
    async fn mock_api(status: u16, body: &'static str) -> String {
        let app = Router::new()
            .route(
                "/organizations/{org}/llm-gateway/keys",
                post(move || async move { (StatusCode::from_u16(status).unwrap(), body) }),
            )
            .route("/organizations", get(|| async { r#"{"data":[]}"# }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn family_setup_walks_signed_out_plan_required_and_saved() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("providers.json");
        let http = reqwest::Client::new();
        let (code, Json(j)) = family_setup(&http, "http://127.0.0.1:9", None, "big-smooth-x", &p).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(j["status"], "signed_out");

        let base = mock_api(402, r#"{"error":"family_plan_required","message":"Start your trial","upgradeUrl":"https://smoo.ai/apps/start/product?plan=Family%20plan&term=monthly"}"#).await;
        let (code, Json(j)) = family_setup(&http, &base, Some(("jwt".into(), Some("org".into()))), "big-smooth-x", &p).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(j["status"], "plan_required");
        assert_eq!(j["upgradeUrl"], family_plan::FAMILY_PLAN_URL);
        assert_eq!(j["topUp"], family_plan::top_up_note());
        assert!(!p.exists(), "nothing saved before the plan is active");

        // Signed in but no org yet (a brand-new user): checkout creates one.
        let (code, Json(j)) = family_setup(&http, &base, Some(("jwt".into(), None)), "big-smooth-x", &p).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(j["status"], "plan_required", "{j}");

        let base = mock_api(201, r#"{"key":"sk-family"}"#).await;
        let (code, Json(j)) = family_setup(&http, &base, Some(("jwt".into(), Some("org".into()))), "big-smooth-x", &p).await;
        assert_eq!(code, StatusCode::OK, "{j}");
        assert_eq!(j["status"], "saved");
        assert_eq!(j["topUp"], family_plan::top_up_note());
        assert!(!j.to_string().contains("sk-family"), "the key never goes back to the browser");
        // The URL follows SMOOAI_GATEWAY_URL, which sibling tests set — check the id.
        let (_, key, _) = smooth_cast::providers::route_at(&p, "coding").unwrap();
        assert_eq!(key, "sk-family");
        let root = smooth_cast::providers::load_value(&p).unwrap();
        assert_eq!(root["routing"]["coding"]["provider"], "smooai-gateway");
    }
}
