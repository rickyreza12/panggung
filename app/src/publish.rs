use crate::identity::{Role, User};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    routing::get,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::{collections::BTreeSet, sync::Arc, time::Duration};

type ApiError = (StatusCode, &'static str);
type RemoteFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<i64, ()>> + Send + 'a>>;
trait Remote: Send + Sync {
    fn create<'a>(&'a self, owner: &'a str, repo: &'a str) -> RemoteFuture<'a>;
}

struct GitHub {
    client: reqwest::Client,
    token: reqwest::header::HeaderValue,
}
impl Remote for GitHub {
    fn create<'a>(&'a self, owner: &'a str, repo: &'a str) -> RemoteFuture<'a> {
        Box::pin(async move {
            if !valid_owner(owner) || !valid_repo(repo) {
                return Err(());
            }
            let body = serde_json::json!({"name":repo,"private":true,"auto_init":false});
            let mut response = self
                .client
                .post(format!("https://api.github.com/orgs/{owner}/repos"))
                .header("Authorization", self.token.clone())
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .header("Content-Type", "application/json")
                .body(body.to_string())
                .send()
                .await
                .map_err(|_| ())?;
            if response.status().as_u16() != 201 {
                return Err(());
            }
            const MAX: usize = 256 * 1024;
            if response.content_length().is_some_and(|n| n > MAX as u64) {
                return Err(());
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
                if chunk.len() > MAX - bytes.len() {
                    return Err(());
                }
                bytes.extend_from_slice(&chunk);
            }
            #[derive(Deserialize)]
            struct Created {
                id: i64,
                full_name: String,
                private: bool,
            }
            let result: Created = serde_json::from_slice(&bytes).map_err(|_| ())?;
            if result.id <= 0
                || !result.private
                || !result
                    .full_name
                    .eq_ignore_ascii_case(&format!("{owner}/{repo}"))
            {
                return Err(());
            }
            Ok(result.id)
        })
    }
}
#[derive(Clone)]
struct PublishState {
    pool: SqlitePool,
    orgs: Arc<BTreeSet<String>>,
    remote: Option<Arc<dyn Remote>>,
}
#[derive(Debug, Serialize, sqlx::FromRow)]
struct Record {
    owner: String,
    repo: String,
    state: String,
    github_id: Option<i64>,
    actor_id: Option<i64>,
    created_at: i64,
    completed_at: Option<i64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    owner: String,
    repo: String,
    confirmation: String,
}
#[derive(Deserialize)]
struct Filter {
    #[serde(default)]
    offset: u32,
    #[serde(default = "default_limit")]
    limit: u32,
}
fn default_limit() -> u32 {
    20
}
#[derive(Serialize)]
struct Page {
    enabled: bool,
    allowed_organizations: Vec<String>,
    items: Vec<Record>,
    has_more: bool,
}

