use crate::{
    identity::{Role, User},
    storage::{ObjectKey, ObjectStore},
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    routing::get,
};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::sync::Arc;

const MAX_MARKDOWN_BYTES: usize = 64 * 1024;
const MAX_JSON_BYTES: usize = 6 * MAX_MARKDOWN_BYTES + 1024;
type ApiError = (StatusCode, &'static str);

#[derive(Clone)]
struct DocsState {
    pool: SqlitePool,
    store: Arc<dyn ObjectStore>,
}

#[derive(Serialize, sqlx::FromRow)]
struct Version {
    version: i64,
    created_at: i64,
    created_by: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct StoredVersion {
    version: i64,
    created_at: i64,
    created_by: Option<i64>,
    object_key: String,
    content_digest: Vec<u8>,
}

#[derive(Serialize)]
struct Document {
    #[serde(flatten)]
    version: Version,
    markdown: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewDocument {
    markdown: String,
}

pub fn routes(pool: SqlitePool, store: Arc<dyn ObjectStore>) -> Router {
    Router::new()
        .route(
            "/api/techdocs/{namespace}/{name}/versions",
            get(list)
                .post(publish)
                .layer(DefaultBodyLimit::max(MAX_JSON_BYTES)),
        )
        .route(
            "/api/techdocs/{namespace}/{name}/versions/{version}",
            get(read),
        )
        .with_state(DocsState { pool, store })
}

fn internal() -> ApiError {
    tracing::error!("TechDocs operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Documentation operation failed",
    )
}

fn database_error(_: sqlx::Error) -> ApiError {
    internal()
}

fn valid_part(value: &str) -> bool {
    (1..=63).contains(&value.len())
        && value.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
}

fn validate_component(namespace: &str, name: &str) -> Result<(), ApiError> {
    if !valid_part(namespace) || !valid_part(name) {
        return Err((StatusCode::BAD_REQUEST, "Invalid component reference"));
    }
    Ok(())
}

fn validate_markdown(markdown: &str) -> Result<(), ApiError> {
    if markdown.len() > MAX_MARKDOWN_BYTES {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Markdown exceeds 64 KiB"));
    }
    if markdown.trim().is_empty()
        || markdown
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err((StatusCode::BAD_REQUEST, "Invalid Markdown text"));
    }
    Ok(())
}

fn random_key() -> Result<ObjectKey, ApiError> {
    let mut bytes = [0u8; 16];
    OsRng.try_fill_bytes(&mut bytes).map_err(|_| internal())?;
    let key: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    ObjectKey::parse(key).map_err(|_| internal())
}

async fn list(
    State(state): State<DocsState>,
    Extension(_user): Extension<User>,
    Path((namespace, name)): Path<(String, String)>,
) -> Result<Json<Vec<Version>>, ApiError> {
    validate_component(&namespace, &name)?;
    let mut transaction = state.pool.begin().await.map_err(database_error)?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM components WHERE namespace = ? AND name = ?)",
    )
    .bind(&namespace)
    .bind(&name)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    if !exists {
        return Err((StatusCode::NOT_FOUND, "Component not found"));
    }
    let versions = sqlx::query_as("SELECT version, created_at, created_by FROM techdocs_versions WHERE namespace = ? AND component_name = ? ORDER BY version DESC LIMIT 100")
        .bind(namespace).bind(name).fetch_all(&mut *transaction).await.map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(Json(versions))
}

