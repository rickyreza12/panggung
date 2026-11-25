use crate::identity::{Role, User};
use axum::{
    Extension, Router,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use opentelemetry::{
    Context, KeyValue,
    metrics::{Counter, Histogram, MeterProvider},
    trace::{Span, SpanKind, Status, TraceContextExt, Tracer, TracerProvider},
};
use opentelemetry_sdk::{
    Resource,
    metrics::SdkMeterProvider,
    trace::{SdkTracer, SdkTracerProvider},
};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use tracing::Instrument;

pub struct Metrics {
    snapshot: Mutex<Snapshot>,
    operation_counts: Mutex<[[u64; 3]; 2]>,
    telemetry: Telemetry,
    scrape_token: Option<[u8; 32]>,
}
struct Telemetry {
    _meter_provider: SdkMeterProvider,
    _tracer_provider: SdkTracerProvider,
    tracer: SdkTracer,
    requests: Counter<u64>,
    duration: Histogram<f64>,
    operations: Counter<u64>,
    operation_duration: Histogram<f64>,
}
#[derive(Clone)]
pub struct RequestTrace(Context);

#[derive(Clone, Copy)]
pub enum Operation {
    Webhook,
    Publish,
}
impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Webhook => "webhook",
            Self::Publish => "publish",
        }
    }
}

#[derive(Clone, Copy)]
pub enum OperationOutcome {
    Success,
    Rejected,
    Failure,
}
impl OperationOutcome {
    fn name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Rejected => "rejected",
            Self::Failure => "failure",
        }
    }
}

