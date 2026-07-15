//! Smooth web dashboard — embedded Vite SPA served by axum.
//!
//! The web assets are compiled into the binary via `rust-embed`.
//! axum serves them as static files, with SPA fallback to index.html.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Router;
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "web/dist/"]
struct WebAssets;

/// Create the web UI router.
///
/// Serves embedded static files from the Vite build.
/// Unknown paths fall back to index.html (SPA routing).
pub fn web_router() -> Router {
    web_router_with_token(None)
}

/// Create the web UI router, injecting an auth token into `index.html`.
///
/// When `token` is `Some`, every served `index.html` (the SPA entry — both the
/// `/` request and the SPA fallback for client routes) carries a
/// `<script>window.__SMOOTH_TOKEN__="…"</script>` in its `<head>`, **before** the
/// app bundle. This is what lets the daemon serve smooth-web **same-origin** at
/// its own `http://127.0.0.1:8787/` — the SPA reads the token from the injected
/// global (highest priority in `operator.ts`'s `resolveTarget`) instead of a
/// `?token=` query string. `None` is the plain build (no injection), identical to
/// [`web_router`].
pub fn web_router_with_token(token: Option<&str>) -> Router {
    // Precompute the (optionally token-injected) index.html once at build time so
    // the per-request handler is a cheap clone, not a per-request string rewrite.
    let index = build_index_html(token).map(Arc::new);
    // The precomputed (optionally injected) index rides as router state, so the
    // fallback is a plain `Fn` handler axum accepts — not a capturing closure.
    Router::new().fallback(serve_web).with_state(index)
}