async fn publish(
    State(state): State<DocsState>,
    Extension(user): Extension<User>,
    Path((namespace, name)): Path<(String, String)>,
    Json(input): Json<NewDocument>,
) -> Result<(StatusCode, Json<Version>), ApiError> {
    if !matches!(user.role, Role::Admin | Role::Member) {
        return Err((StatusCode::FORBIDDEN, "Write permission required"));
    }
    validate_component(&namespace, &name)?;
    validate_markdown(&input.markdown)?;
    let key = random_key()?;
    let digest = Sha256::digest(input.markdown.as_bytes()).to_vec();
    let mut transaction = state.pool.begin().await.map_err(database_error)?;
    // First statement writes atomically, serializing version allocation and component deletion.
    let version = sqlx::query_as::<_, Version>(
        "INSERT INTO techdocs_versions (namespace, component_name, version, object_key, content_digest, created_by)
         SELECT ?, ?, COALESCE((SELECT MAX(version) FROM techdocs_versions WHERE namespace = ? AND component_name = ?), 0) + 1, ?, ?, ?
         WHERE EXISTS (SELECT 1 FROM components WHERE namespace = ? AND name = ?)
         RETURNING version, created_at, created_by",
    ).bind(&namespace).bind(&name).bind(&namespace).bind(&name)
        .bind(key.as_str()).bind(digest).bind(user.id).bind(&namespace).bind(&name)
        .fetch_optional(&mut *transaction).await.map_err(database_error)?
        .ok_or((StatusCode::NOT_FOUND, "Component not found"))?;

    sqlx::query("INSERT INTO identity_audit_events (actor_id, action) VALUES (?, ?)")
        .bind(user.id)
        .bind(format!(
            "publish_techdocs:{namespace}/{name}:{}",
            version.version
        ))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;

    let store = state.store;
    let markdown = input.markdown;
    // Metadata is not committed unless immutable object creation succeeds.
    tokio::task::spawn_blocking(move || store.write(&key, markdown.as_bytes()))
        .await
        .map_err(|_| internal())?
        .map_err(|_| internal())?;
    transaction.commit().await.map_err(database_error)?;
    Ok((StatusCode::CREATED, Json(version)))
}

