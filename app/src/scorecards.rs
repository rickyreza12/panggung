use crate::{
    catalog_relations::{project, reference_parts},
    identity::{Role, User},
    storage::{ObjectKey, ObjectStore},
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    http::StatusCode,
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{SqlitePool, types::Json as StoredJson};
use std::{collections::HashSet, sync::Arc};

type Error = (StatusCode, &'static str);
const INVALID: Error = (StatusCode::BAD_REQUEST, "Invalid scorecard input");
const MISSING: Error = (StatusCode::NOT_FOUND, "Component or standard not found");
#[derive(Clone)]
struct ScoreState {
    pool: SqlitePool,
    store: Arc<dyn ObjectStore>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Check {
    Description,
    Ownership,
    Docs,
    Metadata,
}
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
struct Standard {
    name: String,
    version: i64,
    title: String,
    checks: StoredJson<Vec<Check>>,
}
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Outcome {
    Pass,
    Fail,
    Unknown,
}
#[derive(Debug, Serialize, Deserialize)]
struct ResultItem {
    check: Check,
    status: Outcome,
    evidence: String,
}
#[derive(Debug, Serialize, sqlx::FromRow)]
struct Evaluation {
    id: i64,
    namespace: String,
    name: String,
    standard_name: String,
    standard_version: i64,
    team: Option<String>,
    actor_id: Option<i64>,
    results: StoredJson<Vec<ResultItem>>,
    created_at: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Evaluate {
    namespace: String,
    name: String,
    standard_name: String,
    standard_version: i64,
}
#[derive(sqlx::FromRow)]
struct ComponentEvidence {
    description: String,
    owner: String,
    component_type: Option<String>,
    lifecycle: Option<String>,
    descriptor: Option<StoredJson<Value>>,
}
#[derive(sqlx::FromRow)]
struct DocEvidence {
    object_key: String,
    content_digest: Vec<u8>,
    version: i64,
}
#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    offset: u32,
    #[serde(default = "default_limit")]
    limit: u32,
    team: Option<String>,
    namespace: Option<String>,
    name: Option<String>,
}
fn default_limit() -> u32 {
    50
}
fn valid_part(s: &str) -> bool {
    (1..=63).contains(&s.len())
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.as_bytes()[s.len() - 1].is_ascii_alphanumeric()
}
fn db_error(error: sqlx::Error) -> Error {
    if let sqlx::Error::Database(ref db) = error
        && db.is_unique_violation()
    {
        return (StatusCode::CONFLICT, "Standard version already exists");
    }
    tracing::error!("Scorecard database operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Scorecard database operation failed",
    )
}
pub fn routes(pool: SqlitePool, store: Arc<dyn ObjectStore>) -> Router {
    Router::new()
        .route(
            "/api/scorecards/standards",
            get(standards).post(create_standard),
        )
        .route("/api/scorecards/evaluations", get(history).post(evaluate))
        .layer(DefaultBodyLimit::max(8192))
        .with_state(ScoreState { pool, store })
}
fn validate_standard(s: &Standard) -> Result<(), Error> {
    if !valid_part(&s.name)
        || s.version <= 0
        || s.title.trim().is_empty()
        || s.title.len() > 120
        || s.title.chars().any(char::is_control)
        || s.checks.0.is_empty()
        || s.checks.0.len() > 4
        || s.checks.0.iter().collect::<HashSet<_>>().len() != s.checks.0.len()
    {
        return Err(INVALID);
    }
    Ok(())
}
async fn standards(
    State(state): State<ScoreState>,
    Extension(_): Extension<User>,
    Query(page): Query<Page>,
) -> Result<Json<Vec<Standard>>, Error> {
    if !(1..=100).contains(&page.limit) {
        return Err(INVALID);
    }
    sqlx::query_as("SELECT name,version,title,checks FROM scorecard_standards ORDER BY name,version DESC LIMIT ? OFFSET ?").bind(page.limit).bind(page.offset).fetch_all(&state.pool).await.map(Json).map_err(db_error)
}
async fn create_standard(
    State(state): State<ScoreState>,
    Extension(user): Extension<User>,
    Json(input): Json<Standard>,
) -> Result<(StatusCode, Json<Standard>), Error> {
    if user.role != Role::Admin {
        return Err((StatusCode::FORBIDDEN, "Administrator required"));
    }
    validate_standard(&input)?;
    let mut tx = state.pool.begin().await.map_err(db_error)?;
    sqlx::query("INSERT INTO scorecard_standards(name,version,title,checks) VALUES(?,?,?,?)")
        .bind(&input.name)
        .bind(input.version)
        .bind(&input.title)
        .bind(&input.checks)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
        .bind(user.id)
        .bind(format!(
            "create_scorecard_standard:{}:{}",
            input.name, input.version
        ))
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok((StatusCode::CREATED, Json(input)))
}
fn item(check: Check, status: Outcome, evidence: impl Into<String>) -> ResultItem {
    ResultItem {
        check,
        status,
        evidence: evidence.into(),
    }
}
fn text_result(check: Check, value: Option<&str>, label: &str) -> ResultItem {
    match value {
        None => item(check, Outcome::Unknown, format!("{label} not recorded")),
        Some(s) if s.trim().is_empty() => {
            item(check, Outcome::Fail, format!("{label} explicitly empty"))
        }
        Some(_) => item(check, Outcome::Pass, format!("{label} recorded")),
    }
}
async fn ownership(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    component: &ComponentEvidence,
    namespace: &str,
) -> Result<(ResultItem, Option<String>), Error> {
    if component.owner.trim().is_empty() {
        return Ok((
            item(Check::Ownership, Outcome::Unknown, "No owner recorded"),
            None,
        ));
    }
    let spec = json!({"owner":component.owner});
    let Some(reference) = project(&spec, namespace)
        .ok()
        .and_then(|r| r.into_iter().next())
    else {
        return Ok((
            item(
                Check::Ownership,
                Outcome::Unknown,
                "Owner reference cannot be interpreted",
            ),
            None,
        ));
    };
    let Some((kind, ns, name)) = reference_parts(&reference.target_ref) else {
        return Ok((
            item(
                Check::Ownership,
                Outcome::Unknown,
                "Owner reference cannot be interpreted",
            ),
            None,
        ));
    };
    if ns != "default" {
        return Ok((
            item(
                Check::Ownership,
                Outcome::Unknown,
                "Only default namespace identity records can be verified",
            ),
            None,
        ));
    }
    let exists: bool = match kind {
        "group" => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM identity_teams WHERE name=?)")
            .bind(name)
            .fetch_one(&mut **tx)
            .await
            .map_err(db_error)?,
        "user" => {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM identity_users WHERE username=?)")
                .bind(name)
                .fetch_one(&mut **tx)
                .await
                .map_err(db_error)?
        }
        _ => false,
    };
    let team = (exists && kind == "group").then(|| name.to_owned());
    Ok((
        item(
            Check::Ownership,
            if exists {
                Outcome::Pass
            } else {
                Outcome::Unknown
            },
            if exists {
                "Owner resolves to a registered identity"
            } else {
                "Declared owner is not registered locally"
            },
        ),
        team,
    ))
}
async fn docs_result(state: &ScoreState, doc: Option<DocEvidence>) -> ResultItem {
    let Some(doc) = doc else {
        return item(
            Check::Docs,
            Outcome::Unknown,
            "No documentation version registered",
        );
    };
    let Ok(key) = ObjectKey::parse(doc.object_key) else {
        return item(
            Check::Docs,
            Outcome::Unknown,
            "Documentation key cannot be verified",
        );
    };
    let store = state.store.clone();
    let result = tokio::task::spawn_blocking(move || store.read(&key)).await;
    match result {
        Ok(Ok(bytes))
            if bytes.len() <= 64 * 1024
                && Sha256::digest(&bytes).as_slice() == doc.content_digest.as_slice()
                && std::str::from_utf8(&bytes).is_ok_and(|s| !s.trim().is_empty()) =>
        {
            item(
                Check::Docs,
                Outcome::Pass,
                format!("Verified stored documentation version {}", doc.version),
            )
        }
        Ok(Ok(_)) => item(
            Check::Docs,
            Outcome::Fail,
            "Stored documentation does not match valid published content",
        ),
        _ => item(
            Check::Docs,
            Outcome::Unknown,
            "Documentation object cannot be read",
        ),
    }
}
fn metadata(component: &ComponentEvidence) -> ResultItem {
    let Some(descriptor) = &component.descriptor else {
        return item(Check::Metadata, Outcome::Unknown, "No imported descriptor");
    };
    let valid = descriptor.0["apiVersion"] == "backstage.io/v1alpha1"
        && descriptor.0["kind"] == "Component"
        && component
            .component_type
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        && component
            .lifecycle
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
    item(
        Check::Metadata,
        if valid { Outcome::Pass } else { Outcome::Fail },
        if valid {
            "Supported Component descriptor, type and lifecycle recorded"
        } else {
            "Stored descriptor lacks supported Component metadata"
        },
    )
}
async fn evaluate(
    State(state): State<ScoreState>,
    Extension(user): Extension<User>,
    Json(input): Json<Evaluate>,
) -> Result<(StatusCode, Json<Evaluation>), Error> {
    if user.role == Role::Viewer {
        return Err((StatusCode::FORBIDDEN, "Write permission required"));
    }
    if !valid_part(&input.namespace)
        || !valid_part(&input.name)
        || !valid_part(&input.standard_name)
        || input.standard_version <= 0
    {
        return Err(INVALID);
    }
    let mut tx = state.pool.begin().await.map_err(db_error)?;
    let standard: Standard = sqlx::query_as(
        "SELECT name,version,title,checks FROM scorecard_standards WHERE name=? AND version=?",
    )
    .bind(&input.standard_name)
    .bind(input.standard_version)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?
    .ok_or(MISSING)?;
    let component:ComponentEvidence=sqlx::query_as("SELECT description,owner,component_type,lifecycle,descriptor FROM components WHERE namespace=? AND name=?").bind(&input.namespace).bind(&input.name).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(MISSING)?;
    let (owner, team) = ownership(&mut tx, &component, &input.namespace).await?;
    let doc:Option<DocEvidence>=sqlx::query_as("SELECT object_key,content_digest,version FROM techdocs_versions WHERE namespace=? AND component_name=? ORDER BY version DESC LIMIT 1").bind(&input.namespace).bind(&input.name).fetch_optional(&mut *tx).await.map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    let mut results = Vec::new();
    let mut owner = Some(owner);
    let mut doc = doc;
    for check in standard.checks.0 {
        results.push(match check {
            Check::Description => text_result(check, Some(&component.description), "Description"),
            Check::Ownership => owner.take().ok_or(INVALID)?,
            Check::Docs => docs_result(&state, doc.take()).await,
            Check::Metadata => metadata(&component),
        });
    }
    let mut tx = state.pool.begin().await.map_err(db_error)?;
    let evaluation:Evaluation=sqlx::query_as("INSERT INTO scorecard_evaluations(namespace,name,standard_name,standard_version,team,actor_id,results) VALUES(?,?,?,?,?,?,?) RETURNING id,namespace,name,standard_name,standard_version,team,actor_id,results,created_at").bind(&input.namespace).bind(&input.name).bind(&input.standard_name).bind(input.standard_version).bind(team).bind(user.id).bind(StoredJson(results)).fetch_one(&mut *tx).await.map_err(db_error)?;
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
        .bind(user.id)
        .bind(format!("evaluate_scorecard:{}", evaluation.id))
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok((StatusCode::CREATED, Json(evaluation)))
}
async fn history(
    State(state): State<ScoreState>,
    Extension(_): Extension<User>,
    Query(page): Query<Page>,
) -> Result<Json<Vec<Evaluation>>, Error> {
    if !(1..=100).contains(&page.limit)
        || [&page.team, &page.namespace, &page.name]
            .iter()
            .any(|p| p.as_deref().is_some_and(|s| !valid_part(s)))
    {
        return Err(INVALID);
    }
    sqlx::query_as("SELECT id,namespace,name,standard_name,standard_version,team,actor_id,results,created_at FROM scorecard_evaluations WHERE (? IS NULL OR team=?) AND (? IS NULL OR namespace=?) AND (? IS NULL OR name=?) ORDER BY id DESC LIMIT ? OFFSET ?").bind(&page.team).bind(&page.team).bind(&page.namespace).bind(&page.namespace).bind(&page.name).bind(&page.name).bind(page.limit).bind(page.offset).fetch_all(&state.pool).await.map(Json).map_err(db_error)
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
    use rand_core::{OsRng, RngCore};
    use tower::ServiceExt;

    async fn call(
        app: &Router,
        method: &str,
        path: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let res = app
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
        let status = res.status();
        let bytes = to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    #[tokio::test]
    async fn scorecards_are_authorized_evidence_based_and_persistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scorecards.db");
        let pool = crate::database::open(&path).await.unwrap();
        let store: Arc<dyn ObjectStore> =
            Arc::new(LocalStorage::open(dir.path().join("docs")).unwrap());
        let identity = IdentityStore::new(pool.clone());
        let mut random = [0u8; 32];
        OsRng.fill_bytes(&mut random);
        let password: String = random.iter().map(|b| format!("{b:02x}")).collect();
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
        let app = auth::protect(
            routes(pool.clone(), store.clone()),
            AuthState::new(identity, 3000),
        );
        let standards = "/api/scorecards/standards";
        let evaluations = "/api/scorecards/evaluations";
        let config =
            json!({"name":"engineering","version":2,"title":"Second version","checks":["docs"]});
        let input = json!({"namespace":"default","name":"billing","standard_name":"engineering","standard_version":1});
        assert_eq!(
            call(&app, "GET", standards, "", Value::Null).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "GET", standards, viewer.as_str(), Value::Null)
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            call(&app, "POST", standards, member.as_str(), config.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                &app,
                "POST",
                standards,
                admin_token.as_str(),
                config.clone()
            )
            .await
            .0,
            StatusCode::CREATED
        );
        assert_eq!(
            call(&app, "POST", standards, admin_token.as_str(), config)
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            call(
                &app,
                "POST",
                standards,
                admin_token.as_str(),
                json!({"name":"bad","version":1,"title":"Bad","checks":["docs","docs"]})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(&app, "POST", evaluations, viewer.as_str(), input.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "POST", evaluations, member.as_str(), input.clone())
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        sqlx::query("INSERT INTO components(namespace,name,description,owner) VALUES('default','billing','','platform')").execute(&pool).await.unwrap();
        let (status, first) = call(&app, "POST", evaluations, member.as_str(), input.clone()).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(first["results"][0]["status"], "fail");
        for i in 1..4 {
            assert_eq!(first["results"][i]["status"], "unknown");
        }
        sqlx::query("INSERT INTO identity_teams(name) VALUES('platform')")
            .execute(&pool)
            .await
            .unwrap();
        let descriptor = json!({"apiVersion":"backstage.io/v1alpha1","kind":"Component","metadata":{"name":"billing"},"spec":{"owner":"platform","type":"service","lifecycle":"production"}});
        sqlx::query("UPDATE components SET description='Billing service',component_type='service',lifecycle='production',descriptor=? WHERE name='billing'").bind(descriptor.to_string()).execute(&pool).await.unwrap();
        let key = ObjectKey::parse("a".repeat(32)).unwrap();
        let markdown = b"# Billing";
        store.write(&key, markdown).unwrap();
        sqlx::query("INSERT INTO techdocs_versions(namespace,component_name,version,object_key,content_digest) VALUES('default','billing',1,?,?)").bind(key.as_str()).bind(Sha256::digest(markdown).to_vec()).execute(&pool).await.unwrap();
        let (status, second) =
            call(&app, "POST", evaluations, member.as_str(), input.clone()).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(second["team"], "platform");
        for i in 0..4 {
            assert_eq!(second["results"][i]["status"], "pass");
        }
        sqlx::query("UPDATE techdocs_versions SET content_digest=? WHERE component_name='billing'")
            .bind(vec![0u8; 32])
            .execute(&pool)
            .await
            .unwrap();
        let (_, third) = call(&app, "POST", evaluations, member.as_str(), input).await;
        assert_eq!(third["results"][2]["status"], "fail");
        let (_, filtered) = call(
            &app,
            "GET",
            "/api/scorecards/evaluations?team=platform",
            viewer.as_str(),
            Value::Null,
        )
        .await;
        assert_eq!(filtered.as_array().unwrap().len(), 2);
        drop(app);
        pool.close().await;
        let pool = crate::database::open(&path).await.unwrap();
        let app = auth::protect(
            routes(pool.clone(), store),
            AuthState::new(IdentityStore::new(pool), 3000),
        );
        let (_, history) = call(&app, "GET", evaluations, viewer.as_str(), Value::Null).await;
        assert_eq!(history.as_array().unwrap().len(), 3);
        assert_eq!(history[2], first);
    }
}
