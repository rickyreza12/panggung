//! Passive OpenAPI 3.0 catalog. No fetching, reference expansion or request execution.
use crate::identity::{Role, User};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{SqlitePool, types::Json as StoredJson};

type Error = (StatusCode, &'static str);
const INVALID: Error = (
    StatusCode::BAD_REQUEST,
    "Unsupported or invalid OpenAPI document",
);
const MISSING: Error = (StatusCode::NOT_FOUND, "Component or interface not found");
const MAX_BYTES: usize = 128 * 1024;

#[derive(Serialize, sqlx::FromRow)]
struct Version {
    namespace: String,
    component_name: String,
    version: i64,
    title: String,
    created_at: i64,
}
#[derive(Serialize, sqlx::FromRow)]
struct Document {
    namespace: String,
    component_name: String,
    version: i64,
    title: String,
    specification: StoredJson<Value>,
    created_at: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    specification: Value,
}
#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    offset: u32,
    #[serde(default = "default_limit")]
    limit: u32,
}
fn default_limit() -> u32 {
    20
}
fn internal(_: sqlx::Error) -> Error {
    tracing::error!("Interface catalog operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Interface catalog operation failed",
    )
}
fn valid_part(s: &str) -> bool {
    (1..=63).contains(&s.len())
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.as_bytes()[s.len() - 1].is_ascii_alphanumeric()
}

fn validate(value: &Value) -> Result<&str, Error> {
    if serde_json::to_vec(value).map_err(|_| INVALID)?.len() > MAX_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "Specification exceeds 128 KiB",
        ));
    }
    let root = value.as_object().ok_or(INVALID)?;
    if !matches!(
        root.get("openapi").and_then(Value::as_str),
        Some("3.0.0" | "3.0.1" | "3.0.2" | "3.0.3" | "3.0.4")
    ) {
        return Err(INVALID);
    }
    let info = root.get("info").and_then(Value::as_object).ok_or(INVALID)?;
    let title = info
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty() && s.len() <= 200)
        .ok_or(INVALID)?;
    if !info
        .get("version")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty() && s.len() <= 100)
    {
        return Err(INVALID);
    }
    let paths = root
        .get("paths")
        .and_then(Value::as_object)
        .ok_or(INVALID)?;
    if paths.len() > 200 {
        return Err(INVALID);
    }
    for (path, item) in paths {
        if !path.starts_with('/') || path.starts_with("//") || path.len() > 512 {
            return Err(INVALID);
        }
        let item = item.as_object().ok_or(INVALID)?;
        for (method, operation) in item {
            if matches!(
                method.as_str(),
                "get" | "put" | "post" | "delete" | "options" | "head" | "patch" | "trace"
            ) {
                let responses = operation
                    .get("responses")
                    .and_then(Value::as_object)
                    .ok_or(INVALID)?;
                if responses.is_empty()
                    || responses.values().any(|r| {
                        r.get("description")
                            .and_then(Value::as_str)
                            .is_none_or(|s| s.trim().is_empty())
                            && r.get("$ref").is_none()
                    })
                {
                    return Err(INVALID);
                }
            } else if !matches!(
                method.as_str(),
                "summary" | "description" | "parameters" | "servers" | "$ref"
            ) && !method.starts_with("x-")
            {
                return Err(INVALID);
            }
        }
    }
    // Strict passive subset: local JSON pointers only, no executable markup or active schemes.
    let mut pending = vec![(value, 0usize)];
    let mut count = 0usize;
    while let Some((node, depth)) = pending.pop() {
        count += 1;
        if depth > 40 || count > 20000 {
            return Err(INVALID);
        }
        match node {
            Value::Object(map) => {
                for (key, child) in map {
                    if key == "$ref" {
                        let reference = child.as_str().ok_or(INVALID)?;
                        let pointer = reference
                            .strip_prefix('#')
                            .filter(|p| p.starts_with('/'))
                            .ok_or(INVALID)?;
                        if value.pointer(pointer).is_none() {
                            return Err(INVALID);
                        }
                    }
                    if matches!(key.as_str(), "callbacks" | "externalValue" | "operationRef") {
                        return Err(INVALID);
                    }
                    pending.push((child, depth + 1));
                }
            }
            Value::Array(items) => pending.extend(items.iter().map(|v| (v, depth + 1))),
            Value::String(s) => {
                let lower = s.to_ascii_lowercase();
                if s.contains('<')
                    || s.chars()
                        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                    || lower.contains("javascript:")
                    || lower.contains("vbscript:")
                    || lower.contains("data:text/html")
                    || lower.contains("file://")
                {
                    return Err(INVALID);
                }
            }
            _ => {}
        }
    }
    Ok(title)
}