pub(crate) fn valid_owner(v: &str) -> bool {
    (1..=39).contains(&v.len())
        && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && v.as_bytes()[0].is_ascii_alphanumeric()
        && v.as_bytes()[v.len() - 1].is_ascii_alphanumeric()
        && !v.contains("--")
}
pub(crate) fn valid_repo(v: &str) -> bool {
    (1..=100).contains(&v.len())
        && v != "."
        && v != ".."
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
fn validate(owner: &str, repo: &str) -> Result<(), ApiError> {
    if !valid_owner(owner) || !valid_repo(repo) {
        return Err((StatusCode::BAD_REQUEST, "Invalid destination"));
    }
    Ok(())
}
fn admin(user: &User) -> Result<(), ApiError> {
    if user.role != Role::Admin {
        return Err((StatusCode::FORBIDDEN, "Administrator required"));
    }
    Ok(())
}
fn database_error(_: sqlx::Error) -> ApiError {
    tracing::error!("Repository publication database operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Publication operation failed; inspect status before taking further action",
    )
}
pub fn routes(pool: SqlitePool) -> Result<Router, reqwest::Error> {
    let orgs: BTreeSet<String> = std::env::var("PANGGUNG_GITHUB_PUBLISH_ORGS")
        .ok()
        .and_then(|v| {
            let parts: Vec<_> = v.split(',').map(str::trim).collect();
            if parts.len() > 100 || !parts.iter().all(|s| valid_owner(s)) {
                return None;
            }
            Some(parts.into_iter().map(str::to_ascii_lowercase).collect())
        })
        .unwrap_or_default();
    let token = std::env::var("PANGGUNG_GITHUB_PUBLISH_TOKEN")
        .ok()
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 512
                && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .and_then(|v| reqwest::header::HeaderValue::from_str(&format!("Bearer {v}")).ok());
    let remote: Option<Arc<dyn Remote>> =
        if let Some(mut token) = token.filter(|_| !orgs.is_empty()) {
            token.set_sensitive(true);
            let client = reqwest::Client::builder()
                .https_only(true)
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(15))
                .user_agent("panggung-repository-publish")
                .build()?;
            Some(Arc::new(GitHub { client, token }))
        } else {
            None
        };
    Ok(router(PublishState {
        pool,
        orgs: Arc::new(orgs),
        remote,
    }))
}
fn router(state: PublishState) -> Router {
    Router::new()
        .route(
            "/api/github/publish",
            get(list).post(publish).layer(DefaultBodyLimit::max(2048)),
        )
        .route("/api/github/publish/{owner}/{repo}", get(status))
        .with_state(state)
}
async fn list(
    State(state): State<PublishState>,
    Extension(user): Extension<User>,
    Query(filter): Query<Filter>,
) -> Result<Json<Page>, ApiError> {
    admin(&user)?;
    if filter.offset > 10000 || !(1..=50).contains(&filter.limit) {
        return Err((StatusCode::BAD_REQUEST, "Invalid pagination"));
    }
    let mut items=sqlx::query_as::<_,Record>("SELECT owner,repo,state,github_id,actor_id,created_at,completed_at FROM github_publish_attempts ORDER BY created_at DESC,owner,repo LIMIT ? OFFSET ?")
        .bind(filter.limit+1).bind(filter.offset).fetch_all(&state.pool).await.map_err(database_error)?;
    let has_more = items.len() > filter.limit as usize && filter.offset + filter.limit <= 10000;
    items.truncate(filter.limit as usize);
    Ok(Json(Page {
        enabled: state.remote.is_some(),
        allowed_organizations: state.orgs.iter().cloned().collect(),
        items,
        has_more,
    }))
}
async fn status(
    State(state): State<PublishState>,
    Extension(user): Extension<User>,
    Path((owner, repo)): Path<(String, String)>,
) -> Result<Json<Record>, ApiError> {
    admin(&user)?;
    validate(&owner, &repo)?;
    sqlx::query_as::<_,Record>("SELECT owner,repo,state,github_id,actor_id,created_at,completed_at FROM github_publish_attempts WHERE owner=? AND repo=?")
        .bind(owner).bind(repo).fetch_optional(&state.pool).await.map_err(database_error)?
        .map(Json).ok_or((StatusCode::NOT_FOUND,"Publication not found"))
}
async fn publish(
    State(state): State<PublishState>,
    Extension(user): Extension<User>,
    Json(input): Json<Input>,
) -> Result<(StatusCode, Json<Record>), ApiError> {
    admin(&user)?;
    validate(&input.owner, &input.repo)?;
    if input.confirmation != format!("{}/{}", input.owner, input.repo) {
        return Err((
            StatusCode::BAD_REQUEST,
            "Exact destination confirmation required",
        ));
    }
    let owner = input.owner.to_ascii_lowercase();
    let repo = input.repo.to_ascii_lowercase();
    let remote = state
        .remote
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Publishing disabled"))?;
    if !state.orgs.contains(&owner) {
        return Err((StatusCode::FORBIDDEN, "Organization not allowed"));
    }
    let mut tx = state.pool.begin().await.map_err(database_error)?;
    let inserted=sqlx::query("INSERT INTO github_publish_attempts(owner,repo,state,actor_id) VALUES(?,?,'uncertain',?) ON CONFLICT(owner,repo) DO NOTHING")
        .bind(&owner).bind(&repo).bind(user.id).execute(&mut *tx).await.map_err(database_error)?;
    if inserted.rows_affected() == 0 {
        return Err((
            StatusCode::CONFLICT,
            "Destination already reserved; inspect publication status",
        ));
    }
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
        .bind(user.id)
        .bind(format!("github_publish_reserved:{owner}/{repo}"))
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    // A durable uncertain reservation precedes the sole remote attempt. Crash or cancellation cannot allow a retry.
    let result = remote.create(&owner, &repo).await;
    let mut tx = state.pool.begin().await.map_err(database_error)?;
    let id = match result {
        Ok(id) if id > 0 => id,
        _ => {
            sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
                .bind(user.id)
                .bind(format!("github_publish_uncertain:{owner}/{repo}"))
                .execute(&mut *tx)
                .await
                .map_err(database_error)?;
            tx.commit().await.map_err(database_error)?;
            return Err((
                StatusCode::BAD_GATEWAY,
                "Remote outcome uncertain; do not retry; inspect destination manually",
            ));
        }
    };
    let record=sqlx::query_as::<_,Record>("UPDATE github_publish_attempts SET state='succeeded',github_id=?,completed_at=unixepoch() WHERE owner=? AND repo=? AND state='uncertain' RETURNING owner,repo,state,github_id,actor_id,created_at,completed_at")
        .bind(id).bind(&owner).bind(&repo).fetch_one(&mut *tx).await.map_err(database_error)?;
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
        .bind(user.id)
        .bind(format!("github_publish_succeeded:{owner}/{repo}"))
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    Ok((StatusCode::CREATED, Json(record)))
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;
    struct Fake {
        calls: AtomicUsize,
    }
    impl Remote for Fake {
        fn create<'a>(&'a self, _: &'a str, repo: &'a str) -> RemoteFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if repo == "uncertain" {
                    Err(())
                } else {
                    Ok(123)
                }
            })
        }
    }
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
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }
    #[tokio::test]
    async fn admin_only_confirmed_and_durable_no_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("publish.db");
        let pool = crate::database::open(&path).await.unwrap();
        let identity = IdentityStore::new(pool.clone());
        let mut random = [0u8; 32];
        OsRng.fill_bytes(&mut random);
        let password: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let admin_user = identity.bootstrap_admin("admin", &password).await.unwrap();
        identity
            .create_user(admin_user.id, "member", &password, Role::Member)
            .await
            .unwrap();
        let admin_token = identity.login("admin", &password).await.unwrap();
        let member_token = identity.login("member", &password).await.unwrap();
        let fake = Arc::new(Fake {
            calls: AtomicUsize::new(0),
        });
        let orgs = Arc::new(BTreeSet::from(["allowed".to_string()]));
        let app = auth::protect(
            router(PublishState {
                pool: pool.clone(),
                orgs: orgs.clone(),
                remote: Some(fake.clone()),
            }),
            AuthState::new(identity, 3000),
        );
        let uri = "/api/github/publish";
        let token = admin_token.as_str();
        let body = r#"{"owner":"Allowed","repo":"Service","confirmation":"Allowed/Service"}"#;
        assert_eq!(
            call(&app, "GET", uri, "", "").await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "GET", uri, member_token.as_str(), "").await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "POST", uri, member_token.as_str(), body).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                &app,
                "POST",
                uri,
                token,
                r#"{"owner":"allowed","repo":"service","confirmation":"wrong"}"#
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &app,
                "POST",
                uri,
                token,
                r#"{"owner":"other","repo":"service","confirmation":"other/service"}"#
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
        let (a, b) = tokio::join!(
            call(&app, "POST", uri, token, body),
            call(&app, "POST", uri, token, body)
        );
        assert!(
            (a.0 == StatusCode::CREATED && b.0 == StatusCode::CONFLICT)
                || (b.0 == StatusCode::CREATED && a.0 == StatusCode::CONFLICT)
        );
        let uncertain =
            r#"{"owner":"allowed","repo":"uncertain","confirmation":"allowed/uncertain"}"#;
        assert_eq!(
            call(&app, "POST", uri, token, uncertain).await.0,
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
        drop(app);
        pool.close().await;
        let pool = crate::database::open(&path).await.unwrap();
        let app = auth::protect(
            router(PublishState {
                pool: pool.clone(),
                orgs,
                remote: Some(fake.clone()),
            }),
            AuthState::new(IdentityStore::new(pool.clone()), 3000),
        );
        assert_eq!(
            call(&app, "POST", uri, token, uncertain).await.0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            call(
                &app,
                "POST",
                uri,
                token,
                r#"{"owner":"allowed","repo":"service","confirmation":"allowed/service"}"#
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            call(
                &app,
                "GET",
                "/api/github/publish/allowed/uncertain",
                token,
                ""
            )
            .await
            .1["state"],
            "uncertain"
        );
        assert_eq!(
            call(
                &app,
                "GET",
                "/api/github/publish/allowed/service",
                token,
                ""
            )
            .await
            .1["state"],
            "succeeded"
        );
        assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM identity_audit_events WHERE action LIKE 'github_publish_%'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 4);
    }
}
