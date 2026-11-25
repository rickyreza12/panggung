# Optional monitoring

The existing administrator diagnostics endpoint remains available. Set
`PANGGUNG_METRICS_TOKEN` to a randomly generated secret with at least 32
printable non-space ASCII characters to enable `GET /metrics`. Configure
Prometheus with bearer authorization using a protected credentials file.
The service remains bound to loopback; place the scraper on the same host
or use a deliberately configured authenticated transport.

Example Prometheus job (adjust port and secret file location):

```yaml
scrape_configs:
  - job_name: panggung
    metrics_path: /metrics
    authorization:
      type: Bearer
      credentials_file: /run/secrets/panggung_metrics_token
    static_configs:
      - targets: ['127.0.0.1:3000']
```

Mount the provisioning directory into Grafana's provisioning directory and
the dashboards directory at /var/lib/grafana/dashboards/panggung.
Set PANGGUNG_PROMETHEUS_URL to the reachable Prometheus server URL.

Set PANGGUNG_OTLP_ENABLED=true to enable OpenTelemetry Protocol export using
HTTP protobuf. Configure OTEL_EXPORTER_OTLP_ENDPOINT and standard
OpenTelemetry exporter settings; keep credentials outside source control.
Export is disabled by default. Request metrics include bounded method and
response status attributes. Spans contain method and response status only;
request bodies, raw URLs and credentials are excluded.

Webhook and publish operations expose bounded success, rejection and failure
counts in `panggung_operations_total{operation,outcome}`. OpenTelemetry also
exports `panggung.operation.count` and `panggung.operation.duration` with the
same bounded dimensions; operation spans are children of request spans.
An abandoned operation guard records failure. Invalid optional exporter
configuration or initialization failure disables export and logs a generic
warning; authenticated local metrics remain available.
