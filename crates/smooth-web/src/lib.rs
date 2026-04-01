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
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return ([(header::CONTENT_TYPE, mime.as_ref().to_string())], content.data.to_vec()).into_response();
    }

    // SPA fallback: serve index.html for all unknown paths
    if let Some(content) = WebAssets::get("index.html") {
        return Html(String::from_utf8_lossy(&content.data).to_string()).into_response();
    }

    StatusCode::NOT_FOUND.into_response()
}
