mod catalog;
mod config;
mod database;

use axum::{Router, extract::State, http::StatusCode, response::Html, routing::get};
use std::net::Ipv4Addr;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let config = config::Config::load()?;
    std::fs::create_dir_all(&config.data_dir)?;
    let pool = database::open(&config.data_dir.join("panggung.db")).await?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port)).await?;
    println!("Panggung listening on http://{}", listener.local_addr()?);
    axum::serve(listener, router(pool))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

fn router(pool: sqlx::SqlitePool) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("index.html")) }))
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(ready))
        .route("/api/components", get(catalog::list).post(catalog::create))
        .with_state(pool)
}

async fn ready(State(pool): State<sqlx::SqlitePool>) -> (StatusCode, &'static str) {
    match sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&pool)
        .await
    {
        Ok(1) => (StatusCode::OK, "ready"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "database unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn catalog_round_trip_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let pool = database::open(&dir.path().join("test.db")).await.unwrap();
        let app = router(pool);
        let request = |name: &str| {
            Request::builder()
                .method("POST")
                .uri("/api/components")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"name":name,"description":"demo","owner":"platform"})
                        .to_string(),
                ))
                .unwrap()
        };
        assert_eq!(
            app.clone()
                .oneshot(request("payment-service"))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );
        assert_eq!(
            app.clone()
                .oneshot(request("payment-service"))
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            app.clone()
                .oneshot(request("../bad"))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/components")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        let components: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(components.as_array().unwrap().len(), 1);
        assert_eq!(components[0]["name"], "payment-service");
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
