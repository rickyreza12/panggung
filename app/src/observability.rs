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
use tracing::Instrument;

#[derive(Default)]
pub struct Metrics {
    requests: AtomicU64,
    failures: AtomicU64,
    duration_micros: AtomicU64,
    buckets: [AtomicU64; 8],
}
const BUCKETS: [u64; 8] = [5000, 10000, 25000, 50000, 100000, 250000, 1000000, 5000000];
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
    let method = match request.method().as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    };
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let span = tracing::info_span!("http.request", request_id=%id, method=method, status=tracing::field::Empty);
    let mut response = next.run(request).instrument(span.clone()).await;
    span.record("status", response.status().as_u16());
    let micros = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    metrics.requests.fetch_add(1, Ordering::Relaxed);
    metrics.duration_micros.fetch_add(micros, Ordering::Relaxed);
    for (bound, count) in BUCKETS.iter().zip(&metrics.buckets) {
        if micros <= *bound {
            count.fetch_add(1, Ordering::Relaxed);
        }
    }
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
    let mut output = format!(
        "# HELP panggung_http_requests_total Completed requests.\n# TYPE panggung_http_requests_total counter\npanggung_http_requests_total {}\n# HELP panggung_http_server_errors_total Server errors.\n# TYPE panggung_http_server_errors_total counter\npanggung_http_server_errors_total {}\n# HELP panggung_http_request_duration_seconds Request duration in seconds.\n# TYPE panggung_http_request_duration_seconds histogram\n",
        metrics.requests.load(Ordering::Relaxed),
        metrics.failures.load(Ordering::Relaxed)
    );
    use std::fmt::Write;
    for (bound, count) in BUCKETS.iter().zip(&metrics.buckets) {
        let _ = writeln!(
            output,
            "panggung_http_request_duration_seconds_bucket{{le=\"{}\"}} {}",
            *bound as f64 / 1_000_000.0,
            count.load(Ordering::Relaxed)
        );
    }
    let count = metrics.requests.load(Ordering::Relaxed);
    let _ = writeln!(
        output,
        "panggung_http_request_duration_seconds_bucket{{le=\"+Inf\"}} {count}\npanggung_http_request_duration_seconds_count {count}\npanggung_http_request_duration_seconds_sum {:.6}",
        metrics.duration_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0
    );
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        output,
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