async fn read(
    State(state): State<DocsState>,
    Extension(_user): Extension<User>,
    Path((namespace, name, version)): Path<(String, String, i64)>,
) -> Result<Json<Document>, ApiError> {
    validate_component(&namespace, &name)?;
    if version <= 0 {
        return Err((StatusCode::BAD_REQUEST, "Invalid version"));
    }
    let stored = sqlx::query_as::<_, StoredVersion>(
        "SELECT version, created_at, created_by, object_key, content_digest FROM techdocs_versions WHERE namespace = ? AND component_name = ? AND version = ?",
    ).bind(namespace).bind(name).bind(version)
        .fetch_optional(&state.pool).await.map_err(database_error)?
        .ok_or((StatusCode::NOT_FOUND, "Documentation version not found"))?;
    let key = ObjectKey::parse(stored.object_key).map_err(|_| internal())?;
    let store = state.store;
    let bytes = tokio::task::spawn_blocking(move || store.read(&key))
        .await
        .map_err(|_| internal())?
        .map_err(|_| internal())?;
    if bytes.len() > MAX_MARKDOWN_BYTES || Sha256::digest(&bytes).to_vec() != stored.content_digest
    {
        return Err(internal());
    }
    let markdown = String::from_utf8(bytes).map_err(|_| internal())?;
    validate_markdown(&markdown).map_err(|_| internal())?;
    // Return source as JSON, never execute or render Markdown/HTML.
    Ok(Json(Document {
        version: Version {
            version: stored.version,
            created_at: stored.created_at,
            created_by: stored.created_by,
        },
        markdown,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::{self, AuthState},
        identity::IdentityStore,
        storage::LocalStorage,
    };
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    async fn call(
        app: &Router,
        method: &str,
        path: &str,
        token: &str,
        body: String,
    ) -> (StatusCode, serde_json::Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "127.0.0.1:3000")
                    .header("origin", "http://127.0.0.1:3000")
                    .header("cookie", format!("panggung_session={token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), MAX_JSON_BYTES)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn versions_are_authenticated_immutable_and_persistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("docs.db");
        let root = dir.path().join("docs");
        let pool = crate::database::open(&path).await.unwrap();
        let identity = IdentityStore::new(pool.clone());
        let mut random = [0u8; 32];
        OsRng.fill_bytes(&mut random);
        let password: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let admin = identity.bootstrap_admin("admin", &password).await.unwrap();
        identity
            .create_user(admin.id, "member", &password, Role::Member)
            .await
            .unwrap();
        identity
            .create_user(admin.id, "viewer", &password, Role::Viewer)
            .await
            .unwrap();
        let admin_token = identity.login("admin", &password).await.unwrap();
        let member_token = identity.login("member", &password).await.unwrap();
        let viewer_token = identity.login("viewer", &password).await.unwrap();
        sqlx::query("INSERT INTO components (namespace,name,description,owner) VALUES ('default','service','','team')")
            .execute(&pool).await.unwrap();
        let store = Arc::new(LocalStorage::open(root.clone()).unwrap());
        let app = auth::protect(routes(pool.clone(), store), AuthState::new(identity, 3000));
        let uri = "/api/techdocs/default/service/versions";
        let body = |markdown: &str| serde_json::json!({"markdown":markdown}).to_string();
        let admin = admin_token.as_str();
        let member = member_token.as_str();
        let viewer = viewer_token.as_str();
        assert_eq!(
            call(&app, "GET", uri, "", String::new()).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "POST", uri, viewer, body("# Denied")).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "GET", uri, viewer, String::new()).await.1,
            serde_json::json!([])
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/techdocs/other/service/versions",
                admin,
                body("# Missing")
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&app, "POST", uri, admin, body("")).await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(&app, "POST", uri, admin, body("bad\0text")).await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &app,
                "POST",
                uri,
                admin,
                body(&"a".repeat(MAX_MARKDOWN_BYTES + 1))
            )
            .await
            .0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        let first = "# First\n<script>alert(1)</script>";
        let (status, metadata) = call(&app, "POST", uri, admin, body(first)).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(metadata["version"], 1);
        assert!(metadata.get("object_key").is_none());
        let (second, third) = tokio::join!(
            call(&app, "POST", uri, member, body("# Second")),
            call(&app, "POST", uri, admin, body("# Third")),
        );
        assert_eq!(second.0, StatusCode::CREATED);
        assert_eq!(third.0, StatusCode::CREATED);
        let mut numbers = [
            second.1["version"].as_i64().unwrap(),
            third.1["version"].as_i64().unwrap(),
        ];
        numbers.sort();
        assert_eq!(numbers, [2, 3]);
        let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM identity_audit_events WHERE action LIKE 'publish_techdocs:%' ORDER BY id")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(
            actions,
            [
                "publish_techdocs:default/service:1",
                "publish_techdocs:default/service:2",
                "publish_techdocs:default/service:3"
            ]
        );
        drop(app);
        pool.close().await;
        let pool = crate::database::open(&path).await.unwrap();
        let store = Arc::new(LocalStorage::open(root.clone()).unwrap());
        let app = auth::protect(
            routes(pool.clone(), store),
            AuthState::new(IdentityStore::new(pool.clone()), 3000),
        );
        let (status, document) =
            call(&app, "GET", &format!("{uri}/1"), viewer, String::new()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(document["markdown"], first);
        assert_eq!(
            call(&app, "GET", uri, viewer, String::new())
                .await
                .1
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            call(&app, "GET", &format!("{uri}/99"), viewer, String::new())
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        // A missing/corrupted object is an error, never fabricated content.
        let key: String =
            sqlx::query_scalar("SELECT object_key FROM techdocs_versions WHERE version = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        std::fs::write(root.join(key), b"tampered").unwrap();
        assert_eq!(
            call(&app, "GET", &format!("{uri}/1"), viewer, String::new())
                .await
                .0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    struct FailedStore;

    impl ObjectStore for FailedStore {
        fn capabilities(&self) -> crate::storage::StorageCapabilities {
            crate::storage::StorageCapabilities {
                immutable_create: true,
                max_object_bytes: 1024 * 1024,
            }
        }
        fn read(&self, _: &ObjectKey) -> std::io::Result<Vec<u8>> {
            Err(std::io::Error::other("unavailable"))
        }
        fn write(&self, _: &ObjectKey, _: &[u8]) -> std::io::Result<()> {
            Err(std::io::Error::other("unavailable"))
        }
    }

    #[tokio::test]
    async fn failed_object_write_rolls_back_version_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::database::open(&dir.path().join("docs.db"))
            .await
            .unwrap();
        let identity = IdentityStore::new(pool.clone());
        let mut random = [0u8; 32];
        OsRng.fill_bytes(&mut random);
        let password: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        identity.bootstrap_admin("admin", &password).await.unwrap();
        let token = identity.login("admin", &password).await.unwrap();
        sqlx::query("INSERT INTO components (name,description,owner) VALUES ('service','','team')")
            .execute(&pool)
            .await
            .unwrap();
        let app = auth::protect(
            routes(pool.clone(), Arc::new(FailedStore)),
            AuthState::new(identity, 3000),
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/techdocs/default/service/versions",
                token.as_str(),
                serde_json::json!({"markdown":"# Test"}).to_string()
            )
            .await
            .0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM techdocs_versions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}
