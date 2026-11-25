//! Signed receipts and bounded canonical repository refresh jobs. No raw payload retention.
use crate::identity::{Role, User};
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::sync::Arc;

type Error = (StatusCode, &'static str);
type Signature = Hmac<Sha256>;
const MAX_BODY: usize = 256 * 1024;
const DISABLED: Error = (StatusCode::SERVICE_UNAVAILABLE, "Webhook disabled");
const INVALID: Error = (StatusCode::BAD_REQUEST, "Invalid webhook request");
const UNAUTHORIZED: Error = (StatusCode::UNAUTHORIZED, "Invalid webhook signature");
const FORBIDDEN: Error = (StatusCode::FORBIDDEN, "Administrator required");

#[derive(Clone)]
struct WebhookState {
    pool: SqlitePool,
    secret: Option<Arc<[u8]>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigInput {
    enabled: bool,
}

#[derive(Serialize)]
struct Configuration {
    enabled: bool,
    secret_configured: bool,
}

#[derive(Serialize)]
struct Receipt {
    accepted: bool,
    duplicate: bool,
}

#[derive(Serialize)]
struct WebhookStatus {
    enabled: bool,
    secret_configured: bool,
    accepted_deliveries: i64,
    last_received_at: Option<i64>,
}

/// Management router must use session authentication; receiver must not.
/// Receiver authenticates the raw body using GitHub's signature.
pub fn routes(pool: SqlitePool) -> Result<(Router, Router), std::io::Error> {
    let secret = match std::env::var("PANGGUNG_GITHUB_WEBHOOK_SECRET") {
        Ok(value) if (32..=1024).contains(&value.len()) => {
            Some(Arc::<[u8]>::from(value.into_bytes()))
        }
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Webhook secret must contain 32–1024 bytes",
            ));
        }
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Invalid webhook secret environment variable",
            ));
        }
    };
    Ok(build_routes(WebhookState { pool, secret }))
}

fn build_routes(state: WebhookState) -> (Router, Router) {
    let management = Router::new()
        .route(
            "/api/webhooks/github/config",
            get(config)
                .put(configure)
                .layer(DefaultBodyLimit::max(1024)),
        )
        .route("/api/webhooks/github/status", get(status))
        .route("/api/webhooks/github/jobs", get(jobs))
        .route(
            "/api/webhooks/github/catalog/{id}",
            axum::routing::put(approve_catalog).layer(DefaultBodyLimit::max(2048)),
        )
        .route(
            "/api/webhooks/github/sources/{id}",
            axum::routing::put(approve_source).delete(revoke_source),
        )
        .with_state(state.clone());
    let receiver = Router::new()
        .route(
            "/api/webhooks/github",
            post(observed_receive).layer(DefaultBodyLimit::max(MAX_BODY)),
        )
        .with_state(state);
    (management, receiver)
}

