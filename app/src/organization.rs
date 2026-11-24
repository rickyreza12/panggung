use crate::identity::{IdentityError, IdentityStore, Role, User};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection},
    http::StatusCode,
    routing::{get, put},
};
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, SqlitePool, Transaction};

type ApiError = (StatusCode, &'static str);

#[derive(Serialize)]
struct Team {
    id: i64,
    name: String,
    members: Vec<User>,
}

#[derive(Deserialize)]
struct NewTeam {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewUser {
    username: String,
    password: String,
    role: Role,
}

#[derive(sqlx::FromRow)]
struct TeamRow {
    team_id: i64,
    team_name: String,
    user_id: Option<i64>,
    username: Option<String>,
    role: Option<Role>,
}

pub fn routes(pool: SqlitePool) -> Router {
    Router::new()
        .route(
            "/api/organization/users",
            get(list_users)
                .post(create_user)
                .layer(DefaultBodyLimit::max(8192)),
        )
        .route(
            "/api/organization/teams",
            get(list).post(create).layer(DefaultBodyLimit::max(4096)),
        )
        .route(
            "/api/organization/teams/{team_id}/members/{user_id}",
            put(add_member).delete(remove_member),
        )
        .with_state(pool)
}

fn database_error(error: sqlx::Error) -> ApiError {
    if let sqlx::Error::Database(ref database) = error
        && database.is_unique_violation()
    {
        return (StatusCode::CONFLICT, "Team or membership already exists");
    }
    tracing::error!("Organization database operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Organization database operation failed",
    )
}

fn require_admin(user: &User) -> Result<(), ApiError> {
    if user.role != Role::Admin {
        return Err((StatusCode::FORBIDDEN, "Administrator required"));
    }
    Ok(())
}

async fn list_users(
    State(pool): State<SqlitePool>,
    Extension(_user): Extension<User>,
) -> Result<Json<Vec<User>>, ApiError> {
    sqlx::query_as::<_, User>(
        "SELECT id, username, role FROM identity_users ORDER BY username LIMIT 500",
    )
    .fetch_all(&pool)
    .await
    .map(Json)
    .map_err(database_error)
}

async fn create_user(
    State(pool): State<SqlitePool>,
    Extension(actor): Extension<User>,
    input: Result<Json<NewUser>, JsonRejection>,
) -> Result<(StatusCode, Json<User>), ApiError> {
    require_admin(&actor)?;
    let Json(input) = input.map_err(|error| {
        let status = error.status();
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            (status, "Request too large")
        } else {
            (StatusCode::BAD_REQUEST, "Invalid user request")
        }
    })?;
    let user = IdentityStore::new(pool)
        .create_user(actor.id, &input.username, &input.password, input.role)
        .await
        .map_err(|error| match error {
            IdentityError::InvalidInput => {
                (StatusCode::BAD_REQUEST, "Invalid username or password")
            }
            IdentityError::Forbidden => (StatusCode::FORBIDDEN, "Administrator required"),
            IdentityError::Conflict => (StatusCode::CONFLICT, "Username already exists"),
            _ => {
                tracing::error!("User creation failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "User creation failed")
            }
        })?;
    Ok((StatusCode::CREATED, Json(user)))
}

async fn list(
    State(pool): State<SqlitePool>,
    Extension(_user): Extension<User>,
) -> Result<Json<Vec<Team>>, ApiError> {
    let rows = sqlx::query_as::<_, TeamRow>(
        "SELECT t.id AS team_id, t.name AS team_name, u.id AS user_id, u.username, u.role
         FROM identity_teams t
         LEFT JOIN identity_team_memberships m ON m.team_id = t.id
         LEFT JOIN identity_users u ON u.id = m.user_id
         ORDER BY t.name, u.username",
    )
    .fetch_all(&pool)
    .await
    .map_err(database_error)?;
    let mut teams: Vec<Team> = Vec::new();
    for row in rows {
        if teams.last().is_none_or(|team| team.id != row.team_id) {
            teams.push(Team {
                id: row.team_id,
                name: row.team_name,
                members: Vec::new(),
            });
        }
        if let (Some(id), Some(username), Some(role)) = (row.user_id, row.username, row.role) {
            teams
                .last_mut()
                .expect("team exists")
                .members
                .push(User { id, username, role });
        }
    }
    Ok(Json(teams))
}

fn valid_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
}

async fn create(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
    Json(input): Json<NewTeam>,
) -> Result<(StatusCode, Json<Team>), ApiError> {
    require_admin(&user)?;
    if !valid_name(&input.name) {
        return Err((StatusCode::BAD_REQUEST, "Invalid team name"));
    }
    let mut transaction = pool.begin().await.map_err(database_error)?;
    let id: i64 = sqlx::query_scalar("INSERT INTO identity_teams (name) VALUES (?) RETURNING id")
        .bind(&input.name)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
    audit(&mut transaction, user.id, None, format!("create_team:{id}")).await?;
    transaction.commit().await.map_err(database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(Team {
            id,
            name: input.name,
            members: Vec::new(),
        }),
    ))
}

