use crate::{
    identity::{Role, User},
    scaffolder::publication_files,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

type Error = (StatusCode, &'static str);
const INVALID: Error = (
    StatusCode::BAD_REQUEST,
    "Invalid publication destination or confirmation",
);
const UNCERTAIN: Error = (
    StatusCode::BAD_GATEWAY,
    "Publication outcome uncertain; inspect repository and history before proceeding. Automatic retry prohibited.",
);
fn db(_: sqlx::Error) -> Error {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Publication database operation failed; inspect history",
    )
}
fn admin(user: &User) -> Result<(), Error> {
    if user.role != Role::Admin {
        return Err((StatusCode::FORBIDDEN, "Administrator required"));
    }
    Ok(())
}
fn sha(value: &Value) -> Result<String, ()> {
    let s = value.as_str().ok_or(())?;
    if s.len() != 40 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(());
    }
    Ok(s.to_owned())
}
#[derive(Clone)]
struct StateData {
    pool: SqlitePool,
    remote: Option<Arc<dyn Remote>>,
    orgs: Vec<String>,
    destinations: Vec<String>,
}
type Files = BTreeMap<String, String>;
#[derive(Clone)]
struct Intent {
    pool: SqlitePool,
    job_id: i64,
    actor_id: i64,
}
impl Intent {
    async fn candidate(&self, commit: &str, tree: &str) -> Result<(), ()> {
        let mut tx = self.pool.begin().await.map_err(|_| ())?;
        let result = sqlx::query("UPDATE github_artifact_publications SET candidate_sha=?,candidate_tree=?,mutation_stage='candidate' WHERE job_id=? AND mutation_stage='reserved' AND state='uncertain'")
            .bind(commit).bind(tree).bind(self.job_id).execute(&mut *tx).await.map_err(|_| ())?;
        if result.rows_affected() != 1 {
            return Err(());
        }
        sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
            .bind(self.actor_id)
            .bind(format!("artifact_publish_candidate:{}", self.job_id))
            .execute(&mut *tx)
            .await
            .map_err(|_| ())?;
        tx.commit().await.map_err(|_| ())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteFailure {
    Unresolved,
    Conflict,
}
impl From<()> for RemoteFailure {
    fn from(_: ()) -> Self {
        Self::Unresolved
    }
}
fn branch_sha(reference: &Value) -> Result<String, RemoteFailure> {
    if reference["ref"].as_str() != Some("refs/heads/main")
        || reference["object"]["type"].as_str() != Some("commit")
    {
        return Err(RemoteFailure::Conflict);
    }
    Ok(sha(&reference["object"]["sha"])?)
}

fn verify_repository(repo: &Value, destination: &str, id: i64) -> Result<(), RemoteFailure> {
    if repo["id"].as_i64() != Some(id)
        || repo["private"].as_bool() != Some(true)
        || !repo["full_name"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case(destination))
        || repo["default_branch"].as_str() != Some("main")
        || repo["archived"].as_bool() != Some(false)
    {
        return Err(RemoteFailure::Conflict);
    }
    Ok(())
}

type RemoteFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, RemoteFailure>> + Send + 'a>>;
#[derive(sqlx::FromRow)]
struct Recovery {
    owner: String,
    repo: String,
    repository_id: i64,
    baseline_sha: String,
    candidate_sha: String,
    candidate_tree: String,
    manifest_digest: String,
    artifact: String,
}
trait Remote: Send + Sync {
    fn reconcile<'a>(&'a self, recovery: &'a Recovery, files: &'a Files) -> RemoteFuture<'a>;

    fn publish<'a>(
        &'a self,
        destination: &'a str,
        id: i64,
        baseline: &'a str,
        files: &'a Files,
        intent: &'a Intent,
    ) -> RemoteFuture<'a>;
}
#[cfg(test)]
type FixtureQueue = std::sync::Mutex<
    std::collections::VecDeque<(reqwest::Method, String, Option<Value>, Result<Value, ()>)>,