fn internal(_: sqlx::Error) -> Error {
    tracing::error!("Webhook database operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Webhook database operation failed",
    )
}
fn admin(user: &User) -> Result<(), Error> {
    if user.role != Role::Admin {
        return Err(FORBIDDEN);
    }
    Ok(())
}
async fn enabled(pool: &SqlitePool) -> Result<bool, Error> {
    sqlx::query_scalar("SELECT enabled FROM github_webhook_config WHERE singleton=1")
        .fetch_one(pool)
        .await
        .map_err(internal)
}
async fn config(
    State(state): State<WebhookState>,
    Extension(user): Extension<User>,
) -> Result<Json<Configuration>, Error> {
    admin(&user)?;
    Ok(Json(Configuration {
        enabled: enabled(&state.pool).await?,
        secret_configured: state.secret.is_some(),
    }))
}
async fn configure(
    State(state): State<WebhookState>,
    Extension(user): Extension<User>,
    Json(input): Json<ConfigInput>,
) -> Result<Json<Configuration>, Error> {
    admin(&user)?;
    if input.enabled && state.secret.is_none() {
        return Err((StatusCode::CONFLICT, "Webhook secret not configured"));
    }
    let mut tx = state.pool.begin().await.map_err(internal)?;
    sqlx::query("UPDATE github_webhook_config SET enabled=? WHERE singleton=1")
        .bind(input.enabled)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
        .bind(user.id)
        .bind(if input.enabled {
            "enable_github_webhook"
        } else {
            "disable_github_webhook"
        })
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(Json(Configuration {
        enabled: input.enabled,
        secret_configured: state.secret.is_some(),
    }))
}
async fn status(
    State(state): State<WebhookState>,
    Extension(user): Extension<User>,
) -> Result<Json<WebhookStatus>, Error> {
    admin(&user)?;
    let (enabled,count,last):(bool,i64,Option<i64>)=sqlx::query_as(
        "SELECT enabled,(SELECT COUNT(DISTINCT payload_digest) FROM github_webhook_deliveries),(SELECT MAX(received_at) FROM github_webhook_deliveries) FROM github_webhook_config WHERE singleton=1")
        .fetch_one(&state.pool).await.map_err(internal)?;
    Ok(Json(WebhookStatus {
        enabled,
        secret_configured: state.secret.is_some(),
        accepted_deliveries: count,
        last_received_at: last,
    }))
}
fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(value)
}
fn verify(secret: &[u8], signature: &str, body: &[u8]) -> Result<(), Error> {
    let hex = signature.strip_prefix("sha256=").ok_or(UNAUTHORIZED)?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(UNAUTHORIZED);
    }
    let mut decoded = [0u8; 32];
    for (index, byte) in decoded.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).map_err(|_| UNAUTHORIZED)?;
    }
    let mut mac = Signature::new_from_slice(secret).map_err(|_| UNAUTHORIZED)?;
    mac.update(body);
    mac.verify_slice(&decoded).map_err(|_| UNAUTHORIZED)
}
fn valid_delivery(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
fn valid_event(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
async fn observed_receive(
    State(state): State<WebhookState>,
    metrics: Option<Extension<Arc<crate::observability::Metrics>>>,
    trace: Option<Extension<crate::observability::RequestTrace>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Receipt>), Error> {
    use crate::observability::{Operation, OperationOutcome};
    let guard = metrics
        .as_ref()
        .map(|Extension(m)| m.operation(Operation::Webhook, trace.as_ref().map(|Extension(t)| t)));
    let result = receive(State(state), headers, body).await;
    if let Some(g) = guard {
        g.finish(match &result {
            Ok(_) => OperationOutcome::Success,
            Err((status, _)) if status.is_client_error() => OperationOutcome::Rejected,
            _ => OperationOutcome::Failure,
        });
    }
    result
}
async fn receive(
    State(state): State<WebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Receipt>), Error> {
    let secret = state.secret.as_deref().ok_or(DISABLED)?;
    if !enabled(&state.pool).await? {
        return Err(DISABLED);
    }
    let signature = single_header(&headers, "x-hub-signature-256").ok_or(UNAUTHORIZED)?;
    verify(secret, signature, &body)?;
    let delivery = single_header(&headers, "x-github-delivery")
        .filter(|v| valid_delivery(v))
        .ok_or(INVALID)?
        .to_ascii_lowercase();
    let event = single_header(&headers, "x-github-event")
        .filter(|v| valid_event(v))
        .ok_or(INVALID)?;
    if !single_header(&headers, "content-type").is_some_and(|v| {
        v.split(';')
            .next()
            .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"))
    }) {
        return Err(INVALID);
    }
    let payload: serde_json::Value = serde_json::from_slice(&body).map_err(|_| INVALID)?;
    if !payload.is_object() {
        return Err(INVALID);
    }
    let digest = Sha256::digest(&body).to_vec();
    // First statement is a write. The enabled predicate and unique constraints
    // are evaluated atomically, including concurrent delivery and disable requests.
    // Retain every authenticated delivery identifier, even for duplicate payloads.
    let mut tx = state.pool.begin().await.map_err(internal)?;
    let inserted=sqlx::query("INSERT INTO github_webhook_deliveries(delivery,payload_digest,event)
        SELECT ?,?,? WHERE EXISTS(SELECT 1 FROM github_webhook_config WHERE singleton=1 AND enabled=1)
        ON CONFLICT DO NOTHING")
        .bind(&delivery).bind(&digest).bind(event).execute(&mut *tx).await.map_err(internal)?;
    if inserted.rows_affected() == 1 {
        let repeated:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM github_webhook_deliveries WHERE payload_digest=? AND delivery<>?)")
            .bind(&digest).bind(&delivery).fetch_one(&mut *tx).await.map_err(internal)?;
        if !repeated
            && let (Some(id), Some(name)) = (
                payload["repository"]["id"].as_i64(),
                payload["repository"]["full_name"].as_str(),
            )
        {
            // Header is unsigned. All accepted hints use the same canonical read operation.
            let approved: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM github_webhook_sources s JOIN github_repositories r ON r.github_id=s.github_id AND r.owner=s.owner COLLATE NOCASE AND r.name=s.repo COLLATE NOCASE WHERE s.github_id=? AND lower(s.owner || '/' || s.repo)=lower(?))").bind(id).bind(name).fetch_one(&mut *tx).await.map_err(internal)?;
            let queued = sqlx::query("INSERT INTO github_webhook_jobs(delivery,github_id,owner,repo) SELECT ?,s.github_id,s.owner,s.repo FROM github_webhook_sources s JOIN github_repositories r ON r.github_id=s.github_id AND r.owner=s.owner COLLATE NOCASE AND r.name=s.repo COLLATE NOCASE WHERE s.github_id=? AND lower(s.owner || '/' || s.repo)=lower(?) AND (SELECT count(*) FROM github_webhook_jobs WHERE state IN ('queued','running'))<1000")
                    .bind(&delivery).bind(id).bind(name).execute(&mut *tx).await.map_err(internal)?;
            if approved && queued.rows_affected() == 0 {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Webhook queue full; retry delivery",
                ));
            }
        }
        tx.commit().await.map_err(internal)?;
        return Ok((
            if repeated {
                StatusCode::OK
            } else {
                StatusCode::ACCEPTED
            },
            Json(Receipt {
                accepted: true,
                duplicate: repeated,
            }),
        ));
    }
    let active: bool =
        sqlx::query_scalar("SELECT enabled FROM github_webhook_config WHERE singleton=1")
            .fetch_one(&mut *tx)
            .await
            .map_err(internal)?;
    if !active {
        return Err(DISABLED);
    }
    let previous: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT payload_digest FROM github_webhook_deliveries WHERE delivery=?")
            .bind(&delivery)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal)?;
    if previous.is_some_and(|value| value != digest) {
        return Err((
            StatusCode::CONFLICT,
            "Delivery identifier already used for another payload",
        ));
    }
    tx.commit().await.map_err(internal)?;
    Ok((
        StatusCode::OK,
        Json(Receipt {
            accepted: true,
            duplicate: true,
        }),
    ))
}

