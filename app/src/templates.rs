//! Immutable, declarative software template versions. No execution or file generation.
use crate::identity::{Role, User};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    http::StatusCode,
    routing::get,
};
use serde::{Deserialize, Serialize};
use sqlx::{SqlitePool, types::Json as StoredJson};
use std::collections::HashSet;

type ApiError = (StatusCode, &'static str);
const INVALID: ApiError = (StatusCode::BAD_REQUEST, "Invalid template definition");
const MAX_PARAMETERS: usize = 20;
const MAX_LIMIT: u32 = 100;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ParameterType {
    String,
    Boolean,
    Integer,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Parameter {
    name: String,
    label: String,
    #[serde(rename = "type")]
    parameter_type: ParameterType,
    required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
struct Template {
    name: String,
    version: String,
    title: String,
    description: String,
    stack: String,
    parameters: StoredJson<Vec<Parameter>>,
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    offset: u32,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    50
}

pub fn routes(pool: SqlitePool) -> Router {
    Router::new()
        .route(
            "/api/templates",
            get(list)
                .post(create)
                .layer(DefaultBodyLimit::max(32 * 1024)),
        )
        .route("/api/templates/{name}/{version}", get(detail))
        .with_state(pool)
}

fn database_error(error: sqlx::Error) -> ApiError {
    if let sqlx::Error::Database(ref database) = error
        && database.is_unique_violation()
    {
        return (StatusCode::CONFLICT, "Template version already exists");
    }
    tracing::error!("Template database operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Template database operation failed",
    )
}

fn valid_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
}

fn valid_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 9
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

fn valid_text(text: &str, max: usize, required: bool) -> bool {
    text.len() <= max
        && (!required || !text.trim().is_empty())
        && !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
}

fn validate(template: &Template) -> Result<(), ApiError> {
    if !valid_name(&template.name)
        || !valid_version(&template.version)
        || !valid_text(&template.title, 120, true)
        || !valid_text(&template.description, 2000, false)
        || !matches!(
            template.stack.as_str(),
            "rust" | "dotnet" | "python" | "node" | "react"
        )
        || template.parameters.0.len() > MAX_PARAMETERS
    {
        return Err(INVALID);
    }
    let mut names = HashSet::new();
    for parameter in &template.parameters.0 {
        if !valid_name(&parameter.name)
            || !names.insert(&parameter.name)
            || !valid_text(&parameter.label, 120, true)
            || parameter
                .description
                .as_deref()
                .is_some_and(|description| !valid_text(description, 500, false))
        {
            return Err(INVALID);
        }
    }
    Ok(())
}

async fn list(
    State(pool): State<SqlitePool>,
    Extension(_user): Extension<User>,
    Query(page): Query<Page>,
) -> Result<Json<Vec<Template>>, ApiError> {
    if !(1..=MAX_LIMIT).contains(&page.limit) {
        return Err((StatusCode::BAD_REQUEST, "Invalid page limit"));
    }
    sqlx::query_as("SELECT name, version, title, description, stack, parameters FROM software_template_versions ORDER BY name, version LIMIT ? OFFSET ?")
        .bind(page.limit).bind(page.offset).fetch_all(&pool).await.map(Json).map_err(database_error)
}

async fn detail(
    State(pool): State<SqlitePool>,
    Extension(_user): Extension<User>,
    Path((name, version)): Path<(String, String)>,
) -> Result<Json<Template>, ApiError> {
    if !valid_name(&name) || !valid_version(&version) {
        return Err(INVALID);
    }
    sqlx::query_as("SELECT name, version, title, description, stack, parameters FROM software_template_versions WHERE name = ? AND version = ?")
        .bind(name).bind(version).fetch_optional(&pool).await.map_err(database_error)?.map(Json).ok_or((StatusCode::NOT_FOUND,"Template version not found"))
}

async fn create(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
    input: Result<Json<Template>, JsonRejection>,
) -> Result<(StatusCode, Json<Template>), ApiError> {
    if user.role != Role::Admin {
        return Err((StatusCode::FORBIDDEN, "Administrator required"));
    }
    let Json(template) = input.map_err(|error| (error.status(), "Invalid template request"))?;
    validate(&template)?;
    let mut transaction = pool.begin().await.map_err(database_error)?;
    sqlx::query("INSERT INTO software_template_versions (name,version,title,description,stack,parameters) VALUES (?,?,?,?,?,?)")
        .bind(&template.name).bind(&template.version).bind(&template.title).bind(&template.description).bind(&template.stack).bind(&template.parameters)
        .execute(&mut *transaction).await.map_err(database_error)?;
    sqlx::query("INSERT INTO identity_audit_events (actor_id, action) VALUES (?, ?)")
        .bind(user.id)
        .bind(format!(
            "create_template:{}:{}",
            template.name, template.version
        ))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok((StatusCode::CREATED, Json(template)))
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
    use serde_json::{Value, json};
    use tower::ServiceExt;

    fn definition() -> Value {
        json!({"name":"internal-service","version":"1.0.0","title":"Internal service","description":"Declarative starter","stack":"rust","parameters":[{"name":"project-name","label":"Project name","type":"string","required":true}]})
    }

    #[test]
    fn rejects_invalid_definitions_and_execution_fields() {
        let mut invalid = definition();
        invalid["version"] = json!("01.0.0");
        assert!(validate(&serde_json::from_value(invalid).unwrap()).is_err());
        let mut invalid = definition();
        invalid["parameters"] = json!([invalid["parameters"][0], invalid["parameters"][0]]);
        assert!(validate(&serde_json::from_value(invalid).unwrap()).is_err());
        let mut invalid = definition();
        invalid["parameters"] = json!((0..21).map(|i| json!({"name":format!("p-{i}"),"label":"Param","type":"boolean","required":false})).collect::<Vec<_>>());
        assert!(validate(&serde_json::from_value(invalid).unwrap()).is_err());
        let mut invalid = definition();
        invalid["steps"] = json!([]);
        assert!(serde_json::from_value::<Template>(invalid).is_err());
        let mut invalid = definition();
        invalid["parameters"][0]["type"] = json!("object");
        assert!(serde_json::from_value::<Template>(invalid).is_err());
        assert!(validate(&serde_json::from_value(definition()).unwrap()).is_ok());
    }

    async fn call(
        app: &Router,
        method: &str,
        path: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value) {
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
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn versions_are_authenticated_immutable_and_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("templates.db");
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
        let member = identity.login("member", &password).await.unwrap();
        let viewer = identity.login("viewer", &password).await.unwrap();
        let app = auth::protect(routes(pool.clone()), AuthState::new(identity, 3000));
        let endpoint = "/api/templates";
        assert_eq!(
            call(&app, "GET", endpoint, "", Value::Null).await.0,
            StatusCode::UNAUTHORIZED
        );
        for token in [member.as_str(), viewer.as_str()] {
            let (status, list) = call(&app, "GET", endpoint, token, Value::Null).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(list.as_array().unwrap().len(), 5);
            for item in list.as_array().unwrap() {
                assert!(validate(&serde_json::from_value(item.clone()).unwrap()).is_ok());
            }
            assert_eq!(
                call(&app, "POST", endpoint, token, definition()).await.0,
                StatusCode::FORBIDDEN
            );
        }
        let token = admin_token.as_str();
        let original = definition();
        assert_eq!(
            call(&app, "POST", endpoint, token, original.clone()).await,
            (StatusCode::CREATED, original.clone())
        );
        let mut duplicate = original.clone();
        duplicate["title"] = json!("Replacement");
        assert_eq!(
            call(&app, "POST", endpoint, token, duplicate).await.0,
            StatusCode::CONFLICT
        );
        let mut next = original.clone();
        next["version"] = json!("1.1.0");
        assert_eq!(
            call(&app, "POST", endpoint, token, next).await.0,
            StatusCode::CREATED
        );
        assert_eq!(
            call(
                &app,
                "GET",
                "/api/templates/internal-service/1.0.0",
                token,
                Value::Null
            )
            .await,
            (StatusCode::OK, original.clone())
        );
        assert_eq!(
            call(
                &app,
                "GET",
                "/api/templates/internal-service/2.0.0",
                token,
                Value::Null
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&app, "GET", "/api/templates?limit=101", token, Value::Null)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        let (_, page) = call(
            &app,
            "GET",
            "/api/templates?limit=2&offset=1",
            token,
            Value::Null,
        )
        .await;
        assert_eq!(page.as_array().unwrap().len(), 2);
        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM identity_audit_events WHERE action LIKE 'create_template:%'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(events, 2);
        drop(app);
        pool.close().await;
        let pool = crate::database::open(&path).await.unwrap();
        let app = auth::protect(
            routes(pool.clone()),
            AuthState::new(IdentityStore::new(pool), 3000),
        );
        assert_eq!(
            call(
                &app,
                "GET",
                "/api/templates/internal-service/1.0.0",
                token,
                Value::Null
            )
            .await,
            (StatusCode::OK, original)
        );
    }
}
