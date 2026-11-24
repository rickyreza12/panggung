mod catalog;
mod config;
mod database;
mod storage;

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
    let storage = storage::LocalStorage::open(config.data_dir.join("objects"))?;
    let pool = database::open(&config.data_dir.join("panggung.db")).await?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port)).await?;
    println!("Panggung listening on http://{}", listener.local_addr()?);
    axum::serve(listener, router(pool).merge(storage.routes()))
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
        .route("/api/catalog/import", axum::routing::post(catalog::import))
        .route(
            "/api/components/{name}",
            get(catalog::get_one)
                .put(catalog::update)
                .delete(catalog::remove),
        )
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

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn catalog_import_filter_edit_delete() {
        let dir = tempfile::tempdir().unwrap();
        let app = router(database::open(&dir.path().join("test.db")).await.unwrap());
        let descriptor = serde_json::json!({
            "apiVersion":"backstage.io/v1alpha1","kind":"Component",
            "metadata":{"name":"import","description":"Imported service"},
            "spec":{"type":"service","lifecycle":"experimental","owner":"group:default/platform"}
        });
        let (status, _) = call(&app, "POST", "/api/catalog/import", descriptor.clone()).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            call(&app, "POST", "/api/catalog/import", descriptor.clone())
                .await
                .0,
            StatusCode::CONFLICT
        );
        let mut invalid = descriptor;
        invalid["metadata"]["namespace"] = "other".into();
        assert_eq!(
            call(&app, "POST", "/api/catalog/import", invalid).await.0,
            StatusCode::BAD_REQUEST
        );
        let (status, result) = call(
            &app,
            "GET",
            "/api/components/import",
            serde_json::Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["owner"], "group:default/platform");
        let update = serde_json::json!({"name":"import","description":"Updated","owner":"team-b"});
        assert_eq!(
            call(&app, "PUT", "/api/components/import", update).await.0,
            StatusCode::OK
        );
        let (_, result) = call(
            &app,
            "GET",
            "/api/components?q=TEAM-B",
            serde_json::Value::Null,
        )
        .await;
        assert_eq!(result.as_array().unwrap().len(), 1);
        let (_, result) = call(
            &app,
            "GET",
            "/api/components?q=TEAM-B&offset=1",
            serde_json::Value::Null,
        )
        .await;
        assert!(result.as_array().unwrap().is_empty());
        assert_eq!(
            call(
                &app,
                "DELETE",
                "/api/components/import",
                serde_json::Value::Null
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(
                &app,
                "GET",
                "/api/components/import",
                serde_json::Value::Null
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }

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