#[derive(Deserialize)]
struct JobPage {
    #[serde(default)]
    offset: u32,
}
#[derive(Serialize, sqlx::FromRow)]
struct Job {
    id: i64,
    delivery: String,
    github_id: i64,
    owner: String,
    repo: String,
    state: String,
    attempts: i64,
    outcome: Option<String>,
}
async fn jobs(
    State(state): State<WebhookState>,
    Extension(user): Extension<User>,
    Query(page): Query<JobPage>,
) -> Result<Json<Vec<Job>>, Error> {
    admin(&user)?;
    if page.offset > 10000 {
        return Err(INVALID);
    }
    Ok(Json(sqlx::query_as("SELECT id,delivery,github_id,owner,repo,state,attempts,outcome FROM github_webhook_jobs ORDER BY id DESC LIMIT 20 OFFSET ?")
        .bind(page.offset).fetch_all(&state.pool).await.map_err(internal)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogTarget {
    namespace: String,
    name: String,
    owner: String,
}
async fn approve_catalog(
    State(state): State<WebhookState>,
    Extension(user): Extension<User>,
    Path(id): Path<i64>,
    Json(target): Json<CatalogTarget>,
) -> Result<StatusCode, Error> {
    admin(&user)?;
    if target.namespace.len() > 63 || target.name.len() > 63 || target.owner.len() > 100 {
        return Err(INVALID);
    }
    let mut tx = state.pool.begin().await.map_err(internal)?;
    // Only explicitly approved existing entities can be updated; no implicit adoption or transfer.
    let changed = sqlx::query("INSERT INTO github_webhook_catalog_targets(github_id,namespace,name,owner,approved_by) SELECT s.github_id,c.namespace,c.name,c.owner,? FROM github_webhook_sources s JOIN components c ON c.namespace=? AND c.name=? AND c.owner=? WHERE s.github_id=? ON CONFLICT DO NOTHING")
        .bind(user.id).bind(target.namespace).bind(target.name).bind(target.owner).bind(id).execute(&mut *tx).await.map_err(internal)?;
    if changed.rows_affected() != 1 {
        return Err((
            StatusCode::CONFLICT,
            "Catalog target missing or already bound",
        ));
    }
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
        .bind(user.id)
        .bind(format!("approve_webhook_catalog:{id}"))
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn approve_source(
    State(state): State<WebhookState>,
    Extension(user): Extension<User>,
    Path(id): Path<i64>,
) -> Result<StatusCode, Error> {
    admin(&user)?;
    let mut tx = state.pool.begin().await.map_err(internal)?;
    let result = sqlx::query("INSERT INTO github_webhook_sources(github_id,owner,repo,approved_by) SELECT github_id,owner,name,? FROM github_repositories WHERE github_id=? ON CONFLICT(github_id) DO NOTHING")
        .bind(user.id).bind(id).execute(&mut *tx).await.map_err(internal)?;
    if result.rows_affected() == 0 {
        return Err((StatusCode::CONFLICT, "Source absent or already approved"));
    }
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
        .bind(user.id)
        .bind(format!("approve_webhook_source:{id}"))
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn revoke_source(
    State(state): State<WebhookState>,
    Extension(user): Extension<User>,
    Path(id): Path<i64>,
) -> Result<StatusCode, Error> {
    admin(&user)?;
    let mut tx = state.pool.begin().await.map_err(internal)?;
    sqlx::query("DELETE FROM github_webhook_sources WHERE github_id=?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    sqlx::query("UPDATE github_webhook_jobs SET state='conflict',outcome='source_revoked',finished_at=unixepoch(),lease_until=NULL WHERE github_id=? AND state IN ('queued','running')").bind(id).execute(&mut *tx).await.map_err(internal)?;
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(?,?)")
        .bind(user.id)
        .bind(format!("revoke_webhook_source:{id}"))
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) fn start_worker(
    pool: SqlitePool,
    metrics: Arc<crate::observability::Metrics>,
) -> Result<tokio::task::JoinHandle<()>, reqwest::Error> {
    let client = crate::github::GitHubClient::new()?;
    Ok(tokio::spawn(async move {
        loop {
            if process_one(&pool, &client, Some(&metrics)).await.is_err() {
                tracing::warn!("Webhook refresh processing failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }))
}
type Refresh = Result<(Option<String>, String, bool, String), Error>;
type DescriptorResult = Result<crate::catalog::Component, Error>;
trait Upstream: Send + Sync {
    async fn metadata(&self, owner: &str, repo: &str, id: i64) -> Refresh;
    async fn descriptor(&self, owner: &str, repo: &str, branch: &str) -> DescriptorResult;
}
impl Upstream for crate::github::GitHubClient {
    async fn metadata(&self, owner: &str, repo: &str, id: i64) -> Refresh {
        self.refresh_registered(owner, repo, id).await
    }
    async fn descriptor(&self, owner: &str, repo: &str, branch: &str) -> DescriptorResult {
        self.canonical_descriptor(owner, repo, branch).await
    }
}
async fn process_one(
    pool: &SqlitePool,
    client: &impl Upstream,
    metrics: Option<&Arc<crate::observability::Metrics>>,
) -> Result<(), sqlx::Error> {
    // A generation (attempt number) fences expired workers. No payload data is applied.
    sqlx::query("UPDATE github_webhook_jobs SET state='failed',outcome='lease_exhausted',finished_at=unixepoch(),lease_until=NULL WHERE state='running' AND lease_until<=unixepoch() AND attempts>=5").execute(pool).await?;
    let job: Option<Job> = sqlx::query_as("UPDATE github_webhook_jobs SET state='running',attempts=attempts+1,lease_until=unixepoch()+90 WHERE id=(SELECT j.id FROM github_webhook_jobs j JOIN github_webhook_sources s ON s.github_id=j.github_id AND s.owner=j.owner AND s.repo=j.repo WHERE j.attempts<5 AND ((j.state='queued' AND j.available_at<=unixepoch()) OR (j.state='running' AND j.lease_until<=unixepoch())) AND EXISTS(SELECT 1 FROM github_webhook_config WHERE enabled=1) AND NOT EXISTS(SELECT 1 FROM github_webhook_jobs earlier WHERE earlier.github_id=j.github_id AND earlier.id<j.id AND earlier.state IN ('queued','running')) ORDER BY j.id LIMIT 1) RETURNING id,delivery,github_id,owner,repo,state,attempts,outcome").fetch_optional(pool).await?;
    let Some(job) = job else {
        return Ok(());
    };
    use crate::observability::{Operation, OperationOutcome};
    let guard = metrics.map(|m| m.operation(Operation::Webhook, None));
    // Capture local revisions before upstream I/O, including the approved target.
    let revision: Option<i64> =
        sqlx::query_scalar("SELECT sync_revision FROM github_repositories WHERE github_id=?")
            .bind(job.github_id)
            .fetch_optional(pool)
            .await?;
    let target: Option<(String,String,String,Option<i64>)> = sqlx::query_as("SELECT t.namespace,t.name,t.owner,c.sync_revision FROM github_webhook_catalog_targets t LEFT JOIN components c ON c.namespace=t.namespace AND c.name=t.name AND c.owner=t.owner WHERE t.github_id=?").bind(job.github_id).fetch_optional(pool).await?;
    let result = client.metadata(&job.owner, &job.repo, job.github_id).await;
    let catalog = match (&result, &target) {
        (Ok((_, branch, false, status)), Some(_)) if status == "found" => {
            Some(client.descriptor(&job.owner, &job.repo, branch).await)
        }
        _ => None,
    };
    let mut tx = pool.begin().await?;
    let valid = sqlx::query("UPDATE github_webhook_jobs SET lease_until=lease_until WHERE id=? AND state='running' AND attempts=? AND lease_until>unixepoch() AND EXISTS(SELECT 1 FROM github_webhook_config WHERE enabled=1) AND EXISTS(SELECT 1 FROM github_webhook_sources s JOIN github_repositories r ON r.github_id=s.github_id AND r.owner=s.owner COLLATE NOCASE AND r.name=s.repo COLLATE NOCASE WHERE s.github_id=github_webhook_jobs.github_id AND s.owner=github_webhook_jobs.owner AND s.repo=github_webhook_jobs.repo)").bind(job.id).bind(job.attempts).execute(&mut *tx).await?;
    if valid.rows_affected() == 0 {
        if let Some(g) = guard {
            g.finish(OperationOutcome::Rejected);
        }
        return Ok(());
    }
    let current_revision: Option<i64> =
        sqlx::query_scalar("SELECT sync_revision FROM github_repositories WHERE github_id=?")
            .bind(job.github_id)
            .fetch_optional(&mut *tx)
            .await?;
    let current_target: Option<(String,String,String,Option<i64>)> = sqlx::query_as("SELECT t.namespace,t.name,t.owner,c.sync_revision FROM github_webhook_catalog_targets t LEFT JOIN components c ON c.namespace=t.namespace AND c.name=t.name AND c.owner=t.owner WHERE t.github_id=?").bind(job.github_id).fetch_optional(&mut *tx).await?;
    let (state, outcome, delay) = if revision.is_none()
        || revision != current_revision
        || target != current_target
    {
        ("conflict", "local_revision_changed", 0)
    } else {
        match result {
            Ok((description, branch, archived, descriptor)) => {
                let disposition = if let Some((namespace, name, owner, target_revision)) = target {
                    match catalog {
                        Some(Ok(component))
                            if target_revision.is_some()
                                && component.namespace == namespace
                                && component.name == name
                                && component.owner == owner =>
                        {
                            let updated=sqlx::query("UPDATE components SET description=?,component_type=?,lifecycle=?,descriptor=? WHERE namespace=? AND name=? AND owner=? AND EXISTS(SELECT 1 FROM github_webhook_catalog_targets WHERE github_id=? AND namespace=? AND name=? AND owner=?)")
                            .bind(component.description).bind(component.component_type).bind(component.lifecycle).bind(component.descriptor.map(sqlx::types::Json)).bind(&namespace).bind(&name).bind(&owner).bind(job.github_id).bind(&namespace).bind(&name).bind(&owner).execute(&mut *tx).await?;
                            if updated.rows_affected() == 1 {
                                ("succeeded", "canonical_catalog_refreshed", 0)
                            } else {
                                ("conflict", "catalog_binding_changed", 0)
                            }
                        }
                        Some(Ok(_)) => ("conflict", "catalog_identity_or_owner_changed", 0),
                        Some(Err((code, _)))
                            if code == StatusCode::TOO_MANY_REQUESTS && job.attempts < 5 =>
                        {
                            ("queued", "catalog_rate_limit", 900)
                        }
                        Some(Err((code, _))) if code.is_server_error() && job.attempts < 5 => {
                            ("queued", "catalog_retry", 15_i64 << job.attempts)
                        }
                        Some(Err((code, _)))
                            if code.is_server_error() || code == StatusCode::TOO_MANY_REQUESTS =>
                        {
                            ("failed", "catalog_retry_exhausted", 0)
                        }
                        _ => ("unsupported", "canonical_descriptor_unavailable", 0),
                    }
                } else {
                    ("succeeded", "canonical_metadata_refreshed", 0)
                };
                if disposition.0 == "succeeded" {
                    sqlx::query("UPDATE github_repositories SET description=?,default_branch=?,archived=?,descriptor_status=?,synced_at=unixepoch() WHERE github_id=? AND owner=? COLLATE NOCASE AND name=? COLLATE NOCASE")
                .bind(description).bind(branch).bind(archived).bind(descriptor).bind(job.github_id).bind(&job.owner).bind(&job.repo).execute(&mut *tx).await?;
                }
                disposition
            }
            Err((code, _))
                if code == StatusCode::CONFLICT
                    || code == StatusCode::FORBIDDEN
                    || code == StatusCode::NOT_FOUND =>
            {
                ("conflict", "source_identity_unavailable", 0)
            }
            Err((code, _)) if job.attempts < 5 => (
                "queued",
                "upstream_retry",
                if code == StatusCode::TOO_MANY_REQUESTS {
                    900
                } else {
                    15_i64 << job.attempts
                },
            ),
            Err(_) => ("failed", "retry_exhausted", 0),
        }
    };
    sqlx::query("UPDATE github_webhook_jobs SET state=?,outcome=?,lease_until=NULL,available_at=unixepoch()+?,finished_at=CASE WHEN ?='queued' THEN NULL ELSE unixepoch() END WHERE id=? AND attempts=?")
        .bind(state).bind(outcome).bind(delay).bind(state).bind(job.id).bind(job.attempts).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO identity_audit_events(actor_id,action) VALUES(NULL,?)")
        .bind(format!("webhook_job:{}:{outcome}", job.id))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if let Some(g) = guard {
        g.finish(match state {
            "succeeded" => OperationOutcome::Success,
            "conflict" | "unsupported" => OperationOutcome::Rejected,
            _ => OperationOutcome::Failure,
        });
    }
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
    use serde_json::{Value, json};
    use tower::ServiceExt;

    const ID1: &str = "11111111-2222-3333-4444-555555555555";
    const ID2: &str = "11111111-2222-3333-4444-666666666666";
    const ID3: &str = "11111111-2222-3333-4444-777777777777";

    fn sign(secret: &[u8], body: &[u8]) -> String {
        let mut mac = Signature::new_from_slice(secret).unwrap();
        mac.update(body);
        format!("sha256={:x}", mac.finalize().into_bytes())
    }
    #[test]
    fn github_documented_signature_vector() {
        let signature = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert!(verify(b"It's a Secret to Everybody", signature, b"Hello, World!").is_ok());
        assert!(verify(b"It's a Secret to Everybody", signature, b"Hello, World?").is_err());
        assert!(
            verify(
                b"It's a Secret to Everybody",
                "sha1=abcdef",
                b"Hello, World!"
            )
            .is_err()
        );
    }
    async fn admin_call(
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
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    async fn send(app: &Router, id: &str, body: &[u8], signature: &str) -> StatusCode {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/webhooks/github")
                    .header("content-type", "application/json")
                    .header("x-github-event", "push")
                    .header("x-github-delivery", id)
                    .header("x-hub-signature-256", signature)
                    .body(Body::from(body.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }
    #[tokio::test]
    async fn authorization_signature_replay_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("webhooks.db");
        let pool = crate::database::open(&path).await.unwrap();
        let identity = IdentityStore::new(pool.clone());
        let mut random = [0u8; 32];
        OsRng.fill_bytes(&mut random);
        let password: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let administrator = identity.bootstrap_admin("admin", &password).await.unwrap();
        identity
            .create_user(administrator.id, "member", &password, Role::Member)
            .await
            .unwrap();
        identity
            .create_user(administrator.id, "viewer", &password, Role::Viewer)
            .await
            .unwrap();
        let a = identity.login("admin", &password).await.unwrap();
        let m = identity.login("member", &password).await.unwrap();
        let v = identity.login("viewer", &password).await.unwrap();
        OsRng.fill_bytes(&mut random);
        let secret = Arc::<[u8]>::from(random.to_vec());
        let state = WebhookState {
            pool: pool.clone(),
            secret: Some(secret.clone()),
        };
        let (management, receiver) = build_routes(state.clone());
        let app = auth::protect(management, AuthState::new(identity.clone(), 3000)).merge(receiver);
        let config_path = "/api/webhooks/github/config";
        let status_path = "/api/webhooks/github/status";
        let body = br#"{"synthetic":true}"#;
        let signed = sign(&secret, body);
        assert_eq!(
            send(&app, ID1, body, &signed).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            admin_call(&app, "GET", config_path, "", Value::Null)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        for token in [m.as_str(), v.as_str()] {
            assert_eq!(
                admin_call(&app, "GET", status_path, token, Value::Null)
                    .await
                    .0,
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                admin_call(&app, "PUT", config_path, token, json!({"enabled":true}))
                    .await
                    .0,
                StatusCode::FORBIDDEN
            );
        }
        let (no_secret, _) = build_routes(WebhookState {
            pool: pool.clone(),
            secret: None,
        });
        let no_secret = auth::protect(no_secret, AuthState::new(identity, 3000));
        assert_eq!(
            admin_call(
                &no_secret,
                "PUT",
                config_path,
                a.as_str(),
                json!({"enabled":true})
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        drop(no_secret);
        assert_eq!(
            admin_call(
                &app,
                "PUT",
                config_path,
                a.as_str(),
                json!({"enabled":true})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            send(&app, ID1, body, "sha256=bad").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            send(&app, ID1, b"malformed", &sign(&secret, b"malformed")).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            send(&app, ID1, &vec![b'x'; MAX_BODY + 1], &signed).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(send(&app, ID1, body, &signed).await, StatusCode::ACCEPTED);
        assert_eq!(send(&app, ID1, body, &signed).await, StatusCode::OK);
        assert_eq!(send(&app, ID2, body, &signed).await, StatusCode::OK);
        let second = br#"{"synthetic":false}"#;
        assert_eq!(
            send(&app, ID1, second, &sign(&secret, second)).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            send(&app, ID2, second, &sign(&secret, second)).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            send(&app, ID3, body, "sha256=bad").await,
            StatusCode::UNAUTHORIZED
        );
        let (_, summary) = admin_call(&app, "GET", status_path, a.as_str(), Value::Null).await;
        assert_eq!(summary["accepted_deliveries"], 1);
        assert!(summary.get("secret").is_none());
        drop(app);
        drop(state);
        pool.close().await;
        let pool = crate::database::open(&path).await.unwrap();
        let (management, receiver) = build_routes(WebhookState {
            pool: pool.clone(),
            secret: Some(secret),
        });
        let app = auth::protect(management, AuthState::new(IdentityStore::new(pool), 3000))
            .merge(receiver);
        assert_eq!(send(&app, ID1, body, &signed).await, StatusCode::OK);
        assert_eq!(
            admin_call(
                &app,
                "PUT",
                config_path,
                a.as_str(),
                json!({"enabled":false})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            send(&app, ID3, body, &signed).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    struct SyntheticUpstream {
        pool: SqlitePool,
        scenario: &'static str,
    }
    impl Upstream for SyntheticUpstream {
        async fn metadata(&self, _owner: &str, _repo: &str, _id: i64) -> Refresh {
            match self.scenario {
                "manual" => {
                    sqlx::query("UPDATE components SET description='manual' WHERE name='service'")
                        .execute(&self.pool)
                        .await
                        .unwrap();
                }
                "delete" => {
                    sqlx::query("DELETE FROM components WHERE name='service'")
                        .execute(&self.pool)
                        .await
                        .unwrap();
                    sqlx::query("INSERT INTO components(name,namespace,description,owner) VALUES('service','default','replacement','team')").execute(&self.pool).await.unwrap();
                }
                "revoke" => {
                    sqlx::query("DELETE FROM github_webhook_catalog_targets WHERE github_id=123")
                        .execute(&self.pool)
                        .await
                        .unwrap();
                }
                "registry" => {
                    sqlx::query(
                        "UPDATE github_repositories SET description='manual' WHERE github_id=123",
                    )
                    .execute(&self.pool)
                    .await
                    .unwrap();
                }
                "rate" => return Err((StatusCode::TOO_MANY_REQUESTS, "synthetic rate limit")),
                _ => {}
            }
            Ok((
                Some("upstream".into()),
                "main".into(),
                false,
                "found".into(),
            ))
        }
        async fn descriptor(&self, _owner: &str, _repo: &str, _branch: &str) -> DescriptorResult {
            match self.scenario {
                "descriptor_rate" => {
                    return Err((
                        StatusCode::TOO_MANY_REQUESTS,
                        "synthetic descriptor rate limit",
                    ));
                }
                "descriptor_invalid" => {
                    return Err((
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "synthetic invalid descriptor",
                    ));
                }
                "descriptor_error" => {
                    return Err((StatusCode::BAD_GATEWAY, "synthetic upstream error"));
                }
                _ => {}
            }
            crate::catalog::validate_descriptor(json!({
                "apiVersion":"backstage.io/v1alpha1","kind":"Component",
                "metadata":{"name":"service","description":"canonical"},
                "spec":{"type":"service","lifecycle":"production","owner":if self.scenario=="owner" {"other"}else{"team"},"dependsOn":["component:default/dependency"]}
            }))
        }
    }
    #[tokio::test]
    async fn canonical_worker_persists_and_fences_local_changes() {
        for scenario in [
            "success",
            "manual",
            "delete",
            "revoke",
            "registry",
            "owner",
            "rate",
            "restart",
            "descriptor_rate",
            "descriptor_invalid",
            "descriptor_error",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("fixture.db");
            let pool = crate::database::open(&path).await.unwrap();
            let identity = IdentityStore::new(pool.clone());
            let admin = identity
                .bootstrap_admin("admin", "synthetic-long-password-only")
                .await
                .unwrap();
            sqlx::query("INSERT INTO components(name,namespace,description,owner) VALUES('service','default','original','team')").execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO github_repositories(github_id,owner,name,default_branch,archived,descriptor_status,synced_by) VALUES(123,'fixture','service','main',0,'found',?)").bind(admin.id).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO github_webhook_sources(github_id,owner,repo,approved_by) VALUES(123,'fixture','service',?)").bind(admin.id).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO github_webhook_catalog_targets(github_id,namespace,name,owner,approved_by) VALUES(123,'default','service','team',?)").bind(admin.id).execute(&pool).await.unwrap();
            sqlx::query("UPDATE github_webhook_config SET enabled=1")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO github_webhook_deliveries(delivery,payload_digest,event) VALUES(?,?,'push')").bind(ID1).bind(vec![0u8;32]).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO github_webhook_jobs(delivery,github_id,owner,repo) VALUES(?,123,'fixture','service')").bind(ID1).execute(&pool).await.unwrap();
            if scenario == "restart" {
                sqlx::query("UPDATE github_webhook_jobs SET state='running',attempts=1,lease_until=unixepoch()-1").execute(&pool).await.unwrap();
            }
            drop(identity);
            pool.close().await;
            let pool = crate::database::open(&path).await.unwrap();
            process_one(
                &pool,
                &SyntheticUpstream {
                    pool: pool.clone(),
                    scenario,
                },
                None,
            )
            .await
            .unwrap();
            let (state, attempts): (String, i64) =
                sqlx::query_as("SELECT state,attempts FROM github_webhook_jobs")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let (description,owner,lifecycle,descriptor):(String,String,Option<String>,Option<String>)=sqlx::query_as("SELECT description,owner,lifecycle,descriptor FROM components WHERE name='service'").fetch_one(&pool).await.unwrap();
            assert_eq!(owner, "team", "{scenario}");
            assert_eq!(attempts, if scenario == "restart" { 2 } else { 1 });
            match scenario {
                "success" | "restart" => {
                    assert_eq!(state, "succeeded");
                    assert_eq!(description, "canonical");
                    assert_eq!(lifecycle.as_deref(), Some("production"));
                    assert!(descriptor.unwrap().contains("component:default/dependency"));
                }
                "rate" | "descriptor_rate" | "descriptor_error" => {
                    assert_eq!(state, "queued");
                    assert_eq!(description, "original");
                }
                "descriptor_invalid" => {
                    assert_eq!(state, "unsupported");
                    assert_eq!(description, "original");
                }
                _ => {
                    assert_eq!(state, "conflict", "{scenario}");
                    assert_ne!(description, "canonical", "{scenario}");
                }
            }
            let repository_description: Option<String> = sqlx::query_scalar(
                "SELECT description FROM github_repositories WHERE github_id=123",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(
                repository_description.as_deref(),
                match scenario {
                    "success" | "restart" => Some("upstream"),
                    "registry" => Some("manual"),
                    _ => None,
                },
                "{scenario}: rejected refresh must not modify repository metadata"
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT synced_by FROM github_repositories WHERE github_id=123"
                )
                .fetch_one(&pool)
                .await
                .unwrap(),
                admin.id
            );
            pool.close().await;
        }
    }
    #[tokio::test]
    async fn canonical_http_fixture_retrieval() {
        // The only alternate upstream origin is compiled into unit tests.
        use axum::routing::get;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let yaml=b"apiVersion: backstage.io/v1alpha1\nkind: Component\nmetadata:\n  name: service\n  description: retrieved\nspec:\n  type: service\n  lifecycle: production\n  owner: team\n";
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut encoded = String::new();
        for chunk in yaml.chunks(3) {
            let a = chunk[0];
            let b = *chunk.get(1).unwrap_or(&0);
            let c = *chunk.get(2).unwrap_or(&0);
            encoded.push(alphabet[(a >> 2) as usize] as char);
            encoded.push(alphabet[(((a & 3) << 4) | (b >> 4)) as usize] as char);
            encoded.push(if chunk.len() > 1 {
                alphabet[(((b & 15) << 2) | (c >> 6)) as usize] as char
            } else {
                '='
            });
            encoded.push(if chunk.len() > 2 {
                alphabet[(c & 63) as usize] as char
            } else {
                '='
            });
        }
        let router=Router::new()
            .route("/repos/fixture/service",get(||async {Json(json!({"id":123,"name":"service","owner":{"login":"fixture"},"description":"fixture","default_branch":"main","archived":false,"private":false}))}))
            .route("/repos/fixture/service/contents/catalog-info.yaml",get(move |Query(query):Query<std::collections::HashMap<String,String>>| {
                let encoded=encoded.clone();let count=count.clone();
                async move {
                    count.fetch_add(1,Ordering::SeqCst);
                    if let Some(branch)=query.get("ref"){assert_eq!(branch,"main");}
                    Json(json!({"type":"file","path":"catalog-info.yaml","size":yaml.len(),"encoding":"base64","content":encoded}))
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = crate::github::GitHubClient::loopback_fixture(port);
        let metadata = client
            .refresh_registered("fixture", "service", 123)
            .await
            .unwrap();
        assert_eq!(metadata.3, "found");
        let descriptor = client
            .canonical_descriptor("fixture", "service", &metadata.1)
            .await
            .unwrap();
        assert_eq!(descriptor.name, "service");
        assert_eq!(descriptor.owner, "team");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
        let _ = server.await;
    }
}