async fn serve_web(State(index): State<Option<Arc<String>>>, uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Exact asset match first — but route `index.html` itself through the
    // (possibly token-injected) index path below so it never bypasses injection.
    if !path.is_empty() && path != "index.html" {
        if let Some(content) = WebAssets::get(path) {
            let mime = content_type_for(path);
            return ([(header::CONTENT_TYPE, mime)], content.data.to_vec()).into_response();
        }
    }

    // SPA entry / fallback: serve the prebuilt (optionally injected) index.html.
    match index {
        Some(html) => Html((*html).clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Read `index.html` from the embedded assets and, when `token` is `Some`, inject
/// the `window.__SMOOTH_TOKEN__` global into its `<head>`. Returns `None` only
/// when the bundle has no `index.html` embedded (an unbuilt `web/dist/`).
fn build_index_html(token: Option<&str>) -> Option<String> {
    let raw = WebAssets::get("index.html")?;
    let html = String::from_utf8_lossy(&raw.data).into_owned();
    Some(match token {
        Some(t) => inject_token(&html, t),
        None => html,
    })
}

/// Inject `<script>window.__SMOOTH_TOKEN__="…"</script>` into `html`'s `<head>`,
/// right after the opening `<head>` tag so it runs **before** the app bundle. The
/// token is JSON-encoded so quoting/escaping is always safe. Falls back to
/// inserting before `</head>`, then to prepending, if the markup is unusual.
fn inject_token(html: &str, token: &str) -> String {
    let token_json = serde_json::to_string(token).unwrap_or_else(|_| "\"\"".to_string());
    let script = format!("<script>window.__SMOOTH_TOKEN__={token_json};</script>");
    if let Some(idx) = html.find("<head>") {
        let at = idx + "<head>".len();
        let (head, tail) = html.split_at(at);
        format!("{head}{script}{tail}")
    } else if let Some(idx) = html.find("</head>") {
        let (head, tail) = html.split_at(idx);
        format!("{head}{script}{tail}")
    } else {
        format!("{script}{html}")
    }
}

/// Resolve the Content-Type for a path, with PWA-specific overrides.
///
/// `mime_guess` doesn't know about `.webmanifest`; the Web App Manifest spec
/// requires `application/manifest+json` and Chrome warns when it's not.
fn content_type_for(path: &str) -> String {
    if path.ends_with(".webmanifest") {
        return "application/manifest+json".to_string();
    }
    mime_guess::from_path(path).first_or_octet_stream().as_ref().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webmanifest_uses_pwa_mime() {
        assert_eq!(content_type_for("manifest.webmanifest"), "application/manifest+json");
        assert_eq!(content_type_for("/manifest.webmanifest"), "application/manifest+json");
    }

    #[test]
    fn known_extensions_use_mime_guess() {
        assert_eq!(content_type_for("favicon.ico"), "image/x-icon");
        assert_eq!(content_type_for("pwa-512x512.png"), "image/png");
        assert!(
            content_type_for("assets/index-abc.js").starts_with("application/javascript")
                || content_type_for("assets/index-abc.js").starts_with("text/javascript")
        );
        assert_eq!(content_type_for("index.html"), "text/html");
    }

    #[test]
    fn unknown_extension_falls_back_to_octet_stream() {
        assert_eq!(content_type_for("blob.unknown"), "application/octet-stream");
    }

    #[test]
    fn inject_token_places_global_before_bundle_in_head() {
        let html = "<head><script src=\"/assets/app.js\"></script></head><body></body>";
        let out = inject_token(html, "tok-123");
        // The token global is present, JSON-quoted, and inside the head.
        assert!(out.contains("window.__SMOOTH_TOKEN__=\"tok-123\""), "token global injected: {out}");
        // It must appear BEFORE the app bundle so the SPA reads it on load.
        let token_at = out.find("__SMOOTH_TOKEN__").unwrap();
        let bundle_at = out.find("/assets/app.js").unwrap();
        assert!(token_at < bundle_at, "token global must precede the app bundle");
        // Injected right after the opening <head> tag.
        assert!(out.starts_with("<head><script>window.__SMOOTH_TOKEN__"), "injected at head start: {out}");
    }

    #[test]
    fn inject_token_json_escapes_quotes() {
        // A token containing a quote must be JSON-escaped so it can't break out of
        // the JS string literal. (Real tokens are UUID hex, but defend anyway.)
        let out = inject_token("<head></head>", "a\"b");
        assert!(out.contains(r#"window.__SMOOTH_TOKEN__="a\"b""#), "quote is backslash-escaped: {out}");
    }

    #[test]
    fn inject_token_falls_back_when_no_head_open_tag() {
        // No <head> open tag → insert before </head>.
        let out = inject_token("<html></head>", "t");
        assert!(out.contains("window.__SMOOTH_TOKEN__=\"t\""));
        // No head markup at all → prepend.
        let out2 = inject_token("<html></html>", "t");
        assert!(out2.starts_with("<script>window.__SMOOTH_TOKEN__=\"t\""));
    }

    #[test]
    fn build_index_html_injects_only_with_token() {
        // The embedded dist must carry an index.html for these to be Some.
        let plain = build_index_html(None).expect("dist has index.html");
        assert!(!plain.contains("__SMOOTH_TOKEN__"), "no token global without a token");
        let injected = build_index_html(Some("zzz")).expect("dist has index.html");
        assert!(injected.contains("window.__SMOOTH_TOKEN__=\"zzz\""), "token injected when supplied");
    }

    #[tokio::test]
    async fn router_serves_injected_index_at_root_and_fallback() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let app = web_router_with_token(Some("router-tok"));

        for path in ["/", "/some/spa/route"] {
            let res = app.clone().oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK, "path {path}");
            let body = res.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8_lossy(&body);
            assert!(
                text.contains("window.__SMOOTH_TOKEN__=\"router-tok\""),
                "index served at {path} must carry the injected token: {text}"
            );
        }
    }

    #[tokio::test]
    async fn router_serves_real_assets_unmodified() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        // A hashed asset path must serve the embedded file, not index.html.
        let app = web_router_with_token(Some("tok"));
        let res = app.oneshot(Request::builder().uri("/favicon.ico").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get(header::CONTENT_TYPE).map(|v| v.to_str().unwrap()),
            Some("image/x-icon"),
            "exact asset match keeps its mime, not text/html"
        );
    }
}
