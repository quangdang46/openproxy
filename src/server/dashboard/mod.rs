use axum::{
    body::Body,
    extract::Path,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use axum::response::Html;
use http_body_util::BodyExt;

use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(dashboard))
        .route("/dashboard", get(dashboard))
        .route("/dashboard/{*path}", get(proxy_dashboard))
}

async fn dashboard() -> Html<&'static str> {
    Html(include_str!("dashboard.html"))
}

async fn proxy_dashboard(
    Path(path): Path<String>,
    req: Request<Body>,
) -> Response {
    let target_url = format!("http://127.0.0.1:3000/dashboard/{}", path);

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("Client error: {}", e)).into_response(),
    };

    let method = req.method().clone();
    let mut builder = client.request(method, &target_url);

    for (name, value) in req.headers() {
        let name_str = name.as_str();
        if !is_hop_by_hop(name_str) {
            builder = builder.header(name_str, value.as_bytes());
        }
    }

    let body = req.into_body();
    let bytes = match body.collect().await {
        Ok(b) => b.to_bytes().to_vec(),
        Err(_) => Vec::new(),
    };

    let req_builder = builder.body(bytes);

    let response = req_builder.send().await;

    match response {
        Ok(resp) => {
            let status_code = resp.status().as_u16();
            let mut builder = axum::response::Response::builder().status(status_code);

            for (name, value) in resp.headers().iter() {
                let name_str = name.as_str();
                if !is_hop_by_hop(name_str) {
                    builder = builder.header(name_str, value.as_bytes());
                }
            }
            let body = resp.bytes().await.unwrap_or_default().to_vec();
            builder.body(Body::from(body)).unwrap_or_else(|e| {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            })
        }
        Err(e) => {
            if e.is_connect() || e.is_timeout() {
                (StatusCode::BAD_GATEWAY, "Dashboard server not reachable on port 3000. Is the Next.js dev server running?".to_string()).into_response()
            } else {
                (StatusCode::BAD_GATEWAY, format!("Failed to proxy dashboard: {}", e)).into_response()
            }
        }
    }
}

type Response = axum::response::Response;

fn is_hop_by_hop(header: &str) -> bool {
    matches!(
        header.to_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
            | "host"
    )
}