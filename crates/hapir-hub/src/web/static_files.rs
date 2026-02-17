use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../web/dist/"]
struct WebAssets;

/// Serve embedded static files with SPA fallback.
pub async fn static_handler(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Try the exact path first
    if let Some(resp) = serve_embedded(path) {
        return resp;
    }

    // SPA fallback: serve index.html for non-file paths
    if let Some(resp) = serve_embedded("index.html") {
        return resp;
    }

    // In debug mode, try reading from disk if embedded assets are empty
    #[cfg(debug_assertions)]
    {
        if let Some(resp) = serve_from_disk(path).await {
            return resp;
        }
        if let Some(resp) = serve_from_disk("index.html").await {
            return resp;
        }
    }

    (
        StatusCode::SERVICE_UNAVAILABLE,
        Html(
            "<h1>Frontend not built</h1>\
             <p>Run <code>cd web && bun run build</code> first, \
             or <code>cargo build --release -p hapir-hub</code> to auto-build.</p>",
        ),
    )
        .into_response()
}

fn serve_embedded(path: &str) -> Option<Response> {
    let file = WebAssets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    Some(
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime.as_ref())],
            file.data,
        )
            .into_response(),
    )
}

#[cfg(debug_assertions)]
async fn serve_from_disk(path: &str) -> Option<Response> {
    use tokio::fs;

    let candidates = ["web/dist", "../../web/dist"];

    for base in &candidates {
        let full = std::path::Path::new(base).join(path);
        if let Ok(bytes) = fs::read(&full).await {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            return Some(
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, mime.as_ref().to_owned())],
                    bytes,
                )
                    .into_response(),
            );
        }
    }
    None
}