pub struct OperationGuard {
    metrics: Arc<Metrics>,
    kind: Operation,
    span: Option<opentelemetry_sdk::trace::Span>,
    started: Instant,
}
impl OperationGuard {
    pub fn finish(mut self, outcome: OperationOutcome) {
        self.record(outcome);
    }
    fn record(&mut self, outcome: OperationOutcome) {
        let Some(mut span) = self.span.take() else {
            return;
        };
        let attrs = [
            KeyValue::new("panggung.operation", self.kind.name()),
            KeyValue::new("panggung.outcome", outcome.name()),
        ];
        self.metrics.telemetry.operations.add(1, &attrs);
        self.metrics
            .telemetry
            .operation_duration
            .record(self.started.elapsed().as_secs_f64(), &attrs);
        let mut counts = self
            .metrics
            .operation_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let count = &mut counts[self.kind as usize][outcome as usize];
        *count = count.saturating_add(1);
        span.set_attribute(attrs[1].clone());
        if matches!(outcome, OperationOutcome::Failure) {
            span.set_status(Status::error("operation failed"));
        }
        span.end();
    }
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.record(OperationOutcome::Failure);
    }
}
impl Telemetry {
    fn new(export: bool) -> Result<Self, Box<dyn std::error::Error>> {
        let resource = Resource::builder().with_service_name("panggung").build();
        let mut meters = SdkMeterProvider::builder().with_resource(resource.clone());
        let mut traces = SdkTracerProvider::builder().with_resource(resource);
        if export {
            use opentelemetry_otlp::{MetricExporter, SpanExporter};
            let metrics = MetricExporter::builder().with_http().build()?;
            let spans = SpanExporter::builder().with_http().build()?;
            meters = meters.with_periodic_exporter(metrics);
            traces = traces.with_batch_exporter(spans);
        }
        Ok(Self::from_providers(meters.build(), traces.build()))
    }
    fn from_providers(
        meter_provider: SdkMeterProvider,
        tracer_provider: SdkTracerProvider,
    ) -> Self {
        let meter = meter_provider.meter("panggung");
        Self {
            tracer: tracer_provider.tracer("panggung"),
            requests: meter.u64_counter("http.server.request.count").build(),
            duration: meter
                .f64_histogram("http.server.request.duration")
                .with_unit("s")
                .build(),
            _meter_provider: meter_provider,
            _tracer_provider: tracer_provider,
            operations: meter.u64_counter("panggung.operation.count").build(),
            operation_duration: meter
                .f64_histogram("panggung.operation.duration")
                .with_unit("s")
                .build(),
        }
    }
}
impl Default for Metrics {
    fn default() -> Self {
        Self {
            snapshot: Mutex::default(),
            operation_counts: Mutex::default(),
            telemetry: Telemetry::new(false).expect("local telemetry initialization"),
            scrape_token: None,
        }
    }
}
#[derive(Default, Clone, Copy)]
struct Snapshot {
    requests: u64,
    failures: u64,
    duration_micros: u64,
    buckets: [u64; 8],
}
impl Metrics {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let export = match std::env::var("PANGGUNG_OTLP_ENABLED") {
            Ok(value) if value == "true" => true,
            Ok(value) if value == "false" => false,
            Err(std::env::VarError::NotPresent) => false,
            _ => {
                tracing::warn!("Invalid optional telemetry enablement; export disabled");
                false
            }
        };
        let scrape_token = match std::env::var("PANGGUNG_METRICS_TOKEN") {
            Ok(value) if value.len() >= 32 && value.bytes().all(|b| b.is_ascii_graphic()) => {
                Some(Sha256::digest(value.as_bytes()).into())
            }
            Err(std::env::VarError::NotPresent) => None,
            _ => return Err("PANGGUNG_METRICS_TOKEN must contain at least 32 printable non-space ASCII characters".into()),
        };
        Ok(Self {
            snapshot: Mutex::default(),
            operation_counts: Mutex::default(),
            telemetry: Telemetry::new(export).unwrap_or_else(|_| {
                tracing::warn!(
                    "Optional telemetry exporter initialization failed; export disabled"
                );
                Telemetry::new(false).expect("local telemetry initialization")
            }),
            scrape_token,
        })
    }
    pub fn operation(
        self: &Arc<Self>,
        kind: Operation,
        parent: Option<&RequestTrace>,
    ) -> OperationGuard {
        let builder = self
            .telemetry
            .tracer
            .span_builder(format!("panggung.{}", kind.name()))
            .with_kind(SpanKind::Internal)
            .with_attributes(vec![KeyValue::new("panggung.operation", kind.name())]);
        let span = builder.start_with_context(
            &self.telemetry.tracer,
            &parent.map(|p| p.0.clone()).unwrap_or_default(),
        );
        OperationGuard {
            metrics: self.clone(),
            kind,
            span: Some(span),
            started: Instant::now(),
        }
    }
    fn record(&self, micros: u64, failed: bool) {
        let mut s = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        s.requests = s.requests.saturating_add(1);
        s.failures = s.failures.saturating_add(u64::from(failed));
        s.duration_micros = s.duration_micros.saturating_add(micros);
        for (bound, count) in BUCKETS.iter().zip(&mut s.buckets) {
            if micros <= *bound {
                *count = count.saturating_add(1);
            }
        }
    }
    fn read(&self) -> Snapshot {
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner())
    }
}
const BUCKETS: [u64; 8] = [5000, 10000, 25000, 50000, 100000, 250000, 1000000, 5000000];
pub fn routes(metrics: Arc<Metrics>) -> Router {
    Router::new()
        .route("/api/diagnostics/metrics", get(metrics_text))
        .with_state(metrics)
}
pub fn scrape_routes(metrics: Arc<Metrics>) -> Router {
    if metrics.scrape_token.is_none() {
        return Router::new();
    }
    Router::new()
        .route("/metrics", get(scrape))
        .with_state(metrics)
}
type MetricsText = ([(axum::http::HeaderName, &'static str); 1], String);
async fn scrape(
    State(metrics): State<Arc<Metrics>>,
    headers: HeaderMap,
) -> Result<MetricsText, StatusCode> {
    let expected = metrics.scrape_token.ok_or(StatusCode::NOT_FOUND)?;
    let supplied = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let actual = Sha256::digest(supplied.as_bytes());
    // Compare fixed-size digests without short-circuiting on a differing byte.
    if expected
        .iter()
        .zip(actual)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        != 0
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(render_metrics(&metrics))
}
pub fn instrument(router: Router, metrics: Arc<Metrics>) -> Router {
    router.layer(middleware::from_fn_with_state(metrics, observe))
}
async fn observe(
    State(metrics): State<Arc<Metrics>>,
    mut request: Request,
    next: Next,
) -> Response {
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
    let otel_span = metrics
        .telemetry
        .tracer
        .span_builder("http.request")
        .with_kind(SpanKind::Server)
        .with_attributes(vec![KeyValue::new("http.request.method", method)])
        .start(&metrics.telemetry.tracer);
    let context = Context::new().with_span(otel_span);
    request.extensions_mut().insert(metrics.clone());
    request
        .extensions_mut()
        .insert(RequestTrace(context.clone()));
    let mut response = next.run(request).instrument(span.clone()).await;
    span.record("status", response.status().as_u16());
    let micros = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    metrics.record(micros, response.status().is_server_error());
    let status = i64::from(response.status().as_u16());
    let attributes = [
        KeyValue::new("http.request.method", method),
        KeyValue::new("http.response.status_code", status),
    ];
    metrics.telemetry.requests.add(1, &attributes);
    metrics
        .telemetry
        .duration
        .record(micros as f64 / 1_000_000.0, &attributes);
    let otel_span = context.span();
    otel_span.set_attribute(KeyValue::new("http.response.status_code", status));
    if response.status().is_server_error() {
        otel_span.set_status(Status::error("server error"));
    }
    otel_span.end();
    tracing::info!(request_id=%id,method=%method,status=response.status().as_u16(),duration_micros=micros,"request completed");
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}
async fn metrics_text(
    State(metrics): State<Arc<Metrics>>,
    Extension(user): Extension<User>,
) -> Result<MetricsText, StatusCode> {
    if user.role != Role::Admin {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(render_metrics(&metrics))
}
fn render_metrics(metrics: &Metrics) -> MetricsText {
    let operations = *metrics
        .operation_counts
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let metrics = metrics.read();
    let mut output = format!(
        "# HELP panggung_http_requests_total Completed requests.\n# TYPE panggung_http_requests_total counter\npanggung_http_requests_total {}\n# HELP panggung_http_server_errors_total Server errors.\n# TYPE panggung_http_server_errors_total counter\npanggung_http_server_errors_total {}\n# HELP panggung_http_request_duration_seconds Request duration in seconds.\n# TYPE panggung_http_request_duration_seconds histogram\n",
        metrics.requests, metrics.failures
    );
    use std::fmt::Write;
    for (bound, count) in BUCKETS.iter().zip(&metrics.buckets) {
        let _ = writeln!(
            output,
            "panggung_http_request_duration_seconds_bucket{{le=\"{}\"}} {}",
            *bound as f64 / 1_000_000.0,
            count
        );
    }
    let count = metrics.requests;
    let _ = writeln!(
        output,
        "panggung_http_request_duration_seconds_bucket{{le=\"+Inf\"}} {count}\npanggung_http_request_duration_seconds_count {count}\npanggung_http_request_duration_seconds_sum {:.6}",
        metrics.duration_micros as f64 / 1_000_000.0
    );
    let _ = writeln!(
        output,
        "# HELP panggung_operations_total Completed or abandoned operations.\n# TYPE panggung_operations_total counter"
    );
    for kind in [Operation::Webhook, Operation::Publish] {
        for outcome in [
            OperationOutcome::Success,
            OperationOutcome::Rejected,
            OperationOutcome::Failure,
        ] {
            let _ = writeln!(
                output,
                "panggung_operations_total{{operation=\"{}\",outcome=\"{}\"}} {}",
                kind.name(),
                outcome.name(),
                operations[kind as usize][outcome as usize]
            );
        }
    }
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        output,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    #[derive(Debug, Clone, Default)]
    struct CaptureSpans(Arc<Mutex<Vec<opentelemetry_sdk::trace::SpanData>>>);
    impl opentelemetry_sdk::trace::SpanExporter for CaptureSpans {
        async fn export(
            &self,
            batch: Vec<opentelemetry_sdk::trace::SpanData>,
        ) -> opentelemetry_sdk::error::OTelSdkResult {
            self.0.lock().unwrap().extend(batch);
            Ok(())
        }
    }
    #[derive(Clone, Default)]
    struct CaptureMetrics(Arc<Mutex<Vec<(String, u64)>>>);
    impl opentelemetry_sdk::metrics::exporter::PushMetricExporter for CaptureMetrics {
        async fn export(
            &self,
            metrics: &opentelemetry_sdk::metrics::data::ResourceMetrics,
        ) -> opentelemetry_sdk::error::OTelSdkResult {
            use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
            let mut captured = self.0.lock().unwrap();
            for metric in metrics.scope_metrics().flat_map(|scope| scope.metrics()) {
                let count = match metric.data() {
                    AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                        sum.data_points().map(|p| p.value()).sum()
                    }
                    AggregatedMetrics::F64(MetricData::Histogram(hist)) => {
                        hist.data_points().map(|p| p.count()).sum()
                    }
                    _ => continue,
                };
                captured.push((metric.name().to_owned(), count));
            }
            Ok(())
        }
        fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
            Ok(())
        }
        fn shutdown_with_timeout(
            &self,
            _: std::time::Duration,
        ) -> opentelemetry_sdk::error::OTelSdkResult {
            Ok(())
        }
        fn temporality(&self) -> opentelemetry_sdk::metrics::Temporality {
            opentelemetry_sdk::metrics::Temporality::Cumulative
        }
    }
    #[tokio::test]
    async fn operations_emit_child_spans_and_scrape_counters() {
        let captured = CaptureSpans::default();
        let captured_metrics = CaptureMetrics::default();
        let telemetry = Telemetry::from_providers(
            SdkMeterProvider::builder()
                .with_periodic_exporter(captured_metrics.clone())
                .build(),
            SdkTracerProvider::builder()
                .with_simple_exporter(captured.clone())
                .build(),
        );
        let metrics = Arc::new(Metrics {
            telemetry,
            ..Metrics::default()
        });
        let app = instrument(
            Router::new().route(
                "/",
                get(
                    |Extension(metrics): Extension<Arc<Metrics>>,
                     Extension(parent): Extension<RequestTrace>| async move {
                        metrics
                            .operation(Operation::Webhook, Some(&parent))
                            .finish(OperationOutcome::Rejected);
                        metrics
                            .operation(Operation::Publish, Some(&parent))
                            .finish(OperationOutcome::Success);
                        drop(metrics.operation(Operation::Publish, Some(&parent)));
                        StatusCode::OK
                    },
                ),
            ),
            metrics.clone(),
        );
        assert_eq!(
            app.oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let spans = captured.0.lock().unwrap();
        let request = spans
            .iter()
            .find(|span| span.name == "http.request")
            .unwrap();
        let children: Vec<_> = spans
            .iter()
            .filter(|span| span.name.starts_with("panggung."))
            .collect();
        assert_eq!(children.len(), 3);
        for child in &children {
            assert_eq!(child.parent_span_id, request.span_context.span_id());
            assert_eq!(
                child.span_context.trace_id(),
                request.span_context.trace_id()
            );
        }
        assert!(
            children
                .iter()
                .any(|s| s.name == "panggung.publish" && matches!(s.status, Status::Error { .. }))
        );
        let (_, body) = render_metrics(&metrics);
        assert!(
            body.contains(
                "panggung_operations_total{operation=\"webhook\",outcome=\"rejected\"} 1"
            )
        );
        assert!(
            body.contains("panggung_operations_total{operation=\"publish\",outcome=\"success\"} 1")
        );
        assert!(
            body.contains("panggung_operations_total{operation=\"publish\",outcome=\"failure\"} 1")
        );
        metrics.telemetry._meter_provider.force_flush().unwrap();
        let emitted = captured_metrics.0.lock().unwrap();
        for (name, count) in [
            ("http.server.request.count", 1),
            ("http.server.request.duration", 1),
            ("panggung.operation.count", 3),
            ("panggung.operation.duration", 3),
        ] {
            assert!(
                emitted.contains(&(name.to_owned(), count)),
                "missing emitted metric {name}"
            );
        }
    }
    #[test]
    fn concurrent_snapshots_are_consistent() {
        let metrics = Metrics::default();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..1000 {
                        metrics.record(1000, true);
                    }
                });
            }
            for _ in 0..1000 {
                let s = metrics.read();
                assert_eq!(s.requests, s.failures);
                assert_eq!(s.duration_micros, s.requests * 1000);
                assert_eq!(s.buckets, [s.requests; 8]);
            }
        });
        assert_eq!(metrics.read().requests, 4000);
    }
    #[tokio::test]
    async fn unattended_scrape_is_opt_in_and_authenticated() {
        let token = "synthetic-test-token-01234567890123456789";
        let disabled = scrape_routes(Arc::new(Metrics::default()));
        let request = || Request::builder().uri("/metrics");
        let response = disabled
            .oneshot(request().body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let metrics = Arc::new(Metrics {
            scrape_token: Some(Sha256::digest(token.as_bytes()).into()),
            ..Metrics::default()
        });
        metrics.record(1000, true);
        let app = scrape_routes(metrics);
        for authorization in ["Bearer wrong", "Basic wrong", ""] {
            let response = app
                .clone()
                .oneshot(
                    request()
                        .header(AUTHORIZATION, authorization)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = app
            .oneshot(
                request()
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        assert!(
            std::str::from_utf8(&body)
                .unwrap()
                .contains("panggung_http_requests_total 1")
        );
    }
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
        assert_eq!(metrics.read().requests, 1);
        assert_eq!(metrics.read().failures, 1);
    }
}
