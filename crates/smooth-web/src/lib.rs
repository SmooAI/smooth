//! Smooth web dashboard — embedded Vite SPA served by axum.
//!
//! The web assets are compiled into the binary via `rust-embed`.
//! axum serves them as static files, with SPA fallback to index.html.

use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
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
    Router::new().fallback(get(serve_web))
}

async fn serve_web(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Try exact file match first
    if let Some(content) = WebAssets::get(path) {
        let mime = content_type_for(path);
        return ([(header::CONTENT_TYPE, mime)], content.data.to_vec()).into_response();
    }

    // SPA fallback: serve index.html for all unknown paths
    if let Some(content) = WebAssets::get("index.html") {
        return Html(String::from_utf8_lossy(&content.data).to_string()).into_response();
    }

    StatusCode::NOT_FOUND.into_response()
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
}