pub fn routes(pool: SqlitePool) -> Router {
    Router::new()
        .route("/api/interfaces", get(list))
        .route(
            "/api/interfaces/{namespace}/{name}",
            get(versions).post(register),
        )
        .route("/api/interfaces/{namespace}/{name}/{version}", get(read))
        .layer(DefaultBodyLimit::max(MAX_BYTES + 1024))
        .with_state(pool)
}
async fn list(
    State(pool): State<SqlitePool>,
    Extension(_): Extension<User>,
    Query(page): Query<Page>,
) -> Result<Json<Vec<Version>>, Error> {
    if !(1..=100).contains(&page.limit) || page.offset > 10000 {
        return Err(INVALID);
    }
    sqlx::query_as("SELECT namespace,component_name,version,title,created_at FROM interface_versions ORDER BY namespace,component_name,version DESC LIMIT ? OFFSET ?").bind(page.limit).bind(page.offset).fetch_all(&pool).await.map(Json).map_err(internal)
}
async fn versions(
    State(pool): State<SqlitePool>,
    Extension(_): Extension<User>,
    Path((ns, name)): Path<(String, String)>,
    Query(page): Query<Page>,
) -> Result<Json<Vec<Version>>, Error> {
    if !valid_part(&ns)
        || !valid_part(&name)
        || !(1..=100).contains(&page.limit)
        || page.offset > 10000
    {
        return Err(INVALID);
    }
    sqlx::query_as("SELECT namespace,component_name,version,title,created_at FROM interface_versions WHERE namespace=? AND component_name=? ORDER BY version DESC LIMIT ? OFFSET ?").bind(ns).bind(name).bind(page.limit).bind(page.offset).fetch_all(&pool).await.map(Json).map_err(internal)
}
async fn register(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
    Path((ns, name)): Path<(String, String)>,
    Json(input): Json<Input>,
) -> Result<(StatusCode, Json<Version>), Error> {
    if user.role == Role::Viewer {
        return Err((StatusCode::FORBIDDEN, "Write permission required"));
    }
    if !valid_part(&ns) || !valid_part(&name) {
        return Err(INVALID);
    }
    let title = validate(&input.specification)?.to_owned();
    let mut tx = pool.begin().await.map_err(internal)?;
    let version:Version=sqlx::query_as("INSERT INTO interface_versions(namespace,component_name,version,title,specification,created_by) SELECT ?,?,COALESCE((SELECT MAX(version) FROM interface_versions WHERE namespace=? AND component_name=?),0)+1,?,?,? WHERE EXISTS(SELECT 1 FROM components WHERE namespace=? AND name=?) RETURNING namespace,component_name,version,title,created_at").bind(&ns).bind(&name).bind(&ns).bind(&name).bind(title).bind(StoredJson(input.specification)).bind(user.id).bind(&ns).bind(&name).fetch_optional(&mut *tx).await.map_err(internal)?.ok_or(MISSING)?;
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
        .bind(user.id)
        .bind(format!(
            "register_interface:{ns}/{name}:{}",
            version.version
        ))
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok((StatusCode::CREATED, Json(version)))
}
async fn read(
    State(pool): State<SqlitePool>,
    Extension(_): Extension<User>,
    Path((ns, name, version)): Path<(String, String, i64)>,
) -> Result<Json<Document>, Error> {
    if !valid_part(&ns) || !valid_part(&name) || version <= 0 {
        return Err(INVALID);
    }
    sqlx::query_as("SELECT namespace,component_name,version,title,specification,created_at FROM interface_versions WHERE namespace=? AND component_name=? AND version=?").bind(ns).bind(name).bind(version).fetch_optional(&pool).await.map_err(internal)?.map(Json).ok_or(MISSING)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn sample() -> Value {
        json!({"openapi":"3.0.3","info":{"title":"Example","version":"1"},"paths":{"/health":{"get":{"responses":{"200":{"description":"OK"}}}}}})
    }
    #[test]
    fn rejects_unsafe_or_invalid_documents() {
        assert_eq!(validate(&sample()), Ok("Example"));
        for bad in [
            json!({"$ref":"https://example.invalid/schema"}),
            json!({"$ref":"#/missing"}),
            json!({"description":"<script>alert(1)</script>"}),
            json!({"url":"javascript:alert(1)"}),
            json!({"externalValue":"https://example.invalid"}),
        ] {
            let mut doc = sample();
            doc["x-test"] = bad;
            assert!(validate(&doc).is_err());
        }
        let mut doc = sample();
        doc["paths"]["/health"]["get"] = json!({});
        assert!(validate(&doc).is_err());
        let mut doc = sample();
        doc["x-large"] = json!("a".repeat(MAX_BYTES));
        assert!(validate(&doc).is_err());
    }
    #[tokio::test]
    async fn version_registration_is_authorized_and_persistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("interfaces.db");
        let pool = crate::database::open(&path).await.unwrap();
        let identity = crate::identity::IdentityStore::new(pool.clone());
        let admin = identity
            .bootstrap_admin("admin", "synthetic-test-password")
            .await
            .unwrap();
        let viewer = identity
            .create_user(admin.id, "viewer", "synthetic-test-password", Role::Viewer)
            .await
            .unwrap();
        sqlx::query("INSERT INTO components(name,description,owner) VALUES('service','','team')")
            .execute(&pool)
            .await
            .unwrap();
        let create = |user: User| {
            register(
                State(pool.clone()),
                Extension(user),
                Path(("default".into(), "service".into())),
                Json(Input {
                    specification: sample(),
                }),
            )
        };
        assert_eq!(
            create(viewer.clone()).await.err().unwrap().0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(create(admin.clone()).await.unwrap().1.0.version, 1);
        assert_eq!(create(admin).await.unwrap().1.0.version, 2);
        pool.close().await;
        let pool = crate::database::open(&path).await.unwrap();
        let doc = read(
            State(pool),
            Extension(viewer),
            Path(("default".into(), "service".into(), 1)),
        )
        .await
        .unwrap();
        assert_eq!(doc.0.specification.0, sample());
    }
}
