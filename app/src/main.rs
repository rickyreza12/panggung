mod auth;
mod catalog;
mod catalog_relations;
mod catalog_repository;
mod config;
mod database;
pub mod identity;
mod organization;
mod storage;

use axum::{
    Router,
    extract::{DefaultBodyLimit, State},
    http::{StatusCode, header},
    response::Html,
    routing::get,
};
use catalog_repository::{CatalogState, SqliteCatalogRepository};
use std::{net::Ipv4Addr, sync::Arc};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let config = config::Config::load()?;
    std::fs::create_dir_all(&config.data_dir)?;
    let pool = match config.database_provider {
        config::DatabaseProvider::Sqlite => {
            database::open(&config.data_dir.join("panggung.db")).await?
        }
    };
    let identity = identity::IdentityStore::new(pool.clone());
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        if let [command, username] = args.as_slice()
            && command == "bootstrap-admin"
        {
            use std::io::IsTerminal;
            if !std::io::stdin().is_terminal() {
                return Err("Bootstrap requires an interactive terminal".into());
            }
            let password = rpassword::prompt_password("Password: ")?;
            let confirmation = rpassword::prompt_password("Confirm password: ")?;
            if password != confirmation {
                return Err("Passwords do not match".into());
            }
            identity.bootstrap_admin(username, &password).await?;
            println!("Initial administrator created");
            return Ok(());
        }
        return Err("Usage: panggung [bootstrap-admin <username>]".into());
    }
    let storage = match config.storage_provider {
        config::ObjectStorageProvider::Local => {
            storage::LocalStorage::open(config.data_dir.join("objects"))?
        }
    };
    storage::validate_capabilities(&storage)?;
    let organization_routes = organization::routes(pool.clone());
    let repository: CatalogState = Arc::new(SqliteCatalogRepository::new(pool));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port)).await?;
    let auth = auth::AuthState::new(identity, listener.local_addr()?.port());
    println!("Panggung listening on http://{}", listener.local_addr()?);
    axum::serve(
        listener,
        production_router(repository, storage.routes(), organization_routes, auth),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

#[cfg(test)]
fn router(pool: sqlx::SqlitePool) -> Router {
    let repository: CatalogState = Arc::new(SqliteCatalogRepository::new(pool));
    public_router(repository.clone()).merge(repository_router(repository))
}

fn production_router(
    repository: CatalogState,
    storage: Router,
    organization: Router,
    auth: auth::AuthState,
) -> Router {
    let protected = auth::protect(
        repository_router(repository.clone())
            .merge(storage)
            .merge(organization),
        auth.clone(),
    );
    public_router(repository)
        .merge(auth::routes(auth))
        .merge(protected)
}

fn public_router(repository: CatalogState) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async {
                (
                    [
                        (
                            header::CONTENT_SECURITY_POLICY,
                            "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
                        ),
                        (header::CACHE_CONTROL, "no-cache"),
                        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                    ],
                    Html(include_str!("web/index.html")),
                )
            }),
        )
        .route(
            "/assets/app.js",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "application/javascript; charset=utf-8"),
                        (header::CACHE_CONTROL, "no-cache"),
                        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                    ],
                    include_str!("web/assets/app.js"),
                )
            }),
        )
        .route(
            "/assets/index.css",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "text/css; charset=utf-8"),
                        (header::CACHE_CONTROL, "no-cache"),
                        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                    ],
                    include_str!("web/assets/index.css"),
                )
            }),
        )
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(ready))
        .with_state(repository)
}

