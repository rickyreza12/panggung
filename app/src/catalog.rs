use axum::{Json, extract::State, http::StatusCode};
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
    if let sqlx::Error::Database(ref db) = error {
        if db.is_unique_violation() {
            return (StatusCode::CONFLICT, "Component already exists");
        }
    }
    tracing::error!("Catalog database operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Database operation failed",
    )
}

pub async fn list(State(pool): State<SqlitePool>) -> Result<Json<Vec<Component>>, ApiError> {
    sqlx::query_as("SELECT name, description, owner FROM components ORDER BY name LIMIT 500")
        .fetch_all(&pool)
        .await
        .map(Json)
        .map_err(internal)
}

pub async fn create(
    State(pool): State<SqlitePool>,
    Json(input): Json<NewComponent>,
) -> Result<(StatusCode, Json<Component>), ApiError> {
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
    sqlx::query("INSERT INTO components (name, description, owner) VALUES (?, ?, ?)")
        .bind(name)
        .bind(description)
        .bind(owner)
        .execute(&pool)
        .await
        .map_err(internal)?;
    Ok((
        StatusCode::CREATED,
        Json(Component {
            name: name.into(),
            description: description.into(),
            owner: owner.into(),
        }),
    ))
}
