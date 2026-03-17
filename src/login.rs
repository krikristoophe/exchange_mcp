use axum::{http::StatusCode, response::IntoResponse};

/// Extract Bearer token from an HTTP request's Authorization header.
pub fn extract_bearer_token<B>(req: &http::Request<B>) -> Option<String> {
    let auth = req.headers().get("authorization")?;
    let val = auth.to_str().ok()?;
    val.strip_prefix("Bearer ").map(|t| t.to_string())
}

/// GET /favicon.ico — Return empty response to avoid 404
pub async fn favicon() -> impl IntoResponse {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "image/x-icon".parse().unwrap(),
    );
    (StatusCode::OK, headers, &[] as &[u8])
}
