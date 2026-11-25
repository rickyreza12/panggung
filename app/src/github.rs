use crate::identity::{Role, User};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::{sync::Arc, time::Duration};

const API: &str = "https://api.github.com";
const MAX_BODY: usize = 2 * 1024 * 1024;
type ApiError = (StatusCode, &'static str);

#[derive(Clone)]
struct GitHubState {
    pool: SqlitePool,
    remote: Arc<dyn Remote>,
}
type RemoteFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, ApiError>> + Send + 'a>>;
trait Remote: Send + Sync {
    fn discover<'a>(
        &'a self,
        owner: &'a str,
        page: u32,
        limit: u32,
    ) -> RemoteFuture<'a, Vec<Repository>>;
    fn fetch<'a>(&'a self, owner: &'a str, name: &'a str)
    -> RemoteFuture<'a, (Repository, String)>;
}
struct GitHubClient {
    client: reqwest::Client,
    token: Option<String>,
}

#[derive(Deserialize)]
struct Owner {
    login: String,
}
#[derive(Deserialize)]
struct RemoteRepository {
    id: i64,
    name: String,
    owner: Owner,
    description: Option<String>,
    default_branch: String,
    archived: bool,
    private: bool,
}
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct Repository {
    github_id: i64,
    owner: String,
    name: String,
    description: Option<String>,
    default_branch: String,
    archived: bool,
}
#[derive(Debug, Serialize, sqlx::FromRow)]
struct Registered {
    #[sqlx(flatten)]
    #[serde(flatten)]
    repo: Repository,
    descriptor_status: String,
    synced_at: i64,
    synced_by: Option<i64>,
}
#[derive(Serialize)]
struct Page<T> {
    items: Vec<T>,
    has_more: bool,
}
#[derive(Deserialize)]
struct Discovery {
    #[serde(default = "first_page")]
    page: u32,
    #[serde(default = "default_limit")]
    limit: u32,
}
#[derive(Deserialize)]
struct Registry {
    #[serde(default)]
    offset: u32,
    #[serde(default = "default_limit")]
    limit: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncRequest {
    owner: String,
    repo: String,
}
fn first_page() -> u32 {
    1
}
fn default_limit() -> u32 {
    20
}

fn valid_owner(value: &str) -> bool {
    (1..=39).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
}
fn valid_repo(value: &str) -> bool {
    (1..=100).contains(&value.len())
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
fn validate(owner: &str, repo: Option<&str>) -> Result<(), ApiError> {
    if !valid_owner(owner) || repo.is_some_and(|name| !valid_repo(name)) {
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid GitHub owner or repository name",
        ));
    }
    Ok(())
}
fn upstream() -> ApiError {
    (StatusCode::BAD_GATEWAY, "GitHub request failed")
}
fn db_error(_: sqlx::Error) -> ApiError {
    tracing::error!("GitHub registry operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Repository registry operation failed",
    )
}

impl RemoteRepository {
    fn public(self) -> Result<Repository, ApiError> {
        if self.private {
            return Err((
                StatusCode::FORBIDDEN,
                "Private GitHub repositories are not supported",
            ));
        }
        if self.id <= 0
            || !valid_owner(&self.owner.login)
            || !valid_repo(&self.name)
            || self.default_branch.is_empty()
            || self.default_branch.len() > 255
            || self.default_branch.chars().any(char::is_control)
        {
            return Err(upstream());
        }
        let description = self.description.map(|s| s.chars().take(2000).collect());
        Ok(Repository {
            github_id: self.id,
            owner: self.owner.login,
            name: self.name,
            description,
            default_branch: self.default_branch,
            archived: self.archived,
        })
    }
}
impl GitHubClient {
    fn new() -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .user_agent("panggung-repository-discovery")
            .build()?;
        let token = std::env::var("PANGGUNG_GITHUB_TOKEN")
            .ok()
            .filter(|s| !s.is_empty());
        Ok(Self { client, token })
    }
    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Option<Vec<u8>>, ApiError> {
        let mut request = self
            .client
            .get(format!("{API}{path}"))
            .query(query)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let mut response = request.send().await.map_err(|_| upstream())?;
        let status = response.status();
        if status.as_u16() == 429
            || (status.as_u16() == 403
                && (response.headers().contains_key("retry-after")
                    || response
                        .headers()
                        .get("x-ratelimit-remaining")
                        .is_some_and(|v| v == "0")))
        {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "GitHub rate limit reached; try again later",
            ));
        }
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(upstream());
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_BODY as u64)
        {
            return Err(upstream());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| upstream())? {
            if chunk.len() > MAX_BODY - body.len() {
                return Err(upstream());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Some(body))
    }
}
impl Remote for GitHubClient {
    fn discover<'a>(
        &'a self,
        owner: &'a str,
        page: u32,
        limit: u32,
    ) -> RemoteFuture<'a, Vec<Repository>> {
        Box::pin(async move {
            validate(owner, None)?;
            let body = self
                .get(
                    &format!("/users/{owner}/repos"),
                    &[
                        ("page", page.to_string()),
                        ("per_page", limit.to_string()),
                        ("type", "owner".into()),
                        ("sort", "full_name".into()),
                    ],
                )
                .await?
                .ok_or((StatusCode::NOT_FOUND, "GitHub owner not found"))?;
            let repos: Vec<RemoteRepository> =
                serde_json::from_slice(&body).map_err(|_| upstream())?;
            repos
                .into_iter()
                .filter(|repo| !repo.private)
                .map(RemoteRepository::public)
                .collect()
        })
    }
    fn fetch<'a>(
        &'a self,
        owner: &'a str,
        name: &'a str,
    ) -> RemoteFuture<'a, (Repository, String)> {
        Box::pin(async move {
            validate(owner, Some(name))?;
            let path = format!("/repos/{owner}/{name}");
            let body = self
                .get(&path, &[])
                .await?
                .ok_or((StatusCode::NOT_FOUND, "GitHub repository not found"))?;
            let repo = serde_json::from_slice::<RemoteRepository>(&body)
                .map_err(|_| upstream())?
                .public()?;
            if !repo.owner.eq_ignore_ascii_case(owner) || !repo.name.eq_ignore_ascii_case(name) {
                return Err(upstream());
            }
            // Inspect only the conventional root file; never follow download URLs, symlinks, or submodules.
            let descriptor = self
                .get(&format!("{path}/contents/catalog-info.yaml"), &[])
                .await?;
            let status = match descriptor {
                None => "missing",
                Some(body) => {
                    #[derive(Deserialize)]
                    struct Entry {
                        #[serde(rename = "type")]
                        kind: String,
                        size: u64,
                        path: String,
                        #[serde(default)]
                        target: Option<String>,
                        #[serde(default)]
                        submodule_git_url: Option<String>,
                    }
                    match serde_json::from_slice::<Entry>(&body) {
                        Ok(entry)
                            if entry.kind == "file"
                                && entry.path == "catalog-info.yaml"
                                && entry.size > 0
                                && entry.size <= 65536
                                && entry.target.is_none()
                                && entry.submodule_git_url.is_none() =>
                        {
                            "found"
                        }
                        _ => "unsupported",
                    }
                }
            };
            Ok((repo, status.into()))
        })
    }
}