>;
struct GitHub {
    client: reqwest::Client,
    token: reqwest::header::HeaderValue,
    #[cfg(test)]
    fixtures: Option<FixtureQueue>,
}
impl GitHub {
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, ()> {
        #[cfg(test)]
        if let Some(fixtures) = &self.fixtures {
            let (expected_method, expected_path, expected_body, response) = fixtures
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected remote operation");
            assert_eq!(method, expected_method);
            assert_eq!(path, expected_path);
            assert_eq!(body, expected_body);
            return response;
        }
        let mut req = self
            .client
            .request(method, format!("https://api.github.com{path}"))
            .header("Authorization", self.token.clone())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(body) = body {
            req = req
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }
        let mut response = req.send().await.map_err(|_| ())?;
        if !response.status().is_success() {
            return Err(());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
            if chunk.len() > 524288 - bytes.len() {
                return Err(());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| ())
    }
    async fn verify_candidate(
        &self,
        root: &str,
        r: &Recovery,
        files: &Files,
    ) -> Result<(), RemoteFailure> {
        use reqwest::Method;
        let commit = self
            .call(
                Method::GET,
                &format!("{root}/git/commits/{}", r.candidate_sha),
                None,
            )
            .await?;
        if sha(&commit["sha"])? != r.candidate_sha
            || sha(&commit["tree"]["sha"])? != r.candidate_tree
            || commit["parents"].as_array().map(Vec::len) != Some(1)
            || sha(&commit["parents"][0]["sha"])? != r.baseline_sha
        {
            return Err(RemoteFailure::Conflict);
        }
        let tree = self
            .call(
                Method::GET,
                &format!("{root}/git/trees/{}?recursive=1", r.candidate_tree),
                None,
            )
            .await?;
        if sha(&tree["sha"])? != r.candidate_tree || tree["truncated"].as_bool() != Some(false) {
            return Err(RemoteFailure::Conflict);
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in tree["tree"].as_array().ok_or(())? {
            let path = entry["path"].as_str().ok_or(())?;
            if !seen.insert(path) {
                return Err(RemoteFailure::Conflict);
            }
            if entry["type"].as_str() == Some("tree") {
                if files.contains_key(path)
                    || entry["mode"].as_str() != Some("040000")
                    || !files.keys().any(|f| f.starts_with(&format!("{path}/")))
                {
                    return Err(RemoteFailure::Conflict);
                }
                continue;
            }
            let content = files.get(path).ok_or(())?;
            if entry["type"].as_str() != Some("blob") || entry["mode"].as_str() != Some("100644") {
                return Err(RemoteFailure::Conflict);
            }
            // Compare canonical Git blob identifiers, calculated over exact stored UTF-8 bytes.
            let blob = sha(&entry["sha"])?;
            let actual = self
                .call(Method::GET, &format!("{root}/git/blobs/{blob}"), None)
                .await?;
            if sha(&actual["sha"])? != blob || actual["encoding"].as_str() != Some("base64") {
                return Err(RemoteFailure::Conflict);
            }
            if actual["content"]
                .as_str()
                .ok_or(())?
                .chars()
                .filter(|c| !c.is_ascii_whitespace())
                .collect::<String>()
                != encode_base64(content.as_bytes())
            {
                return Err(RemoteFailure::Conflict);
            }
        }
        if !files.keys().all(|p| seen.contains(p.as_str())) {
            return Err(RemoteFailure::Conflict);
        }
        Ok(())
    }
    async fn run(
        &self,
        destination: &str,
        id: i64,
        baseline: &str,
        files: &Files,
        intent: &Intent,
    ) -> Result<String, RemoteFailure> {
        use reqwest::Method;
        let root = format!("/repos/{destination}");
        let repo = self.call(Method::GET, &root, None).await?;
        verify_repository(&repo, destination, id)?;
        let reference = format!("{root}/git/ref/heads/main");
        let head = self.call(Method::GET, &reference, None).await?;
        if branch_sha(&head)? != baseline {
            return Err(RemoteFailure::Conflict);
        }
        let commit = self
            .call(Method::GET, &format!("{root}/git/commits/{baseline}"), None)
            .await?;
        if sha(&commit["sha"])? != baseline {
            return Err(RemoteFailure::Conflict);
        }
        let tree = sha(&commit["tree"]["sha"])?;
        let existing = self
            .call(Method::GET, &format!("{root}/git/trees/{tree}"), None)
            .await?;
        // Only an explicitly approved empty tree is accepted; never replace existing files.
        if sha(&existing["sha"])? != tree
            || existing["truncated"].as_bool() != Some(false)
            || !existing["tree"].as_array().is_some_and(Vec::is_empty)
        {
            return Err(RemoteFailure::Conflict);
        }
        let entries:Vec<Value>=files.iter().map(|(path,content)|json!({"path":path,"mode":"100644","type":"blob","content":content})).collect();
        let tree = self
            .call(
                Method::POST,
                &format!("{root}/git/trees"),
                Some(json!({"tree":entries})),
            )
            .await?;
        let tree = sha(&tree["sha"])?;
        let commit=self.call(Method::POST,&format!("{root}/git/commits"),Some(json!({"message":"Publish Panggung generated project","tree":tree,"parents":[baseline]}))).await?;
        let commit = sha(&commit["sha"])?;
        let (owner, repo) = destination.split_once('/').ok_or(())?;
        let recovery = Recovery {
            owner: owner.into(),
            repo: repo.into(),
            repository_id: id,
            baseline_sha: baseline.into(),
            candidate_sha: commit.clone(),
            candidate_tree: tree.clone(),
            manifest_digest: String::new(),
            artifact: String::new(),
        };
        self.verify_candidate(&root, &recovery, files).await?;
        intent.candidate(&commit, &tree).await?;
        let head = self.call(Method::GET, &reference, None).await?;
        if branch_sha(&head)? != baseline {
            return Err(RemoteFailure::Conflict);
        }
        let updated = self
            .call(
                Method::PATCH,
                &format!("{root}/git/refs/heads/main"),
                Some(json!({"sha":commit,"force":false})),
            )
            .await?;
        if branch_sha(&updated).map_err(|_| RemoteFailure::Unresolved)? != commit {
            return Err(RemoteFailure::Unresolved);
        }
        self.reconcile(&recovery, files).await
    }
}
fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        out.push(ALPHABET[(a >> 2) as usize] as char);
        out.push(ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
impl Remote for GitHub {
    fn reconcile<'a>(&'a self, r: &'a Recovery, files: &'a Files) -> RemoteFuture<'a> {
        Box::pin(async move {
            use reqwest::Method;
            let destination = format!("{}/{}", r.owner, r.repo);
            let root = format!("/repos/{destination}");
            let repo = self.call(Method::GET, &root, None).await?;
            verify_repository(&repo, &destination, r.repository_id)?;
            let head = self
                .call(Method::GET, &format!("{root}/git/ref/heads/main"), None)
                .await?;
            let head_sha = branch_sha(&head)?;
            if head_sha != r.candidate_sha {
                return Err(if head_sha == r.baseline_sha {
                    RemoteFailure::Unresolved
                } else {
                    RemoteFailure::Conflict
                });
            }
            self.verify_candidate(&root, r, files).await?;
            let final_head = self
                .call(Method::GET, &format!("{root}/git/ref/heads/main"), None)
                .await?;
            if branch_sha(&final_head)? != r.candidate_sha {
                return Err(RemoteFailure::Conflict);
            }
            Ok(r.candidate_sha.clone())
        })
    }

    fn publish<'a>(
        &'a self,
        destination: &'a str,
        id: i64,
        baseline: &'a str,
        files: &'a Files,
        intent: &'a Intent,
    ) -> RemoteFuture<'a> {
        Box::pin(self.run(destination, id, baseline, files, intent))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    job_id: i64,
    owner: String,
    repo: String,
    baseline: String,
    confirmation: String,
}
#[derive(Serialize, sqlx::FromRow)]
struct Record {
    owner: String,
    repo: String,
    job_id: i64,
    state: String,
    commit_sha: Option<String>,
    created_at: i64,
    mutation_stage: String,
    candidate_sha: Option<String>,
    outcome: String,
}
#[derive(Serialize)]
struct Page {
    enabled: bool,
    items: Vec<Record>,
    has_more: bool,
}
pub fn routes(pool: SqlitePool) -> Result<Router, reqwest::Error> {
    let orgs: Vec<String> = std::env::var("PANGGUNG_GITHUB_PUBLISH_ORGS")
        .ok()
        .and_then(|v| {
            let parts: Vec<_> = v.split(',').map(str::trim).collect();
            if parts.len() > 100 || !parts.iter().all(|s| crate::publish::valid_owner(s)) {
                return None;
            }
            Some(parts.into_iter().map(str::to_ascii_lowercase).collect())
        })
        .unwrap_or_default();
    let destinations: Vec<String> = std::env::var("PANGGUNG_GITHUB_ARTIFACT_DESTINATIONS")
        .ok()
        .and_then(|v| {
            let entries: Vec<_> = v.split(',').map(str::trim).collect();
            if entries.len() > 100
                || !entries.iter().all(|s| {
                    s.split_once('/').is_some_and(|(o, r)| {
                        crate::publish::valid_owner(o) && crate::publish::valid_repo(r)
                    })
                })
            {
                return None;
            }
            Some(entries.into_iter().map(str::to_ascii_lowercase).collect())
        })
        .unwrap_or_default();
    let token = std::env::var("PANGGUNG_GITHUB_ARTIFACT_TOKEN")
        .ok()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 512
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .and_then(|s| reqwest::header::HeaderValue::from_str(&format!("Bearer {s}")).ok());
    let remote: Option<Arc<dyn Remote>> =
        if let Some(mut token) = token.filter(|_| !orgs.is_empty() && !destinations.is_empty()) {
            token.set_sensitive(true);
            Some(Arc::new(GitHub {
                token,
                #[cfg(test)]
                fixtures: None,
                client: reqwest::Client::builder()
                    .https_only(true)
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .retry(reqwest::retry::never())
                    .connect_timeout(Duration::from_secs(5))
                    .timeout(Duration::from_secs(15))
                    .user_agent("panggung-artifact-publish")
                    .build()?,
            }))
        } else {
            None
        };
    Ok(router(StateData {
        pool,
        remote,
        orgs,
        destinations,
    }))
}
fn router(state: StateData) -> Router {
    Router::new()
        .route(
            "/api/github/artifact-publish",
            get(list).post(observed_publish),
        )
        .route("/api/github/artifact-publish/{job}", get(detail))
        .route(
            "/api/github/artifact-publish/{job}/reconcile",
            post(observed_reconcile),
        )
        .layer(DefaultBodyLimit::max(2048))
        .with_state(state)
}
#[derive(Deserialize, Default)]
struct Pagination {
    #[serde(default)]
    offset: u32,
}
const RECORD: &str =
    "owner,repo,job_id,state,commit_sha,created_at,mutation_stage,candidate_sha,outcome";
