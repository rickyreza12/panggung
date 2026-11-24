use crate::identity::{Role, User};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    routing::get,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

type ApiError = (StatusCode, &'static str);

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
struct Domain {
    namespace: String,
    name: String,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
struct System {
    namespace: String,
    name: String,
    domain_namespace: Option<String>,
    domain_name: Option<String>,
}

pub fn routes(pool: SqlitePool) -> Router {
    Router::new()
        .route(
            "/api/catalog/domains",
            get(list_domains)
                .post(create_domain)
                .layer(DefaultBodyLimit::max(4096)),
        )
        .route(
            "/api/catalog/systems",
            get(list_systems)
                .post(create_system)
                .layer(DefaultBodyLimit::max(4096)),
        )
        .with_state(pool)
}

fn check_writer(user: &User) -> Result<(), ApiError> {
    if user.role == Role::Viewer {
        return Err((StatusCode::FORBIDDEN, "Write permission required"));
    }
    Ok(())
}

fn valid_part(value: &str) -> bool {
    (1..=63).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
}

fn validate(namespace: &str, name: &str) -> Result<(), ApiError> {
    if !valid_part(namespace) || !valid_part(name) {
        return Err((StatusCode::BAD_REQUEST, "Invalid namespace or name"));
    }
    Ok(())
}

fn database_error(error: sqlx::Error) -> ApiError {
    if let sqlx::Error::Database(ref database) = error {
        if database.is_unique_violation() {
            return (StatusCode::CONFLICT, "Entity already exists");
        }
        if database.is_foreign_key_violation() {
            return (StatusCode::NOT_FOUND, "Domain not found");
        }
    }
    tracing::error!("Catalog structure database operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Database operation failed",
    )
}

async fn list_domains(
    State(pool): State<SqlitePool>,
    Extension(_user): Extension<User>,
) -> Result<Json<Vec<Domain>>, ApiError> {
    sqlx::query_as("SELECT namespace, name FROM catalog_domains ORDER BY namespace, name LIMIT 500")
        .fetch_all(&pool)
        .await
        .map(Json)
        .map_err(database_error)
}

async fn create_domain(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
    Json(domain): Json<Domain>,
) -> Result<(StatusCode, Json<Domain>), ApiError> {
    check_writer(&user)?;
    validate(&domain.namespace, &domain.name)?;
    sqlx::query("INSERT INTO catalog_domains (namespace, name) VALUES (?, ?)")
        .bind(&domain.namespace)
        .bind(&domain.name)
        .execute(&pool)
        .await
        .map_err(database_error)?;
    Ok((StatusCode::CREATED, Json(domain)))
}

async fn list_systems(
    State(pool): State<SqlitePool>,
    Extension(_user): Extension<User>,
) -> Result<Json<Vec<System>>, ApiError> {
    sqlx::query_as("SELECT namespace, name, domain_namespace, domain_name FROM catalog_systems ORDER BY namespace, name LIMIT 500")
        .fetch_all(&pool).await.map(Json).map_err(database_error)
}

async fn create_system(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
    Json(system): Json<System>,
) -> Result<(StatusCode, Json<System>), ApiError> {
    check_writer(&user)?;
    validate(&system.namespace, &system.name)?;
    match (&system.domain_namespace, &system.domain_name) {
        (Some(namespace), Some(name)) => validate(namespace, name)?,
        (None, None) => {}
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Domain namespace and name must be supplied together",
            ));
        }
    }
    sqlx::query("INSERT INTO catalog_systems (namespace, name, domain_namespace, domain_name) VALUES (?, ?, ?, ?)")
        .bind(&system.namespace).bind(&system.name).bind(&system.domain_namespace).bind(&system.domain_name)
        .execute(&pool).await.map_err(database_error)?;
    Ok((StatusCode::CREATED, Json(system)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::{self, AuthState},
        identity::IdentityStore,
    };
    use axum::{body::Body, http::Request};
    use rand_core::{OsRng, RngCore};
    use tower::ServiceExt;

    async fn call(app: &Router, method: &str, path: &str, token: &str, body: &str) -> StatusCode {
        app.clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "127.0.0.1:3000")
                    .header("origin", "http://127.0.0.1:3000")
                    .header("cookie", format!("panggung_session={token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn structure_routes_enforce_roles_and_domain_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::database::open(&dir.path().join("structure.db"))
            .await
            .unwrap();
        let identity = IdentityStore::new(pool.clone());
        let mut random = [0u8; 32];
        OsRng.fill_bytes(&mut random);
        let password: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let admin = identity.bootstrap_admin("admin", &password).await.unwrap();
        identity
            .create_user(admin.id, "viewer", &password, Role::Viewer)
            .await
            .unwrap();
        let admin_token = identity.login("admin", &password).await.unwrap();
        let viewer_token = identity.login("viewer", &password).await.unwrap();
        let app = auth::protect(routes(pool.clone()), AuthState::new(identity, 3000));
        let domains = "/api/catalog/domains";
        let systems = "/api/catalog/systems";
        let domain = r#"{"namespace":"default","name":"payments"}"#;
        let system = r#"{"namespace":"platform","name":"billing","domain_namespace":"default","domain_name":"payments"}"#;
        assert_eq!(
            call(&app, "GET", domains, "", "").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "GET", domains, viewer_token.as_str(), "").await,
            StatusCode::OK
        );
        assert_eq!(
            call(&app, "POST", domains, viewer_token.as_str(), domain).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "POST", systems, admin_token.as_str(), system).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&app, "POST", domains, admin_token.as_str(), domain).await,
            StatusCode::CREATED
        );
        assert_eq!(
            call(&app, "POST", domains, admin_token.as_str(), domain).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            call(&app, "POST", systems, admin_token.as_str(), system).await,
            StatusCode::CREATED
        );
        let domain: String = sqlx::query_scalar("SELECT domain_name FROM catalog_systems WHERE namespace = 'platform' AND name = 'billing'").fetch_one(&pool).await.unwrap();
        assert_eq!(domain, "payments");
    }
}
