mod catalog;

use axum::{Router, response::Html, routing::get};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::{net::Ipv4Addr, path::PathBuf};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let port = std::env::var("PANGGUNG_PORT")
        .unwrap_or_else(|_| "3000".to_owned())
        .parse::<u16>()?;
    let data =
        PathBuf::from(std::env::var_os("PANGGUNG_DATA_DIR").unwrap_or_else(|| "data".into()));
    std::fs::create_dir_all(&data)?;
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(data.join("panggung.db"))
                .create_if_missing(true)
                .foreign_keys(true)
                .journal_mode(SqliteJournalMode::Wal)
                .busy_timeout(std::time::Duration::from_secs(5)),
        )
        .await?;
    sqlx::migrate!().run(&pool).await?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
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
        .route("/api/components", get(catalog::list).post(catalog::create))
        .with_state(pool)
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
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(dir.path().join("test.db"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
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