async fn add_member(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
    Path((team_id, user_id)): Path<(i64, i64)>,
) -> Result<StatusCode, ApiError> {
    require_admin(&user)?;
    validate_ids(team_id, user_id)?;
    let mut transaction = pool.begin().await.map_err(database_error)?;
    let inserted = sqlx::query(
        "INSERT INTO identity_team_memberships (team_id, user_id)
         SELECT ?, ? WHERE EXISTS (SELECT 1 FROM identity_teams WHERE id = ?)
         AND EXISTS (SELECT 1 FROM identity_users WHERE id = ?)",
    )
    .bind(team_id)
    .bind(user_id)
    .bind(team_id)
    .bind(user_id)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    if inserted.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "Team or user not found"));
    }
    audit(
        &mut transaction,
        user.id,
        Some(user_id),
        format!("add_team_member:{team_id}"),
    )
    .await?;
    transaction.commit().await.map_err(database_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_member(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
    Path((team_id, user_id)): Path<(i64, i64)>,
) -> Result<StatusCode, ApiError> {
    require_admin(&user)?;
    validate_ids(team_id, user_id)?;
    let mut transaction = pool.begin().await.map_err(database_error)?;
    let removed =
        sqlx::query("DELETE FROM identity_team_memberships WHERE team_id = ? AND user_id = ?")
            .bind(team_id)
            .bind(user_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
    if removed.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "Membership not found"));
    }
    audit(
        &mut transaction,
        user.id,
        Some(user_id),
        format!("remove_team_member:{team_id}"),
    )
    .await?;
    transaction.commit().await.map_err(database_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn validate_ids(team_id: i64, user_id: i64) -> Result<(), ApiError> {
    if team_id <= 0 || user_id <= 0 {
        return Err((StatusCode::BAD_REQUEST, "Invalid team or user identifier"));
    }
    Ok(())
}

// The existing audit schema has a user subject, so team identity is encoded in the action.
async fn audit(
    transaction: &mut Transaction<'_, Sqlite>,
    actor: i64,
    subject: Option<i64>,
    action: String,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO identity_audit_events (actor_id, subject_id, action) VALUES (?, ?, ?)",
    )
    .bind(actor)
    .bind(subject)
    .bind(action)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
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

    async fn call(
        app: &Router,
        method: &str,
        path: &str,
        token: &str,
        body: &str,
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
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn create_user_requires_admin_and_audits_only_successful_requests() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::database::open(&dir.path().join("users.db"))
            .await
            .unwrap();
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
        let app = auth::protect(routes(pool.clone()), AuthState::new(identity.clone(), 3000));
        let path = "/api/organization/users";
        let valid = serde_json::json!({"username":"new-user","password":password,"role":"viewer"})
            .to_string();
        for (token, expected) in [
            ("", StatusCode::UNAUTHORIZED),
            (member_token.as_str(), StatusCode::FORBIDDEN),
            (viewer_token.as_str(), StatusCode::FORBIDDEN),
        ] {
            assert_eq!(call(&app, "POST", path, token, &valid).await.0, expected);
        }
        let token = admin_token.as_str();
        for invalid in [
            serde_json::json!({"username":"../bad","password":password,"role":"member"}),
            serde_json::json!({"username":"new-user","password":"short","role":"member"}),
            serde_json::json!({"username":"new-user","password":password,"role":"owner"}),
        ] {
            assert_eq!(
                call(&app, "POST", path, token, &invalid.to_string())
                    .await
                    .0,
                StatusCode::BAD_REQUEST
            );
        }
        let (status, user) = call(&app, "POST", path, token, &valid).await;
        assert_eq!(status, StatusCode::CREATED);
        let user_id = user["id"].as_i64().unwrap();
        assert_eq!(
            user,
            serde_json::json!({"id":user_id,"username":"new-user","role":"viewer"})
        );
        assert_eq!(
            call(&app, "POST", path, token, &valid).await.0,
            StatusCode::CONFLICT
        );
        let new_token = identity.login("new-user", &password).await.unwrap();
        assert_eq!(
            identity
                .resolve_session(new_token.as_str())
                .await
                .unwrap()
                .role,
            Role::Viewer
        );
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM identity_users")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 4);
        let events: Vec<(i64,i64)> = sqlx::query_as("SELECT actor_id, subject_id FROM identity_audit_events WHERE action = 'create_user' ORDER BY id").fetch_all(&pool).await.unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[2], (admin.id, user_id));
        // Force audit failure and ensure the user insert rolls back with it.
        sqlx::query("CREATE TRIGGER reject_user_audit BEFORE INSERT ON identity_audit_events WHEN NEW.action = 'create_user' BEGIN SELECT RAISE(ABORT, 'audit unavailable'); END")
            .execute(&pool).await.unwrap();
        let rejected =
            serde_json::json!({"username":"rollback-user","password":password,"role":"member"})
                .to_string();
        assert_eq!(
            call(&app, "POST", path, token, &rejected).await.0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM identity_users WHERE username = 'rollback-user'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn authenticated_team_mutations_are_admin_only_and_persist_with_audit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("teams.db");
        let pool = crate::database::open(&path).await.unwrap();
        let identity = IdentityStore::new(pool.clone());
        let mut random = [0u8; 32];
        OsRng.fill_bytes(&mut random);
        let password: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let admin = identity.bootstrap_admin("admin", &password).await.unwrap();
        let member = identity
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
        let app = auth::protect(routes(pool.clone()), AuthState::new(identity, 3000));
        let teams = "/api/organization/teams";
        let users = "/api/organization/users";
        let name = r#"{"name":"platform"}"#;
        assert_eq!(
            call(&app, "GET", teams, "", "").await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "GET", users, "", "").await.0,
            StatusCode::UNAUTHORIZED
        );
        for token in [member_token.as_str(), viewer_token.as_str()] {
            assert_eq!(call(&app, "GET", teams, token, "").await.0, StatusCode::OK);
            let (status, listed) = call(&app, "GET", users, token, "").await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(listed.as_array().unwrap().len(), 3);
            assert!(
                listed
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|user| user.as_object().unwrap().len() == 3)
            );
            assert_eq!(
                call(&app, "POST", teams, token, name).await.0,
                StatusCode::FORBIDDEN
            );
        }
        let token = admin_token.as_str();
        assert_eq!(
            call(&app, "POST", teams, token, r#"{"name":"../bad"}"#)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        let (status, team) = call(&app, "POST", teams, token, name).await;
        assert_eq!(status, StatusCode::CREATED);
        let team_id = team["id"].as_i64().unwrap();
        assert_eq!(
            call(&app, "POST", teams, token, name).await.0,
            StatusCode::CONFLICT
        );
        let membership = format!("{teams}/{team_id}/members/{}", member.id);
        for method in ["PUT", "DELETE"] {
            assert_eq!(
                call(&app, method, &membership, member_token.as_str(), "")
                    .await
                    .0,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            call(
                &app,
                "PUT",
                &format!("{teams}/{team_id}/members/999"),
                token,
                ""
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(
                &app,
                "PUT",
                &format!("{teams}/999/members/{}", member.id),
                token,
                ""
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&app, "PUT", &membership, token, "").await.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(&app, "PUT", &membership, token, "").await.0,
            StatusCode::CONFLICT
        );
        drop(app);
        pool.close().await;
        let pool = crate::database::open(&path).await.unwrap();
        let app = auth::protect(
            routes(pool.clone()),
            AuthState::new(IdentityStore::new(pool.clone()), 3000),
        );
        let (status, listed) = call(&app, "GET", teams, viewer_token.as_str(), "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed[0]["members"][0]["username"], "member");
        assert_eq!(listed[0]["members"][0]["role"], "member");
        assert!(listed[0]["members"][0].get("password_hash").is_none());
        assert_eq!(
            call(&app, "DELETE", &membership, token, "").await.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(&app, "DELETE", &membership, token, "").await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&app, "GET", teams, token, "").await.1[0]["members"],
            serde_json::json!([])
        );
        let events: Vec<(Option<i64>, Option<i64>, String)> = sqlx::query_as(
            "SELECT actor_id, subject_id, action FROM identity_audit_events WHERE action LIKE '%team%' ORDER BY id"
        ).fetch_all(&pool).await.unwrap();
        assert_eq!(
            events,
            vec![
                (Some(admin.id), None, format!("create_team:{team_id}")),
                (
                    Some(admin.id),
                    Some(member.id),
                    format!("add_team_member:{team_id}")
                ),
                (
                    Some(admin.id),
                    Some(member.id),
                    format!("remove_team_member:{team_id}")
                ),
            ]
        );
    }
}
