use crate::identity::{Role, User};
use axum::{
    Extension, Router,
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use rand_core::{OsRng, RngCore};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

#[derive(Default)]
pub struct Metrics {
    requests: AtomicU64,
    failures: AtomicU64,
    duration_micros: AtomicU64,
}
pub fn routes(metrics: Arc<Metrics>) -> Router {
    Router::new()
        .route("/api/diagnostics/metrics", get(metrics_text))
        .with_state(metrics)
}
pub fn instrument(router: Router, metrics: Arc<Metrics>) -> Router {
    router.layer(middleware::from_fn_with_state(metrics, observe))
}
async fn observe(State(metrics): State<Arc<Metrics>>, request: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut response = next.run(request).await;
    let micros = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    metrics.requests.fetch_add(1, Ordering::Relaxed);
    metrics.duration_micros.fetch_add(micros, Ordering::Relaxed);
    if response.status().is_server_error() {
        metrics.failures.fetch_add(1, Ordering::Relaxed);
    }
    tracing::info!(request_id=%id,method=%method,status=response.status().as_u16(),duration_micros=micros,"request completed");
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}
async fn metrics_text(
    State(metrics): State<Arc<Metrics>>,
    Extension(user): Extension<User>,
) -> Result<([(axum::http::HeaderName, &'static str); 1], String), StatusCode> {
    if user.role != Role::Admin {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        format!(
            "# TYPE panggung_http_requests_total counter\npanggung_http_requests_total {}\n# TYPE panggung_http_server_errors_total counter\npanggung_http_server_errors_total {}\n# TYPE panggung_http_request_duration_seconds_sum counter\npanggung_http_request_duration_seconds_sum {:.6}\n",
            metrics.requests.load(Ordering::Relaxed),
            metrics.failures.load(Ordering::Relaxed),
            metrics.duration_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    #[tokio::test]
    async fn counts_responses_and_replaces_untrusted_request_id() {
        let metrics = Arc::new(Metrics::default());
        let app = instrument(
            Router::new().route("/", get(|| async { StatusCode::INTERNAL_SERVER_ERROR })),
            metrics.clone(),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header("x-request-id", "untrusted")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers()["x-request-id"].to_str().unwrap().len(),
            32
        );
        assert_eq!(metrics.requests.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.failures.load(Ordering::Relaxed), 1);
    }
}
