use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Component {
    pub name: String,
    pub description: String,
    pub owner: String,
}

#[derive(Deserialize)]
pub struct NewComponent {
    name: String,
    description: String,
    owner: String,
}

type ApiError = (StatusCode, &'static str);

fn internal(error: sqlx::Error) -> ApiError {
    if let sqlx::Error::Database(ref db) = error
        && db.is_unique_violation()
    {
        return (StatusCode::CONFLICT, "Component already exists");
    }
    tracing::error!("Catalog database operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Database operation failed",
    )
}

#[derive(Deserialize)]
pub struct Filter {
    #[serde(default)]
    q: String,
    #[serde(default)]
    offset: u32,
}

pub async fn list(
    State(pool): State<SqlitePool>,
    Query(filter): Query<Filter>,
) -> Result<Json<Vec<Component>>, ApiError> {
    if filter.q.len() > 200 {
        return Err((StatusCode::BAD_REQUEST, "Search term too long"));
    }
    sqlx::query_as("SELECT name, description, owner FROM components WHERE instr(lower(name || ' ' || description || ' ' || owner), lower(?)) > 0 OR ? = '' ORDER BY name LIMIT 500 OFFSET ?")
        .bind(&filter.q).bind(&filter.q).bind(filter.offset)
        .fetch_all(&pool)
        .await
        .map(Json)
        .map_err(internal)
}

pub async fn create(
    State(pool): State<SqlitePool>,
    Json(input): Json<NewComponent>,
) -> Result<(StatusCode, Json<Component>), ApiError> {
    let component = insert(&pool, input).await?;
    Ok((StatusCode::CREATED, Json(component)))
}

fn validate(input: NewComponent) -> Result<Component, ApiError> {
    let name = input.name.trim();
    let owner = input.owner.trim();
    let description = input.description.trim();
    if name.is_empty()
        || name.len() > 63
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
        || owner.is_empty()
        || owner.len() > 100
        || description.len() > 2000
    {
        return Err((StatusCode::BAD_REQUEST, "Invalid component fields"));
    }
    Ok(Component {
        name: name.into(),
        description: description.into(),
        owner: owner.into(),
    })
}

async fn insert(pool: &SqlitePool, input: NewComponent) -> Result<Component, ApiError> {
    let component = validate(input)?;
    sqlx::query("INSERT INTO components (name, description, owner) VALUES (?, ?, ?)")
        .bind(&component.name)
        .bind(&component.description)
        .bind(&component.owner)
        .execute(pool)
        .await
        .map_err(internal)?;
    Ok(component)
}

pub async fn get_one(
    State(pool): State<SqlitePool>,
    Path(name): Path<String>,
) -> Result<Json<Component>, ApiError> {
    sqlx::query_as("SELECT name, description, owner FROM components WHERE name = ?")
        .bind(name)
        .fetch_optional(&pool)
        .await
        .map_err(internal)?
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, "Component not found"))
}

pub async fn update(
    State(pool): State<SqlitePool>,
    Path(name): Path<String>,
    Json(input): Json<NewComponent>,
) -> Result<Json<Component>, ApiError> {
    let component = validate(input)?;
    if name != component.name {
        return Err((StatusCode::BAD_REQUEST, "Component names cannot be changed"));
    }
    let result = sqlx::query("UPDATE components SET description = ?, owner = ? WHERE name = ?")
        .bind(&component.description)
        .bind(&component.owner)
        .bind(name)
        .execute(&pool)
        .await
        .map_err(internal)?;
    if result.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "Component not found"));
    }
    Ok(Json(component))
}

pub async fn remove(
    State(pool): State<SqlitePool>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let result = sqlx::query("DELETE FROM components WHERE name = ?")
        .bind(name)
        .execute(&pool)
        .await
        .map_err(internal)?;
    if result.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "Component not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    api_version: String,
    kind: String,
    metadata: Metadata,
    spec: Spec,
}

#[derive(Deserialize)]
struct Metadata {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    namespace: Option<String>,
}

#[derive(Deserialize)]
struct Spec {
    owner: String,
    #[serde(rename = "type")]
    component_type: String,
    lifecycle: String,
}

pub async fn import(
    State(pool): State<SqlitePool>,
    Json(input): Json<Descriptor>,
) -> Result<(StatusCode, Json<Component>), ApiError> {
    if input.api_version != "backstage.io/v1alpha1"
        || input.kind != "Component"
        || input
            .metadata
            .namespace
            .as_deref()
            .is_some_and(|ns| ns != "default")
        || input.spec.component_type.trim().is_empty()
        || input.spec.lifecycle.trim().is_empty()
    {
        return Err((StatusCode::BAD_REQUEST, "Unsupported component descriptor"));
    }
    let component = insert(
        &pool,
        NewComponent {
            name: input.metadata.name,
            description: input.metadata.description,
            owner: input.spec.owner,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(component)))
}
