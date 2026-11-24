//! Passive GitHub webhook receipt ledger. No payload retention or job execution.
use crate::identity::{Role, User};
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
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
        .with_state(state.clone());
    let receiver = Router::new()
        .route(
            "/api/webhooks/github",
            post(receive).layer(DefaultBodyLimit::max(MAX_BODY)),
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
    if !serde_json::from_slice::<serde_json::Value>(&body).is_ok_and(|v| v.is_object()) {
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
}