fn repository_router(repository: CatalogState) -> Router {
    Router::new()
        .route("/api/components", get(catalog::list).post(catalog::create))
        .route(
            "/api/catalog/import",
            axum::routing::post(catalog::import).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/api/catalog/import/yaml",
            axum::routing::post(catalog::import_yaml).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route("/api/catalog/relations/{name}", get(catalog::relations))
        .route(
            "/api/catalog/relations/{namespace}/{name}",
            get(catalog::relations_qualified),
        )
        .route(
            "/api/components/{name}",
            get(catalog::get_one)
                .put(catalog::update)
                .delete(catalog::remove),
        )
        .route(
            "/api/components/{namespace}/{name}",
            get(catalog::get_qualified)
                .put(catalog::update_qualified)
                .delete(catalog::remove_qualified),
        )
        .with_state(repository)
}

async fn ready(State(repository): State<CatalogState>) -> (StatusCode, &'static str) {
    match repository.ready().await {
        Ok(()) => (StatusCode::OK, "ready"),
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
    async fn production_routes_enforce_sessions_roles_and_origin() {
        use rand_core::{OsRng, RngCore};
        let dir = tempfile::tempdir().unwrap();
        let pool = database::open(&dir.path().join("auth.db")).await.unwrap();
        let identity = identity::IdentityStore::new(pool.clone());
        let mut random = [0_u8; 32];
        OsRng.fill_bytes(&mut random);
        let password: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let admin = identity.bootstrap_admin("admin", &password).await.unwrap();
        identity
            .create_user(admin.id, "viewer", &password, identity::Role::Viewer)
            .await
            .unwrap();
        identity
            .create_user(admin.id, "member", &password, identity::Role::Member)
            .await
            .unwrap();
        let app = production_router(
            Arc::new(SqliteCatalogRepository::new(pool.clone())),
            storage::LocalStorage::open(dir.path().join("objects"))
                .unwrap()
                .routes(),
            organization::routes(pool),
            auth::AuthState::new(identity, 3000),
        );
        let request = |method: &str, uri: &str, cookie: &str, origin: &str, body: String| {
            Request::builder()
                .method(method)
                .uri(uri)
                .header("host", "127.0.0.1:3000")
                .header("origin", origin)
                .header("cookie", cookie)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap()
        };
        let origin = "http://127.0.0.1:3000";
        let object = "/api/objects/0123456789abcdef0123456789abcdef";
        for (method, path) in [
            ("GET", "/api/components"),
            ("POST", "/api/components"),
            ("GET", "/api/components/default/service"),
            ("PUT", "/api/components/service"),
            ("DELETE", "/api/components/default/service"),
            ("POST", "/api/catalog/import"),
            ("POST", "/api/catalog/import/yaml"),
            ("GET", "/api/catalog/relations/service"),
            ("GET", "/api/catalog/relations/default/service"),
            ("GET", object),
            ("PUT", object),
            ("GET", "/api/auth/me"),
        ] {
            let response = app
                .clone()
                .oneshot(request(method, path, "", origin, String::new()))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
        let login_body = |username: &str| {
            serde_json::json!({"username":username,"password":password}).to_string()
        };
        let cross = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/auth/login",
                "",
                "http://example.invalid",
                login_body("admin"),
            ))
            .await
            .unwrap();
        assert_eq!(cross.status(), StatusCode::FORBIDDEN);
        let mut cookies = Vec::new();
        for username in ["admin", "member", "viewer"] {
            let response = app
                .clone()
                .oneshot(request(
                    "POST",
                    "/api/auth/login",
                    "",
                    origin,
                    login_body(username),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let cookie = response.headers()["set-cookie"].to_str().unwrap();
            assert!(
                cookie.contains("HttpOnly")
                    && cookie.contains("SameSite=Strict")
                    && cookie.contains("Max-Age=86400")
                    && !cookie.contains("Domain=")
            );
            cookies.push(cookie.split(';').next().unwrap().to_owned());
            let body = to_bytes(response.into_body(), 4096).await.unwrap();
            let user: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(user["username"], username);
            assert_eq!(user["role"], username);
            assert!(user.get("password_hash").is_none() && user.get("token").is_none());
        }
        let component =
            serde_json::json!({"name":"service","description":"demo","owner":"team"}).to_string();
        for (index, method, path, body, expected) in [
            (2, "GET", "/api/components", String::new(), StatusCode::OK),
            (
                2,
                "POST",
                "/api/components",
                component.clone(),
                StatusCode::FORBIDDEN,
            ),
            (2, "PUT", object, "demo".into(), StatusCode::FORBIDDEN),
            (
                1,
                "POST",
                "/api/components",
                component.clone(),
                StatusCode::CREATED,
            ),
            (0, "PUT", object, "demo".into(), StatusCode::CREATED),
            (2, "GET", object, String::new(), StatusCode::OK),
        ] {
            let response = app
                .clone()
                .oneshot(request(method, path, &cookies[index], origin, body))
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{method} {path}");
        }
        let response = app
            .clone()
            .oneshot(request(
                "DELETE",
                "/api/components/service",
                &cookies[0],
                "http://example.invalid",
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/auth/me",
                &cookies[2],
                origin,
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/auth/logout",
                &cookies[2],
                origin,
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            response.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
        let response = app
            .oneshot(request(
                "GET",
                "/api/components",
                &cookies[2],
                origin,
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

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
    async fn yaml_http_import_preserves_descriptor_and_enforces_body_limit() {
        let dir = tempfile::tempdir().unwrap();
        let app = router(database::open(&dir.path().join("test.db")).await.unwrap());
        let yaml = "apiVersion: backstage.io/v1alpha1\nkind: Component\nmetadata:\n  name: billing\n  tags: [finance, rust]\nspec:\n  type: service\n  lifecycle: production\n  owner: team-a\n";
        let imported = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/catalog/import/yaml")
                    .header("content-type", "application/yaml")
                    .body(Body::from(yaml))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(imported.status(), StatusCode::CREATED);
        let (status, result) = call(
            &app,
            "GET",
            "/api/components/billing",
            serde_json::Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            result["descriptor"]["metadata"]["tags"],
            serde_json::json!(["finance", "rust"])
        );
        assert_eq!(result["component_type"], "service");
        assert_eq!(result["lifecycle"], "production");

        let oversized = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/catalog/import/yaml")
                    .body(Body::from(vec![b'a'; 64 * 1024 + 1]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn qualified_namespace_routes_isolate_same_named_components() {
        let dir = tempfile::tempdir().unwrap();
        let app = router(database::open(&dir.path().join("test.db")).await.unwrap());
        let mut descriptor = serde_json::json!({
            "apiVersion": "backstage.io/v1alpha1",
            "kind": "Component",
            "metadata": {"name": "shared"},
            "spec": {"type": "service", "lifecycle": "production", "owner": "default-team"}
        });
        assert_eq!(
            call(&app, "POST", "/api/catalog/import", descriptor.clone())
                .await
                .0,
            StatusCode::CREATED
        );
        descriptor["metadata"]["namespace"] = "team-x".into();
        descriptor["spec"]["owner"] = "team-x-owner".into();
        assert_eq!(
            call(&app, "POST", "/api/catalog/import", descriptor.clone())
                .await
                .0,
            StatusCode::CREATED
        );
        assert_eq!(
            call(&app, "POST", "/api/catalog/import", descriptor)
                .await
                .0,
            StatusCode::CONFLICT
        );
        let (_, default) = call(
            &app,
            "GET",
            "/api/components/shared",
            serde_json::Value::Null,
        )
        .await;
        let (_, qualified) = call(
            &app,
            "GET",
            "/api/components/team-x/shared",
            serde_json::Value::Null,
        )
        .await;
        assert_eq!(default["owner"], "default-team");
        assert_eq!(qualified["owner"], "team-x-owner");
        let (status, filtered) = call(
            &app,
            "GET",
            "/api/components?namespace=team-x&limit=1",
            serde_json::Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(filtered.as_array().unwrap().len(), 1);
        assert_eq!(filtered[0]["namespace"], "team-x");
        let update = serde_json::json!({
            "name": "shared", "description": "New", "owner": "team-x-updated"
        });
        assert_eq!(
            call(&app, "PUT", "/api/components/team-x/shared", update)
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            call(
                &app,
                "DELETE",
                "/api/components/team-x/shared",
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
                "/api/components/shared",
                serde_json::Value::Null
            )
            .await
            .0,
            StatusCode::OK
        );
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
        invalid["metadata"]["namespace"] = "Other".into();
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
