use crate::identity::{Role, User};
use axum::{
    Extension, Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

const MAX_OFFSET: u32 = 10_000;
const MAX_LIMIT: u32 = 50;
type ApiError = (StatusCode, &'static str);

#[derive(Deserialize)]
struct Filter {
    q: String,
    #[serde(default)]
    offset: u32,
    #[serde(default = "default_limit")]
    limit: u32,
}
fn default_limit() -> u32 {
    20
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct Hit {
    namespace: String,
    name: String,
    description: String,
    owner: String,
    component_type: Option<String>,
    lifecycle: Option<String>,
}
#[derive(Serialize)]
struct Page {
    items: Vec<Hit>,
    has_more: bool,
}

pub fn routes(pool: SqlitePool) -> Router {
    Router::new()
        .route("/api/search", get(search))
        .route("/api/search/rebuild", post(rebuild))
        .with_state(pool)
}

fn internal(_: sqlx::Error) -> ApiError {
    tracing::error!("Component search database operation failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "Search operation failed")
}

// Treat all alphanumeric runs as literal tokens, never as FTS operators.
fn literal_query(input: &str) -> Result<String, ApiError> {
    let invalid = (
        StatusCode::BAD_REQUEST,
        "Search requires 1–16 words and at most 200 bytes",
    );
    if input.len() > 200 || input.chars().any(char::is_control) {
        return Err(invalid);
    }
    let terms: Vec<_> = input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .collect();
    if terms.is_empty() || terms.len() > 16 {
        return Err(invalid);
    }
    Ok(terms
        .iter()
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>()
        .join(" AND "))
}

async fn search(
    State(pool): State<SqlitePool>,
    Extension(_user): Extension<User>,
    Query(filter): Query<Filter>,
) -> Result<Json<Page>, ApiError> {
    if filter.offset > MAX_OFFSET || !(1..=MAX_LIMIT).contains(&filter.limit) {
        return Err((StatusCode::BAD_REQUEST, "Invalid search pagination"));
    }
    let query = literal_query(&filter.q)?;
    let mut items = sqlx::query_as::<_, Hit>(
        "SELECT c.namespace,c.name,c.description,c.owner,c.component_type,c.lifecycle
         FROM component_search JOIN components c ON c.rowid = component_search.rowid
         WHERE component_search MATCH ?
         ORDER BY bm25(component_search, 8.0, 3.0, 1.0, 2.0, 1.0, 1.0), c.namespace, c.name
         LIMIT ? OFFSET ?",
    )
    .bind(query)
    .bind(filter.limit + 1)
    .bind(filter.offset)
    .fetch_all(&pool)
    .await
    .map_err(internal)?;
    let has_more =
        items.len() > filter.limit as usize && filter.offset + filter.limit <= MAX_OFFSET;
    items.truncate(filter.limit as usize);
    Ok(Json(Page { items, has_more }))
}

async fn rebuild(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
) -> Result<StatusCode, ApiError> {
    if user.role != Role::Admin {
        return Err((StatusCode::FORBIDDEN, "Administrator required"));
    }
    let mut transaction = pool.begin().await.map_err(internal)?;
    sqlx::query("DELETE FROM component_search")
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
    sqlx::query("INSERT INTO component_search(rowid,name,namespace,description,owner,component_type,lifecycle) SELECT rowid,name,namespace,description,owner,component_type,lifecycle FROM components")
        .execute(&mut *transaction).await.map_err(internal)?;
    sqlx::query(
        "INSERT INTO identity_audit_events(actor_id,action) VALUES (?, 'rebuild_component_search')",
    )
    .bind(user.id)
    .execute(&mut *transaction)
    .await
    .map_err(internal)?;
    transaction.commit().await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::{self, AuthState},
        identity::IdentityStore,
    };
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use rand_core::{OsRng, RngCore};
    use tower::ServiceExt;

    #[test]
    fn query_operators_are_literal_and_bounded() {
        assert_eq!(
            literal_query("billing OR name:other*").unwrap(),
            "\"billing\" AND \"OR\" AND \"name\" AND \"other\""
        );
        assert!(literal_query("*").is_err());
        assert!(literal_query(&"a ".repeat(17)).is_err());
        assert!(literal_query(&"a".repeat(201)).is_err());
        assert!(literal_query("bad\0").is_err());
    }

    #[tokio::test]
    async fn migration_backfills_existing_rows_and_triggers_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("search.db");
        // Apply the actual pre-search migration files, then upgrade through the normal opener.
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let mut migrator = sqlx::migrate!();
        migrator
            .migrations
            .to_mut()
            .retain(|migration| migration.version < 9);
        migrator.run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO components(name,description,owner) VALUES ('legacy','original','team')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        let pool = crate::database::open(&path).await.unwrap();
        let count = |pool: SqlitePool, term: &'static str| async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM component_search WHERE component_search MATCH ?",
            )
            .bind(term)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        assert_eq!(count(pool.clone(), "original").await, 1);
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO components(name,description,owner) VALUES ('new','rolledback','team')",
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.rollback().await.unwrap();
        assert_eq!(count(pool.clone(), "rolledback").await, 0);
        sqlx::query("UPDATE components SET description='updated' WHERE name='legacy'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(count(pool.clone(), "original").await, 0);
        assert_eq!(count(pool.clone(), "updated").await, 1);
        pool.close().await;
        let pool = crate::database::open(&path).await.unwrap();
        assert_eq!(count(pool.clone(), "updated").await, 1);
        sqlx::query("DELETE FROM components WHERE name='legacy'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(count(pool.clone(), "updated").await, 0);
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        token: &str,
    ) -> (StatusCode, serde_json::Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("host", "127.0.0.1:3000")
                    .header("origin", "http://127.0.0.1:3000")
                    .header("cookie", format!("panggung_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    #[tokio::test]
    async fn authenticated_search_ranks_pages_and_admin_rebuilds() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::database::open(&dir.path().join("search.db"))
            .await
            .unwrap();
        let identity = IdentityStore::new(pool.clone());
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let password: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
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
        sqlx::query("INSERT INTO components(name,description,owner) VALUES ('billing','','team'),('service','billing','team')").execute(&pool).await.unwrap();
        let app = auth::protect(routes(pool.clone()), AuthState::new(identity, 3000));
        let uri = "/api/search?q=billing&limit=1";
        assert_eq!(call(&app, "GET", uri, "").await.0, StatusCode::UNAUTHORIZED);
        for token in [member_token.as_str(), viewer_token.as_str()] {
            let (status, page) = call(&app, "GET", uri, token).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(page["items"][0]["name"], "billing");
            assert_eq!(page["has_more"], true);
            assert_eq!(
                call(&app, "POST", "/api/search/rebuild", token).await.0,
                StatusCode::FORBIDDEN
            );
        }
        let token = admin_token.as_str();
        assert_eq!(
            call(&app, "GET", &format!("{uri}&offset=1"), token).await.1["items"][0]["name"],
            "service"
        );
        assert_eq!(
            call(&app, "GET", "/api/search?q=billing&limit=51", token)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(&app, "GET", "/api/search?q=billing%20OR%20service", token)
                .await
                .1["items"],
            serde_json::json!([])
        );
        sqlx::query("DELETE FROM component_search")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            call(&app, "POST", "/api/search/rebuild", token).await.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(&app, "GET", uri, token).await.1["items"][0]["name"],
            "billing"
        );
        let actor: i64 = sqlx::query_scalar(
            "SELECT actor_id FROM identity_audit_events WHERE action='rebuild_component_search'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(actor, admin.id);
    }
}