async fn list(
    State(state): State<StateData>,
    Extension(user): Extension<User>,
    Query(page): Query<Pagination>,
) -> Result<Json<Page>, Error> {
    admin(&user)?;
    if page.offset > 10000 {
        return Err(INVALID);
    }
    let mut items:Vec<Record>=sqlx::query_as(&format!("SELECT {RECORD} FROM github_artifact_publications ORDER BY created_at DESC,owner,repo LIMIT 21 OFFSET ?")).bind(page.offset).fetch_all(&state.pool).await.map_err(db)?;
    let has_more = items.len() > 20;
    items.truncate(20);
    Ok(Json(Page {
        enabled: state.remote.is_some(),
        items,
        has_more,
    }))
}
async fn detail(
    State(state): State<StateData>,
    Extension(user): Extension<User>,
    Path(job): Path<i64>,
) -> Result<Json<Record>, Error> {
    admin(&user)?;
    sqlx::query_as(&format!(
        "SELECT {RECORD} FROM github_artifact_publications WHERE job_id=?"
    ))
    .bind(job)
    .fetch_optional(&state.pool)
    .await
    .map_err(db)?
    .map(Json)
    .ok_or((StatusCode::NOT_FOUND, "Publication not found"))
}
async fn observed_reconcile(
    State(state): State<StateData>,
    Extension(user): Extension<User>,
    metrics: Option<Extension<std::sync::Arc<crate::observability::Metrics>>>,
    trace: Option<Extension<crate::observability::RequestTrace>>,
    Path(job): Path<i64>,
) -> Result<Json<Record>, Error> {
    use crate::observability::{Operation, OperationOutcome};
    let guard = metrics
        .as_ref()
        .map(|Extension(m)| m.operation(Operation::Publish, trace.as_ref().map(|Extension(t)| t)));
    let result = reconcile(State(state), Extension(user), Path(job)).await;
    if let Some(g) = guard {
        g.finish(match &result {
            Ok(_) => OperationOutcome::Success,
            Err((status, _)) if status.is_client_error() => OperationOutcome::Rejected,
            _ => OperationOutcome::Failure,
        });
    }
    result
}
async fn reconcile(
    State(state): State<StateData>,
    Extension(user): Extension<User>,
    Path(job): Path<i64>,
) -> Result<Json<Record>, Error> {
    admin(&user)?;
    let remote = state.remote.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Artifact publishing disabled",
    ))?;
    let r:Recovery=sqlx::query_as("SELECT p.owner,p.repo,p.repository_id,p.baseline_sha,p.candidate_sha,p.candidate_tree,p.manifest_digest,j.artifact FROM github_artifact_publications p JOIN scaffolder_jobs j ON j.id=p.job_id WHERE p.job_id=? AND p.state='uncertain' AND p.mutation_stage='candidate' AND p.candidate_sha IS NOT NULL AND p.candidate_tree IS NOT NULL AND p.baseline_sha IS NOT NULL AND p.repository_id IS NOT NULL AND p.manifest_digest IS NOT NULL").bind(job).fetch_optional(&state.pool).await.map_err(db)?.ok_or((StatusCode::CONFLICT,"No recoverable candidate; reservation retained"))?;
    if !state.orgs.contains(&r.owner)
        || !state
            .destinations
            .contains(&format!("{}/{}", r.owner, r.repo))
    {
        return Err((StatusCode::FORBIDDEN, "Destination not allowed"));
    }
    if format!("{:x}", Sha256::digest(r.artifact.as_bytes())) != r.manifest_digest
        || [&r.baseline_sha, &r.candidate_sha, &r.candidate_tree]
            .iter()
            .any(|v| sha(&json!(v)).is_err())
    {
        return Err(INVALID);
    }
    let files = publication_files(&r.artifact)?;
    let result = tokio::time::timeout(Duration::from_secs(90), remote.reconcile(&r, &files)).await;
    let confirmed = matches!(result,Ok(Ok(ref v)) if v==&r.candidate_sha);
    let mut tx = state.pool.begin().await.map_err(db)?;
    let outcome = if confirmed {
        "confirmed"
    } else if matches!(result, Ok(Err(RemoteFailure::Conflict))) {
        "conflict"
    } else {
        "unresolved"
    };
    if confirmed {
        sqlx::query("UPDATE github_artifact_publications SET state='succeeded',commit_sha=candidate_sha,mutation_stage='confirmed',outcome='confirmed',completed_at=unixepoch() WHERE job_id=? AND state='uncertain' AND candidate_sha=?").bind(job).bind(&r.candidate_sha).execute(&mut *tx).await.map_err(db)?;
    } else {
        sqlx::query("UPDATE github_artifact_publications SET outcome=? WHERE job_id=? AND state='uncertain' AND candidate_sha=?").bind(outcome).bind(job).bind(&r.candidate_sha).execute(&mut *tx).await.map_err(db)?;
    }
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
        .bind(user.id)
        .bind(format!("artifact_reconcile:{}:{job}", outcome))
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    tx.commit().await.map_err(db)?;
    if !confirmed {
        return Err(UNCERTAIN);
    }
    detail(State(state), Extension(user), Path(job)).await
}
async fn observed_publish(
    State(state): State<StateData>,
    Extension(user): Extension<User>,
    metrics: Option<Extension<std::sync::Arc<crate::observability::Metrics>>>,
    trace: Option<Extension<crate::observability::RequestTrace>>,
    Json(input): Json<Input>,
) -> Result<Json<Record>, Error> {
    use crate::observability::{Operation, OperationOutcome};
    let guard = metrics
        .as_ref()
        .map(|Extension(m)| m.operation(Operation::Publish, trace.as_ref().map(|Extension(t)| t)));
    let result = publish(State(state), Extension(user), Json(input)).await;
    if let Some(g) = guard {
        g.finish(match &result {
            Ok(_) => OperationOutcome::Success,
            Err((status, _)) if status.is_client_error() => OperationOutcome::Rejected,
            _ => OperationOutcome::Failure,
        });
    }
    result
}
async fn publish(
    State(state): State<StateData>,
    Extension(user): Extension<User>,
    Json(input): Json<Input>,
) -> Result<Json<Record>, Error> {
    admin(&user)?;
    let owner = input.owner.to_ascii_lowercase();
    let repo = input.repo.to_ascii_lowercase();
    let destination = format!("{owner}/{repo}");
    if input.job_id <= 0
        || !crate::publish::valid_owner(&owner)
        || !crate::publish::valid_repo(&repo)
        || sha(&json!(input.baseline)).is_err()
        || input.confirmation != format!("{destination}@{}#{}", input.baseline, input.job_id)
    {
        return Err(INVALID);
    }
    if !state.orgs.contains(&owner) || !state.destinations.contains(&destination) {
        return Err((
            StatusCode::FORBIDDEN,
            "Organization or destination not allowed",
        ));
    }
    let remote = state.remote.ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Artifact publishing disabled",
    ))?;
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await.map_err(db)?;
    // Require the same administrator to own the generated job and destination creation.
    let row:Option<(i64,String)>=sqlx::query_as("SELECT p.github_id,j.artifact FROM github_publish_attempts p JOIN scaffolder_jobs j ON j.id=? WHERE p.owner=? AND p.repo=? AND p.state='succeeded' AND p.actor_id=? AND j.actor_id=?").bind(input.job_id).bind(&owner).bind(&repo).bind(user.id).bind(user.id).fetch_optional(&mut *tx).await.map_err(db)?;
    let (id, body) = row.ok_or((
        StatusCode::NOT_FOUND,
        "Owned job or successful repository creation not found",
    ))?;
    let reserved: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM github_artifact_publications WHERE job_id=? OR (owner=? AND repo=?))").bind(input.job_id).bind(&owner).bind(&repo).fetch_one(&mut *tx).await.map_err(db)?;
    if reserved {
        return Err((
            StatusCode::CONFLICT,
            "Job or destination already reserved; inspect history",
        ));
    }
    let files = publication_files(&body)?;
    let inserted=sqlx::query("INSERT INTO github_artifact_publications(owner,repo,job_id,actor_id,state,baseline_sha,repository_id,manifest_digest,mutation_stage) VALUES(?,?,?,?,'uncertain',?,?,?,'reserved') ON CONFLICT DO NOTHING").bind(&owner).bind(&repo).bind(input.job_id).bind(user.id).bind(&input.baseline).bind(id).bind(format!("{:x}",Sha256::digest(body.as_bytes()))).execute(&mut *tx).await.map_err(db)?;
    if inserted.rows_affected() == 0 {
        return Err((
            StatusCode::CONFLICT,
            "Job or destination already reserved; inspect history",
        ));
    }
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
        .bind(user.id)
        .bind(format!(
            "artifact_publish_reserved:{destination}:{}",
            input.job_id
        ))
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    tx.commit().await.map_err(db)?;
    let intent = Intent {
        pool: state.pool.clone(),
        job_id: input.job_id,
        actor_id: user.id,
    };
    let result = tokio::time::timeout(
        Duration::from_secs(90),
        remote.publish(&destination, id, &input.baseline, &files, &intent),
    )
    .await;
    let commit = match result {
        Ok(Ok(value)) if sha(&json!(value)).is_ok() => value,
        _ => {
            let mut tx = state.pool.begin().await.map_err(db)?;
            let rejected = sqlx::query("UPDATE github_artifact_publications SET mutation_stage='rejected',outcome='rejected',completed_at=unixepoch() WHERE job_id=? AND mutation_stage='reserved'").bind(input.job_id).execute(&mut *tx).await.map_err(db)?.rows_affected()==1;
            let outcome = if rejected {
                "rejected"
            } else if matches!(result, Ok(Err(RemoteFailure::Conflict))) {
                "conflict"
            } else {
                "unresolved"
            };
            if !rejected {
                sqlx::query("UPDATE github_artifact_publications SET outcome=? WHERE job_id=? AND state='uncertain'").bind(outcome).bind(input.job_id).execute(&mut *tx).await.map_err(db)?;
            }
            sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
                .bind(user.id)
                .bind(format!(
                    "artifact_publish_failed:{outcome}:{destination}:{}",
                    input.job_id
                ))
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            tx.commit().await.map_err(db)?;
            return Err(if rejected {
                (
                    StatusCode::BAD_GATEWAY,
                    "Publication failed before branch update; reservation retained",
                )
            } else {
                UNCERTAIN
            });
        }
    };
    let mut tx = state.pool.begin().await.map_err(db)?;
    let record=sqlx::query_as("UPDATE github_artifact_publications SET state='succeeded',commit_sha=?,mutation_stage='confirmed',outcome='confirmed',completed_at=unixepoch() WHERE owner=? AND repo=? AND candidate_sha=? AND mutation_stage IN ('candidate','confirmed') RETURNING owner,repo,job_id,state,commit_sha,created_at,mutation_stage,candidate_sha,outcome").bind(&commit).bind(&owner).bind(&repo).bind(&commit).fetch_one(&mut *tx).await.map_err(db)?;
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
        .bind(user.id)
        .bind(format!(
            "artifact_publish_succeeded:{destination}:{}",
            input.job_id
        ))
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    tx.commit().await.map_err(db)?;
    Ok(Json(record))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::IdentityStore;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[tokio::test]
    async fn canonical_recovery_reads_only_and_checks_every_file() {
        use reqwest::Method;
        let candidate = "b".repeat(40);
        let baseline = "a".repeat(40);
        let tree = "c".repeat(40);
        let blob = "d".repeat(40);
        let root = "/repos/allowed/service";
        let recovery = Recovery {
            owner: "allowed".into(),
            repo: "service".into(),
            repository_id: 123,
            baseline_sha: baseline.clone(),
            candidate_sha: candidate.clone(),
            candidate_tree: tree.clone(),
            manifest_digest: String::new(),
            artifact: String::new(),
        };
        let files = Files::from([("main.py".into(), "hello".into())]);
        for defect in [
            "none",
            "privacy",
            "identity",
            "name",
            "archive",
            "archive_missing",
            "default_branch",
            "wrong_ref",
            "missing_ref",
            "wrong_type",
            "missing_type",
            "bad_sha",
            "head",
            "parent",
            "truncated",
            "content",
            "missing",
            "final_ref",
        ] {
            let mut fixtures = std::collections::VecDeque::new();
            let mut add = |path: String, value: Value| {
                fixtures.push_back((Method::GET, path, None, Ok(value)))
            };
            let mut repository = json!({"id":if defect=="identity"{124}else{123},"private":defect!="privacy","full_name":if defect=="name"{"allowed/other"}else{"allowed/service"},"default_branch":if defect=="default_branch"{"other"}else{"main"},"archived":defect=="archive"});
            if defect == "archive_missing" {
                repository.as_object_mut().unwrap().remove("archived");
            }
            add(root.into(), repository);
            if !matches!(
                defect,
                "privacy" | "identity" | "name" | "archive" | "archive_missing" | "default_branch"
            ) {
                let mut head = json!({"ref":"refs/heads/main","object":{"type":"commit","sha":if defect=="head"{&baseline}else{&candidate}}});
                match defect {
                    "wrong_ref" => head["ref"] = json!("refs/heads/main-other"),
                    "missing_ref" => {
                        head.as_object_mut().unwrap().remove("ref");
                    }
                    "wrong_type" => head["object"]["type"] = json!("tag"),
                    "missing_type" => {
                        head["object"].as_object_mut().unwrap().remove("type");
                    }
                    "bad_sha" => head["object"]["sha"] = json!("invalid"),
                    _ => {}
                }
                add(format!("{root}/git/ref/heads/main"), head);
                if !matches!(
                    defect,
                    "head"
                        | "wrong_ref"
                        | "missing_ref"
                        | "wrong_type"
                        | "missing_type"
                        | "bad_sha"
                ) {
                    add(
                        format!("{root}/git/commits/{candidate}"),
                        json!({"sha":candidate,"tree":{"sha":tree},"parents":[{"sha":if defect=="parent"{&candidate}else{&baseline}}]}),
                    );
                    if defect != "parent" {
                        add(
                            format!("{root}/git/trees/{tree}?recursive=1"),
                            json!({"sha":tree,"truncated":defect=="truncated","tree":if defect=="missing"{json!([])}else{json!([{"path":"main.py","mode":"100644","type":"blob","sha":blob}])}}),
                        );
                        if !matches!(defect, "truncated" | "missing") {
                            add(
                                format!("{root}/git/blobs/{blob}"),
                                json!({"sha":blob,"encoding":"base64","content":if defect=="content"{"YmFk"}else{"aGVsbG8="}}),
                            );
                        }
                    }
                }
            }
            if matches!(defect, "none" | "final_ref") {
                fixtures.push_back((
                    Method::GET,
                    format!("{root}/git/ref/heads/main"),
                    None,
                    Ok(json!({"ref":if defect=="final_ref"{"refs/heads/other"}else{"refs/heads/main"},"object":{"type":"commit","sha":candidate}})),
                ));
            }
            let remote = GitHub {
                client: reqwest::Client::new(),
                token: reqwest::header::HeaderValue::from_static("synthetic"),
                fixtures: Some(std::sync::Mutex::new(fixtures)),
            };
            assert_eq!(
                remote.reconcile(&recovery, &files).await.is_ok(),
                defect == "none",
                "{defect}"
            );
            assert!(remote.fixtures.as_ref().unwrap().lock().unwrap().is_empty());
        }
    }
    #[tokio::test]
    async fn mutation_protocol_never_updates_without_durable_candidate() {
        use reqwest::Method;
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::database::open(&dir.path().join("mutation.db"))
            .await
            .unwrap();
        let user = IdentityStore::new(pool.clone())
            .bootstrap_admin("admin", "synthetic-test-password-123")
            .await
            .unwrap();
        let template: (String, String) = sqlx::query_as(
            "SELECT name,version FROM software_template_versions WHERE stack='python' LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let baseline = "a".repeat(40);
        let candidate = "b".repeat(40);
        let tree = "c".repeat(40);
        let empty = "d".repeat(40);
        let blob = "e".repeat(40);
        let root = "/repos/allowed/service";
        let files = Files::from([("main.py".into(), "hello".into())]);
        sqlx::query(
            "INSERT INTO scaffolder_jobs(id,actor_id,name,version,artifact) VALUES(1,?,?,?,'{}')",
        )
        .bind(user.id)
        .bind(&template.0)
        .bind(&template.1)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO github_publish_attempts(owner,repo,state,github_id,actor_id,completed_at) VALUES('allowed','service','succeeded',123,?,unixepoch())").bind(user.id).execute(&pool).await.unwrap();
        for defect in [
            "nonempty",
            "truncated",
            "database",
            "conflict",
            "prepatch_ref",
            "ambiguous",
            "patch_type",
            "success",
        ] {
            // Only disposable synthetic reservations are removed between cases.
            sqlx::query("DELETE FROM github_artifact_publications")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO github_artifact_publications(owner,repo,job_id,actor_id,state,baseline_sha,repository_id,manifest_digest,mutation_stage) VALUES('allowed','service',1,?,'uncertain',?,123,?,'reserved')").bind(user.id).bind(&baseline).bind("f".repeat(64)).execute(&pool).await.unwrap();
            let intent = Intent {
                pool: pool.clone(),
                job_id: 1,
                actor_id: user.id,
            };
            if defect == "database" {
                sqlx::query("CREATE TRIGGER fail_candidate BEFORE UPDATE ON github_artifact_publications WHEN NEW.mutation_stage='candidate' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END").execute(&pool).await.unwrap();
            }
            let repo = json!({"id":123,"private":true,"full_name":"allowed/service","default_branch":"main","archived":false});
            let mut fixtures = std::collections::VecDeque::new();
            let mut add = |method, path: String, body, response| {
                fixtures.push_back((method, path, body, response))
            };
            add(Method::GET, root.into(), None, Ok(repo.clone()));
            add(
                Method::GET,
                format!("{root}/git/ref/heads/main"),
                None,
                Ok(json!({"ref":"refs/heads/main","object":{"type":"commit","sha":baseline}})),
            );
            add(
                Method::GET,
                format!("{root}/git/commits/{baseline}"),
                None,
                Ok(json!({"sha":baseline,"tree":{"sha":empty}})),
            );
            add(
                Method::GET,
                format!("{root}/git/trees/{empty}"),
                None,
                Ok(
                    json!({"sha":empty,"truncated":defect=="truncated","tree":if defect=="nonempty"{json!([{"path":"README.md"}])}else{json!([])}}),
                ),
            );
            if !matches!(defect, "nonempty" | "truncated") {
                add(
                    Method::POST,
                    format!("{root}/git/trees"),
                    Some(
                        json!({"tree":[{"path":"main.py","mode":"100644","type":"blob","content":"hello"}]}),
                    ),
                    Ok(json!({"sha":tree})),
                );
                add(
                    Method::POST,
                    format!("{root}/git/commits"),
                    Some(
                        json!({"message":"Publish Panggung generated project","tree":tree,"parents":[baseline]}),
                    ),
                    Ok(json!({"sha":candidate})),
                );
                add(
                    Method::GET,
                    format!("{root}/git/commits/{candidate}"),
                    None,
                    Ok(json!({"sha":candidate,"tree":{"sha":tree},"parents":[{"sha":baseline}]})),
                );
                add(
                    Method::GET,
                    format!("{root}/git/trees/{tree}?recursive=1"),
                    None,
                    Ok(
                        json!({"sha":tree,"truncated":false,"tree":[{"path":"main.py","mode":"100644","type":"blob","sha":blob}]}),
                    ),
                );
                add(
                    Method::GET,
                    format!("{root}/git/blobs/{blob}"),
                    None,
                    Ok(json!({"sha":blob,"encoding":"base64","content":"aGVsbG8="})),
                );
                if defect != "database" {
                    add(
                        Method::GET,
                        format!("{root}/git/ref/heads/main"),
                        None,
                        Ok(
                            json!({"ref":if defect=="prepatch_ref"{"refs/heads/other"}else{"refs/heads/main"},"object":{"type":"commit","sha":if defect=="conflict"{&blob}else{&baseline}}}),
                        ),
                    );
                    if !matches!(defect, "conflict" | "prepatch_ref") {
                        add(
                            Method::PATCH,
                            format!("{root}/git/refs/heads/main"),
                            Some(json!({"sha":candidate,"force":false})),
                            if defect == "ambiguous" {
                                Err(())
                            } else {
                                Ok(
                                    json!({"ref":"refs/heads/main","object":{"type":if defect=="patch_type"{"tag"}else{"commit"},"sha":candidate}}),
                                )
                            },
                        );
                        if defect == "success" {
                            add(Method::GET, root.into(), None, Ok(repo));
                            add(
                                Method::GET,
                                format!("{root}/git/ref/heads/main"),
                                None,
                                Ok(
                                    json!({"ref":"refs/heads/main","object":{"type":"commit","sha":candidate}}),
                                ),
                            );
                            add(
                                Method::GET,
                                format!("{root}/git/commits/{candidate}"),
                                None,
                                Ok(
                                    json!({"sha":candidate,"tree":{"sha":tree},"parents":[{"sha":baseline}]}),
                                ),
                            );
                            add(
                                Method::GET,
                                format!("{root}/git/trees/{tree}?recursive=1"),
                                None,
                                Ok(
                                    json!({"sha":tree,"truncated":false,"tree":[{"path":"main.py","mode":"100644","type":"blob","sha":blob}]}),
                                ),
                            );
                            add(
                                Method::GET,
                                format!("{root}/git/blobs/{blob}"),
                                None,
                                Ok(json!({"sha":blob,"encoding":"base64","content":"aGVsbG8="})),
                            );
                        }
                    }
                }
            }
            if defect == "success" {
                fixtures.push_back((
                    Method::GET,
                    format!("{root}/git/ref/heads/main"),
                    None,
                    Ok(json!({"ref":"refs/heads/main","object":{"type":"commit","sha":candidate}})),
                ));
            }
            let remote = GitHub {
                client: reqwest::Client::new(),
                token: reqwest::header::HeaderValue::from_static("synthetic"),
                fixtures: Some(std::sync::Mutex::new(fixtures)),
            };
            let result = remote
                .run("allowed/service", 123, &baseline, &files, &intent)
                .await;
            assert_eq!(result.is_ok(), defect == "success", "{defect}");
            if defect == "patch_type" {
                assert_eq!(result, Err(RemoteFailure::Unresolved));
            }
            if defect == "prepatch_ref" {
                assert_eq!(result, Err(RemoteFailure::Conflict));
            }
            assert!(remote.fixtures.as_ref().unwrap().lock().unwrap().is_empty());
            let record:(String,Option<String>)=sqlx::query_as("SELECT mutation_stage,candidate_sha FROM github_artifact_publications WHERE job_id=1").fetch_one(&pool).await.unwrap();
            if matches!(defect, "nonempty" | "truncated" | "database") {
                assert_eq!(record, ("reserved".into(), None));
            } else {
                assert_eq!(record, ("candidate".into(), Some(candidate.clone())));
            }
            if defect == "database" {
                sqlx::query("DROP TRIGGER fail_candidate")
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }
        pool.close().await;
    }
    #[test]
    fn manifest_duplicate_keys_are_rejected() {
        let body = r#"{"name":"test","version":"1.0.0","stack":"python","files":{"README.md":"one","README.md":"two","panggung.json":"{}","main.py":"pass"}}"#;
        assert!(publication_files(body).is_err());
    }
    struct Fake(AtomicUsize);
    impl Remote for Fake {
        fn reconcile<'a>(&'a self, r: &'a Recovery, _: &'a Files) -> RemoteFuture<'a> {
            Box::pin(async move { Ok(r.candidate_sha.clone()) })
        }

        fn publish<'a>(
            &'a self,
            destination: &'a str,
            id: i64,
            baseline: &'a str,
            files: &'a Files,
            intent: &'a Intent,
        ) -> RemoteFuture<'a> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                assert_eq!(id, 123);
                assert_eq!(baseline, "a".repeat(40));
                assert_eq!(files.len(), 3);
                assert_eq!(files["main.py"], "print('synthetic')");
                intent.candidate(&"b".repeat(40), &"c".repeat(40)).await?;
                if destination.ends_with("/uncertain") {
                    Err(RemoteFailure::Unresolved)
                } else {
                    Ok("b".repeat(40))
                }
            })
        }
    }
    fn input(job_id: i64, repo: &str) -> Input {
        let baseline = "a".repeat(40);
        Input {
            job_id,
            owner: "allowed".into(),
            repo: repo.into(),
            confirmation: format!("allowed/{repo}@{baseline}#{job_id}"),
            baseline,
        }
    }
    #[tokio::test]
    async fn concurrent_publications_reserve_job_and_destination_once() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::database::open(&dir.path().join("duplicate.db"))
            .await
            .unwrap();
        let user = IdentityStore::new(pool.clone())
            .bootstrap_admin("admin", "synthetic-test-password-123")
            .await
            .unwrap();
        let template: (String, String) = sqlx::query_as(
            "SELECT name,version FROM software_template_versions WHERE stack='python' LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let artifact = json!({"name":"python-starter","version":"1.0.0","stack":"python","files":{"README.md":"synthetic","panggung.json":"{}","main.py":"print('synthetic')"}}).to_string();
        for job in 1..=5_i64 {
            sqlx::query(
                "INSERT INTO scaffolder_jobs(id,actor_id,name,version,artifact) VALUES(?,?,?,?,?)",
            )
            .bind(job)
            .bind(user.id)
            .bind(&template.0)
            .bind(&template.1)
            .bind(&artifact)
            .execute(&pool)
            .await
            .unwrap();
        }
        for repo in ["same", "job-a", "job-b", "destination"] {
            sqlx::query("INSERT INTO github_publish_attempts(owner,repo,state,github_id,actor_id,completed_at) VALUES('allowed',?,'succeeded',123,?,unixepoch())")
                .bind(repo).bind(user.id).execute(&pool).await.unwrap();
        }
        let fake = Arc::new(Fake(AtomicUsize::new(0)));
        let state = StateData {
            pool: pool.clone(),
            remote: Some(fake.clone()),
            orgs: vec!["allowed".into()],
            destinations: ["same", "job-a", "job-b", "destination"]
                .map(|repo| format!("allowed/{repo}"))
                .to_vec(),
        };
        for (left, right) in [
            (input(1, "same"), input(1, "same")),
            (input(2, "job-a"), input(2, "job-b")),
            (input(3, "destination"), input(4, "destination")),
        ] {
            let calls_before = fake.0.load(Ordering::SeqCst);
            let (left, right) = tokio::join!(
                publish(State(state.clone()), Extension(user.clone()), Json(left)),
                publish(State(state.clone()), Extension(user.clone()), Json(right)),
            );
            match (left, right) {
                (Ok(Json(record)), Err((StatusCode::CONFLICT, _)))
                | (Err((StatusCode::CONFLICT, _)), Ok(Json(record))) => {
                    assert_eq!(record.state, "succeeded");
                }
                _ => panic!("exactly one publication must succeed and the duplicate must conflict"),
            }
            assert_eq!(fake.0.load(Ordering::SeqCst), calls_before + 1);
        }
        let confirmed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM github_artifact_publications WHERE state='succeeded' AND mutation_stage='confirmed' AND commit_sha=candidate_sha")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(confirmed, 3);
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM github_artifact_publications")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(total, 3);
        let reservations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM identity_audit_events WHERE action LIKE 'artifact_publish_reserved:%'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(reservations, 3);
        pool.close().await;
    }
    #[tokio::test]
    async fn publication_reservation_is_authorized_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.db");
        let pool = crate::database::open(&path).await.unwrap();
        let identity = IdentityStore::new(pool.clone());
        let user = identity
            .bootstrap_admin("admin", "synthetic-test-password-123")
            .await
            .unwrap();
        let member = identity
            .create_user(
                user.id,
                "member",
                "synthetic-test-password-123",
                Role::Member,
            )
            .await
            .unwrap();
        let artifact=json!({"name":"python-starter","version":"1.0.0","stack":"python","files":{"README.md":"synthetic","panggung.json":"{}","main.py":"print('synthetic')"}}).to_string();
        let template: (String, String) = sqlx::query_as(
            "SELECT name,version FROM software_template_versions WHERE stack='python' LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        for (job, repo) in [(1, "service"), (2, "uncertain"), (3, "malformed")] {
            let body = if job == 3 {
                json!({"name":"bad","version":"1.0.0","stack":"python","files":{"../escape":"bad"}})
                    .to_string()
            } else {
                artifact.clone()
            };
            sqlx::query(
                "INSERT INTO scaffolder_jobs(id,actor_id,name,version,artifact) VALUES(?,?,?,?,?)",
            )
            .bind(job)
            .bind(user.id)
            .bind(&template.0)
            .bind(&template.1)
            .bind(body)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO github_publish_attempts(owner,repo,state,github_id,actor_id,completed_at) VALUES('allowed',?,'succeeded',123,?,unixepoch())").bind(repo).bind(user.id).execute(&pool).await.unwrap();
        }
        let fake = Arc::new(Fake(AtomicUsize::new(0)));
        let state = StateData {
            pool: pool.clone(),
            remote: Some(fake.clone()),
            orgs: vec!["allowed".into()],
            destinations: vec![
                "allowed/service".into(),
                "allowed/uncertain".into(),
                "allowed/malformed".into(),
            ],
        };
        assert_eq!(
            publish(
                State(state.clone()),
                Extension(member),
                Json(input(1, "service"))
            )
            .await
            .err()
            .unwrap()
            .0,
            StatusCode::FORBIDDEN
        );
        let mut bad = input(1, "service");
        bad.confirmation.clear();
        assert_eq!(
            publish(State(state.clone()), Extension(user.clone()), Json(bad))
                .await
                .err()
                .unwrap()
                .0,
            StatusCode::BAD_REQUEST
        );
        assert!(
            publish(
                State(state.clone()),
                Extension(user.clone()),
                Json(input(3, "malformed"))
            )
            .await
            .is_err()
        );
        assert_eq!(fake.0.load(Ordering::SeqCst), 0);
        assert_eq!(
            publish(
                State(state.clone()),
                Extension(user.clone()),
                Json(input(1, "service"))
            )
            .await
            .unwrap()
            .0
            .state,
            "succeeded"
        );
        assert_eq!(
            publish(
                State(state.clone()),
                Extension(user.clone()),
                Json(input(2, "uncertain"))
            )
            .await
            .err()
            .unwrap()
            .0,
            StatusCode::BAD_GATEWAY
        );
        drop(state);
        drop(identity);
        pool.close().await;
        let pool = crate::database::open(&path).await.unwrap();
        let state = StateData {
            pool: pool.clone(),
            remote: Some(fake.clone()),
            orgs: vec!["allowed".into()],
            destinations: vec![
                "allowed/service".into(),
                "allowed/uncertain".into(),
                "allowed/malformed".into(),
            ],
        };
        assert_eq!(
            publish(
                State(state.clone()),
                Extension(user.clone()),
                Json(input(2, "uncertain"))
            )
            .await
            .err()
            .unwrap()
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            publish(
                State(state.clone()),
                Extension(user.clone()),
                Json(input(1, "service"))
            )
            .await
            .err()
            .unwrap()
            .0,
            StatusCode::CONFLICT
        );
        let recovered = reconcile(State(state.clone()), Extension(user.clone()), Path(2))
            .await
            .unwrap()
            .0;
        assert_eq!(recovered.state, "succeeded");
        let page = list(State(state), Extension(user), Query(Pagination::default()))
            .await
            .unwrap()
            .0;
        assert_eq!(page.items.len(), 2);
        assert_eq!(fake.0.load(Ordering::SeqCst), 2);
        pool.close().await;
    }
}