pub fn routes(pool: SqlitePool) -> Result<Router, reqwest::Error> {
    Ok(router(GitHubState {
        pool,
        remote: Arc::new(GitHubClient::new()?),
    }))
}
fn router(state: GitHubState) -> Router {
    Router::new()
        .route("/api/github/discover/{owner}", get(discover))
        .route("/api/github/repositories", get(registry))
        .route(
            "/api/github/sync",
            post(sync).layer(DefaultBodyLimit::max(2048)),
        )
        .with_state(state)
}
async fn discover(
    State(state): State<GitHubState>,
    Extension(_user): Extension<User>,
    Path(owner): Path<String>,
    Query(query): Query<Discovery>,
) -> Result<Json<Page<Repository>>, ApiError> {
    validate(&owner, None)?;
    if !(1..=100).contains(&query.page) || !(1..=50).contains(&query.limit) {
        return Err((StatusCode::BAD_REQUEST, "Invalid discovery pagination"));
    }
    let items = state
        .remote
        .discover(&owner, query.page, query.limit)
        .await?;
    let has_more = items.len() == query.limit as usize && query.page < 100;
    Ok(Json(Page { items, has_more }))
}
async fn registry(
    State(state): State<GitHubState>,
    Extension(_user): Extension<User>,
    Query(query): Query<Registry>,
) -> Result<Json<Page<Registered>>, ApiError> {
    if query.offset > 10000 || !(1..=50).contains(&query.limit) {
        return Err((StatusCode::BAD_REQUEST, "Invalid registry pagination"));
    }
    let mut items=sqlx::query_as::<_,Registered>("SELECT github_id,owner,name,description,default_branch,archived,descriptor_status,synced_at,synced_by FROM github_repositories ORDER BY owner,name LIMIT ? OFFSET ?")
        .bind(query.limit+1).bind(query.offset).fetch_all(&state.pool).await.map_err(db_error)?;
    let has_more = items.len() > query.limit as usize && query.offset + query.limit <= 10000;
    items.truncate(query.limit as usize);
    Ok(Json(Page { items, has_more }))
}
async fn sync(
    State(state): State<GitHubState>,
    Extension(user): Extension<User>,
    Json(input): Json<SyncRequest>,
) -> Result<Json<Registered>, ApiError> {
    if !matches!(user.role, Role::Admin | Role::Member) {
        return Err((StatusCode::FORBIDDEN, "Write permission required"));
    }
    validate(&input.owner, Some(&input.repo))?;
    let (repo, status) = state.remote.fetch(&input.owner, &input.repo).await?;
    let mut tx = state.pool.begin().await.map_err(db_error)?;
    let saved=sqlx::query_as::<_,Registered>(
        "INSERT INTO github_repositories(github_id,owner,name,description,default_branch,archived,descriptor_status,synced_by) VALUES(?,?,?,?,?,?,?,?)
         ON CONFLICT(github_id) DO UPDATE SET owner=excluded.owner,name=excluded.name,description=excluded.description,default_branch=excluded.default_branch,archived=excluded.archived,descriptor_status=excluded.descriptor_status,synced_at=unixepoch() WHERE github_repositories.synced_by=excluded.synced_by OR ?
         RETURNING github_id,owner,name,description,default_branch,archived,descriptor_status,synced_at,synced_by")
        .bind(repo.github_id).bind(&repo.owner).bind(&repo.name).bind(&repo.description).bind(&repo.default_branch).bind(repo.archived).bind(status).bind(user.id).bind(user.role == Role::Admin)
        .fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or((StatusCode::FORBIDDEN, "Repository registration belongs to another user"))?;
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
        .bind(user.id)
        .bind(format!("sync_github:{}/{}", repo.owner, repo.name))
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok(Json(saved))
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

    struct SyntheticRemote;
    impl Remote for SyntheticRemote {
        fn discover<'a>(
            &'a self,
            owner: &'a str,
            _: u32,
            _: u32,
        ) -> RemoteFuture<'a, Vec<Repository>> {
            Box::pin(async move { Ok(vec![fixture(owner, "service")]) })
        }
        fn fetch<'a>(
            &'a self,
            owner: &'a str,
            name: &'a str,
        ) -> RemoteFuture<'a, (Repository, String)> {
            Box::pin(async move {
                if name == "limited" {
                    return Err((StatusCode::TOO_MANY_REQUESTS, "GitHub rate limit reached"));
                }
                Ok((fixture(owner, name), "found".into()))
            })
        }
    }
    fn fixture(owner: &str, name: &str) -> Repository {
        Repository {
            github_id: 1,
            owner: owner.into(),
            name: name.into(),
            description: None,
            default_branch: "main".into(),
            archived: false,
        }
    }
    #[test]
    fn rejects_untrusted_path_segments() {
        for bad in ["", "../x", "a/b", "a?b", "a#b", "a%2fb", "https://evil"] {
            assert!(validate(bad, None).is_err());
            assert!(validate("owner", Some(bad)).is_err());
        }
        assert!(validate("owner-1", Some("service.v2")).is_ok());
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
    async fn discovery_is_read_only_and_sync_is_authorized_durable_and_audited() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("github.db");
        let pool = crate::database::open(&path).await.unwrap();
        let identity = IdentityStore::new(pool.clone());
        let mut random = [0u8; 32];
        OsRng.fill_bytes(&mut random);
        let password: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let admin = identity.bootstrap_admin("admin", &password).await.unwrap();
        let member = identity
            .create_user(admin.id, "member", &password, Role::Member)
            .await
            .unwrap();
        identity
            .create_user(admin.id, "viewer", &password, Role::Viewer)
            .await
            .unwrap();
        let member_token = identity.login("member", &password).await.unwrap();
        let admin_token = identity.login("admin", &password).await.unwrap();
        identity
            .create_user(admin.id, "other", &password, Role::Member)
            .await
            .unwrap();
        let other_token = identity.login("other", &password).await.unwrap();
        let viewer_token = identity.login("viewer", &password).await.unwrap();
        let app = auth::protect(
            router(GitHubState {
                pool: pool.clone(),
                remote: Arc::new(SyntheticRemote),
            }),
            AuthState::new(identity, 3000),
        );
        let body = r#"{"owner":"example","repo":"service"}"#;
        let viewer = viewer_token.as_str();
        let writer = member_token.as_str();
        assert_eq!(
            call(&app, "GET", "/api/github/discover/example", "", "")
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "GET", "/api/github/discover/example", viewer, "")
                .await
                .1["items"][0]["name"],
            "service"
        );
        assert_eq!(
            call(&app, "GET", "/api/github/repositories", viewer, "")
                .await
                .1["items"],
            serde_json::json!([])
        );
        assert_eq!(
            call(&app, "POST", "/api/github/sync", viewer, body).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/github/sync",
                writer,
                r#"{"owner":"example","repo":"limited"}"#
            )
            .await
            .0,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            call(&app, "POST", "/api/github/sync", writer, body).await.1["descriptor_status"],
            "found"
        );
        assert_eq!(
            call(&app, "POST", "/api/github/sync", writer, body).await.0,
            StatusCode::OK
        );
        assert_eq!(
            call(&app, "POST", "/api/github/sync", other_token.as_str(), body)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let (status, record) =
            call(&app, "POST", "/api/github/sync", admin_token.as_str(), body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(record["synced_by"], member.id);
        drop(app);
        pool.close().await;
        let pool = crate::database::open(&path).await.unwrap();
        let app = auth::protect(
            router(GitHubState {
                pool: pool.clone(),
                remote: Arc::new(SyntheticRemote),
            }),
            AuthState::new(IdentityStore::new(pool.clone()), 3000),
        );
        assert_eq!(
            call(&app, "GET", "/api/github/repositories", viewer, "")
                .await
                .1["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let actors: Vec<i64> = sqlx::query_scalar(
            "SELECT actor_id FROM identity_audit_events WHERE action='sync_github:example/service'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(actors, vec![member.id, member.id, admin.id]);
    }
}
