use axum::{
    body::Body,
    extract::Path,
    http::{Request, StatusCode},
    response::IntoResponse,
};
use reqwest::Method;

const UPDATE_BASE: &str = "http://127.0.0.1:8682";

/// Proxy GET /api/installer/{target}/version → kaonic-installer
pub async fn get_version(Path(target): Path<String>) -> impl IntoResponse {
    proxy_get(format!("{UPDATE_BASE}/api/installer/{target}/version")).await
}

/// Proxy POST /api/installer/{target}/upload → kaonic-installer (streams multipart body)
pub async fn upload_update(Path(target): Path<String>, req: Request<Body>) -> impl IntoResponse {
    proxy_request(
        Method::POST,
        format!("{UPDATE_BASE}/api/installer/{target}/upload"),
        Some(req),
    )
    .await
}

pub async fn list_plugins() -> impl IntoResponse {
    proxy_get(format!("{UPDATE_BASE}/api/plugins")).await
}

pub async fn install_plugin(req: Request<Body>) -> impl IntoResponse {
    proxy_request(
        Method::POST,
        format!("{UPDATE_BASE}/api/plugins/install"),
        Some(req),
    )
    .await
}

pub async fn upload_plugin(Path(plugin_id): Path<String>, req: Request<Body>) -> impl IntoResponse {
    proxy_request(
        Method::POST,
        format!("{UPDATE_BASE}/api/plugins/{plugin_id}/upload"),
        Some(req),
    )
    .await
}

pub async fn start_plugin(Path(plugin_id): Path<String>) -> impl IntoResponse {
    proxy_request(
        Method::POST,
        format!("{UPDATE_BASE}/api/plugins/{plugin_id}/start"),
        None,
    )
    .await
}

pub async fn stop_plugin(Path(plugin_id): Path<String>) -> impl IntoResponse {
    proxy_request(
        Method::POST,
        format!("{UPDATE_BASE}/api/plugins/{plugin_id}/stop"),
        None,
    )
    .await
}

pub async fn restart_plugin(Path(plugin_id): Path<String>) -> impl IntoResponse {
    proxy_request(
        Method::POST,
        format!("{UPDATE_BASE}/api/plugins/{plugin_id}/restart"),
        None,
    )
    .await
}

pub async fn delete_plugin(Path(plugin_id): Path<String>) -> impl IntoResponse {
    proxy_request(
        Method::DELETE,
        format!("{UPDATE_BASE}/api/plugins/{plugin_id}"),
        None,
    )
    .await
}

async fn proxy_get(url: String) -> impl IntoResponse {
    match reqwest::Client::new().get(&url).send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let body = resp.text().await.unwrap_or_default();
            (status, body).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("{{\"detail\":\"kaonic-installer unreachable: {e}\"}}"),
        )
            .into_response(),
    }
}

async fn proxy_request(
    method: Method,
    url: String,
    req: Option<Request<Body>>,
) -> axum::response::Response {
    let client = reqwest::Client::new();
    let mut builder = client.request(method, &url);

    if let Some(req) = req {
        let content_type = req
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        let body_bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
            Ok(b) => b,
            Err(err) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("failed to read body: {err}"),
                )
                    .into_response()
            }
        };

        if let Some(content_type) = content_type {
            builder = builder.header(reqwest::header::CONTENT_TYPE, content_type);
        }
        builder = builder.body(body_bytes.to_vec());
    }

    match builder.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let body = resp.text().await.unwrap_or_default();
            (status, body).into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            format!("{{\"detail\":\"kaonic-installer unreachable: {err}\"}}"),
        )
            .into_response(),
    }
}
