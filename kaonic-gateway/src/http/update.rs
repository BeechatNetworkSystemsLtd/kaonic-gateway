use axum::{
    body::Body,
    extract::Path,
    http::{Request, StatusCode},
    response::IntoResponse,
};

const UPDATE_BASE: &str = "http://127.0.0.1:8682";

/// Proxy GET /api/update/{target}/version → kaonic-update
pub async fn get_version(Path(target): Path<String>) -> impl IntoResponse {
    proxy_get(format!("{UPDATE_BASE}/api/update/{target}/version")).await
}

/// Proxy POST /api/update/{target}/upload → kaonic-update (streams multipart body)
pub async fn upload_update(Path(target): Path<String>, req: Request<Body>) -> impl IntoResponse {
    let client = reqwest::Client::new();
    let url = format!("{UPDATE_BASE}/api/update/{target}/upload");

    // Forward the raw body and content-type header so multipart works
    let content_type = req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let body_bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("failed to read body: {e}")).into_response()
        }
    };

    let result = client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, &content_type)
        .body(body_bytes.to_vec())
        .send()
        .await;

    match result {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let body = resp.text().await.unwrap_or_default();
            (status, body).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("{{\"detail\":\"kaonic-update unreachable: {e}\"}}"),
        )
            .into_response(),
    }
}

async fn proxy_get(url: String) -> impl IntoResponse {
    let client = reqwest::Client::new();
    match client.get(&url).send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let body = resp.text().await.unwrap_or_default();
            (status, body).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("{{\"detail\":\"kaonic-update unreachable: {e}\"}}"),
        )
            .into_response(),
    }
}
